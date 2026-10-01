use std::collections::{BTreeMap, BTreeSet};
use syn::visit::{self, Visit};

pub type Edge = (String, String);

pub fn edges(
    module: &str,
    source: &str,
    modules: &BTreeSet<String>,
) -> Result<BTreeSet<Edge>, String> {
    let syntax = syn::parse_file(source).map_err(|e| format!("{module}: parse error: {e}"))?;
    let mut visitor = Dependencies {
        module: module.into(),
        modules,
        edges: BTreeSet::new(),
        errors: Vec::new(),
    };
    visitor.visit_file(&syntax);
    if visitor.errors.is_empty() {
        Ok(visitor.edges)
    } else {
        Err(visitor.errors.join("\n"))
    }
}

struct Dependencies<'a> {
    module: String,
    modules: &'a BTreeSet<String>,
    edges: BTreeSet<Edge>,
    errors: Vec<String>,
}

impl Dependencies<'_> {
    fn resolve(&mut self, parts: &[String]) {
        let Some(first) = parts.first() else {
            return;
        };
        if !matches!(first.as_str(), "crate" | "self" | "super") {
            self.resolve_local(parts);
            return;
        }
        let mut path = Vec::new();
        let mut index = 0;
        if first == "crate" {
            index = 1;
        } else {
            path = self.module.split("::").map(String::from).collect();
            while parts.get(index).is_some_and(|p| p == "super") {
                path.pop();
                index += 1;
            }
            if parts.get(index).is_some_and(|p| p == "self") {
                index += 1;
            }
        }
        let mut target = if self.modules.contains(&path.join("::")) {
            Some(path.join("::"))
        } else {
            None
        };
        for part in &parts[index..] {
            path.push(part.clone());
            let candidate = path.join("::");
            if self.modules.contains(&candidate) {
                target = Some(candidate);
            }
        }
        match target {
            Some(target) if target != self.module => {
                self.edges.insert((self.module.clone(), target));
            }
            Some(_) => {}
            None if parts.len() > index => self.errors.push(format!(
                "{}: unresolved internal module path {}",
                self.module,
                parts.join("::")
            )),
            None => {}
        }
    }
    fn resolve_local(&mut self, parts: &[String]) {
        let first = &parts[0];
        let candidates = [format!("{}::{first}", self.module), first.clone()];
        if let Some(target) = candidates.iter().find(|p| self.modules.contains(*p)) {
            let mut qualified = vec!["crate".into()];
            qualified.extend(target.split("::").map(String::from));
            qualified.extend_from_slice(&parts[1..]);
            self.resolve(&qualified);
        }
    }
    fn import_name(&mut self, name: &str, prefix: &mut Vec<String>) {
        prefix.push(name.into());
        self.resolve(prefix);
        prefix.pop();
    }
    fn imports(&mut self, tree: &syn::UseTree, prefix: &mut Vec<String>) {
        match tree {
            syn::UseTree::Path(p) => {
                prefix.push(p.ident.to_string());
                self.imports(&p.tree, prefix);
                prefix.pop();
            }
            syn::UseTree::Group(g) => {
                for item in &g.items {
                    self.imports(item, prefix);
                }
            }
            syn::UseTree::Name(n) => self.import_name(&n.ident.to_string(), prefix),
            syn::UseTree::Rename(n) => self.import_name(&n.ident.to_string(), prefix),
            syn::UseTree::Glob(_) => self.resolve(prefix),
        }
    }
}

impl<'ast> Visit<'ast> for Dependencies<'_> {
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if item.content.is_some() {
            let old = self.module.clone();
            self.module = format!("{}::{}", old, item.ident);
            visit::visit_item_mod(self, item);
            self.module = old;
        }
    }
    fn visit_path(&mut self, path: &'ast syn::Path) {
        if path.segments.len() < 2 {
            return;
        }
        self.resolve(
            &path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect::<Vec<_>>(),
        );
        visit::visit_path(self, path);
    }
    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        self.imports(&item.tree, &mut Vec::new());
    }
}

pub fn feedback_edges(edges: &BTreeSet<Edge>) -> BTreeSet<Edge> {
    let mut accepted: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut feedback = BTreeSet::new();
    for (from, to) in edges {
        if reaches(&accepted, to, from, &mut BTreeSet::new()) {
            feedback.insert((from.clone(), to.clone()));
        } else {
            accepted.entry(from).or_default().push(to);
        }
    }
    feedback
}

fn reaches<'a>(
    graph: &BTreeMap<&'a str, Vec<&'a str>>,
    from: &'a str,
    to: &str,
    visited: &mut BTreeSet<&'a str>,
) -> bool {
    if from == to {
        return true;
    }
    if !visited.insert(from) {
        return false;
    }
    graph
        .get(from)
        .is_some_and(|next| next.iter().any(|node| reaches(graph, node, to, visited)))
}
