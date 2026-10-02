//! Syntax-level module identities. Unsupported or conflicting internal bindings fail closed.
use std::collections::{BTreeMap, BTreeSet};
use syn::{Item, UseTree};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Binding {
    Module(String),
    Item(String),
    Import(usize, Vec<String>),
    External,
    Ambiguous,
}

#[derive(Default)]
struct Scope {
    names: BTreeMap<String, Binding>,
    globs: Vec<Vec<String>>,
    parent: Option<usize>,
    module: String,
}

#[derive(Default)]
pub(crate) struct Bindings {
    scopes: Vec<Scope>,
    modules: BTreeMap<String, usize>,
    strict: bool,
}

pub(crate) fn child(module: &str, name: &str) -> String {
    if module == "root" {
        name.into()
    } else {
        format!("{module}::{name}")
    }
}

pub(crate) fn imports(
    tree: &UseTree,
    prefix: &mut Vec<String>,
    out: &mut Vec<(String, Vec<String>)>,
) {
    match tree {
        UseTree::Path(p) => {
            prefix.push(p.ident.to_string());
            imports(&p.tree, prefix, out);
            prefix.pop();
        }
        UseTree::Group(g) => {
            for tree in &g.items {
                imports(tree, prefix, out);
            }
        }
        UseTree::Name(n) => leaf(&n.ident.to_string(), None, prefix, out),
        UseTree::Rename(n) => leaf(
            &n.ident.to_string(),
            Some(n.rename.to_string()),
            prefix,
            out,
        ),
        UseTree::Glob(_) => out.push(("*".into(), prefix.clone())),
    }
}

fn leaf(
    name: &str,
    alias: Option<String>,
    prefix: &[String],
    out: &mut Vec<(String, Vec<String>)>,
) {
    let mut path = prefix.to_vec();
    if name != "self" {
        path.push(name.into());
    }
    let name = alias.unwrap_or_else(|| path.last().cloned().unwrap_or_default());
    out.push((name, path));
}

fn item_name(item: &Item) -> Option<String> {
    let ident = match item {
        Item::Struct(i) => &i.ident,
        Item::Enum(i) => &i.ident,
        Item::Union(i) => &i.ident,
        Item::Trait(i) => &i.ident,
        Item::TraitAlias(i) => &i.ident,
        Item::Type(i) => &i.ident,
        Item::Fn(i) => &i.sig.ident,
        Item::Const(i) => &i.ident,
        Item::Static(i) => &i.ident,
        _ => return None,
    };
    Some(ident.to_string())
}

impl Bindings {
    pub(crate) fn new(modules: &BTreeSet<String>, strict: bool) -> Self {
        let mut bindings = Self {
            strict,
            ..Self::default()
        };
        for module in modules.iter().chain(std::iter::once(&"root".to_string())) {
            if !bindings.modules.contains_key(module) {
                let id = bindings.scopes.len();
                bindings.modules.insert(module.clone(), id);
                bindings.scopes.push(Scope {
                    module: module.clone(),
                    ..Scope::default()
                });
            }
        }
        for module in modules {
            if module == "root" {
                continue;
            }
            let (parent, name) = module.rsplit_once("::").unwrap_or(("root", module));
            if let Some(&scope) = bindings.modules.get(parent) {
                bindings.insert(scope, name.into(), Binding::Module(module.clone()));
            }
        }
        bindings
    }

    pub(crate) fn module(&self, name: &str) -> usize {
        self.modules[name]
    }

    fn insert(&mut self, scope: usize, name: String, binding: Binding) {
        let entry = self.scopes[scope]
            .names
            .entry(name)
            .or_insert(binding.clone());
        if *entry != binding {
            *entry = Binding::Ambiguous;
        }
    }

    pub(crate) fn add_items(&mut self, scope: usize, items: &[Item]) {
        for item in items {
            self.add_item(scope, item);
            if let Item::Mod(m) = item
                && let Some((_, items)) = &m.content
            {
                let name = child(&self.scopes[scope].module, &m.ident.to_string());
                if let Some(&id) = self.modules.get(&name) {
                    self.add_items(id, items);
                }
            }
        }
    }

    fn add_item(&mut self, scope: usize, item: &Item) {
        if let Some(name) = item_name(item) {
            self.insert(
                scope,
                name,
                Binding::Item(self.scopes[scope].module.clone()),
            );
        }
        match item {
            Item::Use(i) => {
                let mut entries = Vec::new();
                let mut prefix = if i.leading_colon.is_some() {
                    vec![String::new()]
                } else {
                    Vec::new()
                };
                imports(&i.tree, &mut prefix, &mut entries);
                for (name, parts) in entries {
                    if name == "*" {
                        self.scopes[scope].globs.push(parts);
                    } else {
                        self.insert(scope, name, Binding::Import(scope, parts));
                    }
                }
            }
            Item::ExternCrate(i) => {
                let name = i
                    .rename
                    .as_ref()
                    .map_or(&i.ident, |(_, name)| name)
                    .to_string();
                self.insert(scope, name, Binding::External);
            }
            _ => {}
        }
    }

