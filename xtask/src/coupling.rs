use crate::module_bindings::{self, Binding, Bindings};
use std::collections::{BTreeMap, BTreeSet};
use syn::visit::{self, Visit};

pub type Edge = (String, String);

pub fn edges(
    module: &str,
    source: &str,
    modules: &BTreeSet<String>,
) -> Result<BTreeSet<Edge>, String> {
    let syntax = syn::parse_file(source).map_err(|e| format!("{module}: parse error: {e}"))?;
    let mut names = modules.clone();
    names.insert(module.into());
    let mut bindings = Bindings::new(&names, false);
    bindings.add_items(bindings.module(module), &syntax.items);
    edges_in(module, source, &mut bindings, module)
}

pub(crate) fn edges_in(
    module: &str,
    source: &str,
    bindings: &mut Bindings,
    path: &str,
) -> Result<BTreeSet<Edge>, String> {
    let syntax = syn::parse_file(source).map_err(|e| format!("{module}: parse error: {e}"))?;
    crate::macro_policy::validate_bindings(path, &syntax)?;
    let scope = bindings.module(module);
    let mut visitor = Dependencies {
        module: module.into(),
        path: path.into(),
        scope,
        bindings,
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
    path: String,
    scope: usize,
    bindings: &'a mut Bindings,
    edges: BTreeSet<Edge>,
    errors: Vec<String>,
}

impl Dependencies<'_> {
    fn resolve(&mut self, parts: &[String]) {
        match self.bindings.resolve(self.scope, parts) {
            Ok(Some(Binding::Module(target) | Binding::Item(target))) if target != self.module => {
                self.edges.insert((self.module.clone(), target));
            }
            Err(error) => self.errors.push(format!(
                "{}: {}: {error} ({})",
                self.path,
                self.module,
                parts.join("::")
            )),
            _ => {}
        }
    }
}

impl<'ast> Visit<'ast> for Dependencies<'_> {
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        self.visit_path(&mac.path);
        let path = self.path.clone();
        if let Err(error) = crate::macro_policy::visit_inputs(self, mac, &path) {
            self.errors.push(error);
        }
    }
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if item.content.is_some() {
            let old_module = self.module.clone();
            let old_scope = self.scope;
            self.module = module_bindings::child(&old_module, &item.ident.to_string());
            self.scope = self.bindings.module(&self.module);
            visit::visit_item_mod(self, item);
            self.module = old_module;
            self.scope = old_scope;
        }
    }
    fn visit_block(&mut self, block: &'ast syn::Block) {
        let old = self.scope;
        match self.bindings.block(old, block) {
            Ok(scope) => {
                self.scope = scope;
                visit::visit_block(self, block);
                self.scope = old;
            }
            Err(error) => self
                .errors
                .push(format!("{}: {}: {error}", self.path, self.module)),
        }
    }
    fn visit_generics(&mut self, generics: &'ast syn::Generics) {
        for param in &generics.params {
            if let syn::GenericParam::Type(ty) = param {
                let name = ty.ident.to_string();
                match self
                    .bindings
                    .resolve(self.scope, std::slice::from_ref(&name))
                {
                    Ok(Some(Binding::Module(_))) => self.errors.push(format!(
                        "{}: unsupported generic/module binding shadow {name}",
                        self.path
                    )),
                    Ok(Some(Binding::Item(owner))) if owner != self.module => {
                        self.errors.push(format!(
                            "{}: unsupported generic/import binding shadow {name}",
                            self.path
                        ))
                    }
                    Err(error) => self.errors.push(format!("{}: {error}", self.path)),
                    _ => {}
                }
            }
        }
        visit::visit_generics(self, generics);
    }
    fn visit_path(&mut self, path: &'ast syn::Path) {
        let mut parts = Vec::new();
        if path.leading_colon.is_some() {
            parts.push(String::new());
        }
        parts.extend(path.segments.iter().map(|s| s.ident.to_string()));
        self.resolve(&parts);
        visit::visit_path(self, path);
    }
    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        let mut entries = Vec::new();
        let mut prefix = if item.leading_colon.is_some() {
            vec![String::new()]
        } else {
            Vec::new()
        };
        module_bindings::imports(&item.tree, &mut prefix, &mut entries);
        for (_, parts) in entries {
            self.resolve(&parts);
        }
    }
}

/// Every edge whose target can reach its source belongs to a directed cycle.
pub fn cyclic_edges(edges: &BTreeSet<Edge>) -> BTreeSet<Edge> {
    let mut graph: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (from, to) in edges {
        graph.entry(from).or_default().push(to);
    }
    edges
        .iter()
        .filter(|(from, to)| reaches(&graph, to, from, &mut BTreeSet::new()))
        .cloned()
        .collect()
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