    pub(crate) fn block(&mut self, parent: usize, block: &syn::Block) -> Result<usize, String> {
        let id = self.scopes.len();
        self.scopes.push(Scope {
            parent: Some(parent),
            module: self.scopes[parent].module.clone(),
            ..Scope::default()
        });
        for stmt in &block.stmts {
            if let syn::Stmt::Item(item) = stmt {
                if matches!(item, Item::Mod(_)) {
                    return Err("unsupported block-local module identity".into());
                }
                self.add_item(id, item);
            }
        }
        Ok(id)
    }

    pub(crate) fn resolve(
        &self,
        scope: usize,
        parts: &[String],
    ) -> Result<Option<Binding>, String> {
        self.resolve_inner(scope, parts, &mut BTreeSet::new())
    }

    fn resolve_inner(
        &self,
        scope: usize,
        parts: &[String],
        active: &mut BTreeSet<(usize, Vec<String>)>,
    ) -> Result<Option<Binding>, String> {
        let Some(first) = parts.first() else {
            return Ok(None);
        };
        let key = (scope, parts.to_vec());
        if !active.insert(key.clone()) {
            return Err(format!(
                "recursive/ambiguous module binding {}",
                parts.join("::")
            ));
        }
        let (binding, consumed) = match first.as_str() {
            "" => (Some(Binding::External), 1),
            "crate" => (Some(Binding::Module("root".into())), 1),
            "self" => (Some(Binding::Module(self.scopes[scope].module.clone())), 1),
            "super" => self.super_path(scope, parts)?,
            _ => (
                self.lexical(scope, first, active, parts.len() > 1 || !self.strict)?,
                1,
            ),
        };
        let result = self.traverse(binding, &parts[consumed..], active);
        active.remove(&key);
        result
    }

    fn super_path(
        &self,
        scope: usize,
        parts: &[String],
    ) -> Result<(Option<Binding>, usize), String> {
        let mut module = self.scopes[scope].module.clone();
        let count = parts.iter().take_while(|p| *p == "super").count();
        for _ in 0..count {
            if module == "root" {
                return Err("super path escapes crate root".into());
            }
            module = module.rsplit_once("::").map_or("root", |(p, _)| p).into();
        }
        Ok((Some(Binding::Module(module)), count))
    }

    fn lexical(
        &self,
        scope: usize,
        name: &str,
        active: &mut BTreeSet<(usize, Vec<String>)>,
        root_fallback: bool,
    ) -> Result<Option<Binding>, String> {
        let mut current = Some(scope);
        while let Some(id) = current {
            if let Some(binding) = self.lookup(id, name, active)? {
                return Ok(Some(binding));
            }
            current = self.scopes[id].parent;
        }
        // Preserve bare crate-root module paths used by the standalone graph API.
        if root_fallback && self.modules.contains_key(name) {
            return Ok(Some(Binding::Module(name.into())));
        }
        Ok(None)
    }

    fn expand(
        &self,
        binding: Binding,
        active: &mut BTreeSet<(usize, Vec<String>)>,
    ) -> Result<Option<Binding>, String> {
        match binding {
            Binding::Import(id, parts) => self
                .resolve_inner(id, &parts, active)
                .map(|b| Some(b.unwrap_or(Binding::External))),
            Binding::Ambiguous => {
                Err("ambiguous module binding across declarations/cfg alternatives".into())
            }
            binding => Ok(Some(binding)),
        }
    }

    fn lookup(
        &self,
        scope: usize,
        name: &str,
        active: &mut BTreeSet<(usize, Vec<String>)>,
    ) -> Result<Option<Binding>, String> {
        if let Some(binding) = self.scopes[scope].names.get(name) {
            return self.expand(binding.clone(), active);
        }
        let mut found = None;
        for glob in &self.scopes[scope].globs {
            let key = (scope, vec![format!("*::{name}")]);
            if !active.insert(key.clone()) {
                return Err("recursive/ambiguous module glob binding".into());
            }
            let target = self.resolve_inner(scope, glob, active)?;
            let candidate = if let Some(Binding::Module(module)) = target {
                self.lookup(self.module(&module), name, active)?
            } else {
                None
            };
            active.remove(&key);
            if let Some(candidate) = candidate {
                if found.as_ref().is_some_and(|old| *old != candidate) {
                    return Err(format!("ambiguous module glob binding {name}"));
                }
                found = Some(candidate);
            }
        }
        Ok(found)
    }

    fn traverse(
        &self,
        mut binding: Option<Binding>,
        parts: &[String],
        active: &mut BTreeSet<(usize, Vec<String>)>,
    ) -> Result<Option<Binding>, String> {
        for (index, part) in parts.iter().enumerate() {
            let Some(Binding::Module(module)) = &binding else {
                return Ok(binding);
            };
            let scope = self.module(module);
            binding = match self.lookup(scope, part, active)? {
                Some(binding) => Some(binding),
                None if !self.strict && module != "root" && index + 1 == parts.len() => {
                    Some(Binding::Item(module.clone()))
                }
                None => {
                    return Err(format!(
                        "unresolved internal module destination {module}::{part}"
                    ));
                }
            };
        }
        Ok(binding)
    }
}
