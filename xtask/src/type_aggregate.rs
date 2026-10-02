//! Resolve lexical type identities before aggregating responsibilities across files.
use crate::{metrics::effective_lines, modules::Module};
use std::collections::{BTreeMap, BTreeSet};
use syn::{
    Item, UseTree,
    visit::{self, Visit},
};

type Totals = BTreeMap<String, (usize, usize)>;
type Scope = BTreeMap<String, Binding>;

#[derive(Clone)]
enum Binding {
    Declaration(String),
    Alias(String),
    Unsupported,
}

pub fn aggregate(modules: &[Module<'_>], prefix: &str) -> Result<Totals, String> {
    let mut globals = BTreeMap::new();
    for module in modules {
        let scope = globals
            .entry(module.name.clone())
            .or_insert_with(Scope::new);
        for item in &module.items {
            declare(
                item,
                &format!("{}::{}", module.path, module.name),
                scope,
                false,
            )
            .map_err(|e| format!("{}: {e}; cannot aggregate safely", module.path))?;
        }
    }
    let mut totals = Totals::new();
    for module in modules {
        let mut collector = Collector {
            globals: &globals,
            scopes: Vec::new(),
            module: &module.name,
            path: module.path,
            totals: &mut totals,
            errors: Vec::new(),
        };
        for item in &module.items {
            collector.visit_item(item);
        }
        if !collector.errors.is_empty() {
            return Err(collector.errors.join("\n"));
        }
    }
    Ok(totals
        .into_iter()
        .map(|(k, v)| (format!("{prefix}{k}"), v))
        .collect())
}

fn insert(scope: &mut Scope, name: String, binding: Binding) -> Result<(), String> {
    if scope.insert(name.clone(), binding).is_some() {
        return Err(format!("ambiguous type binding {name}"));
    }
    Ok(())
}

fn declare(item: &Item, owner: &str, scope: &mut Scope, local: bool) -> Result<(), String> {
    let ident = match item {
        Item::Struct(s) => Some(&s.ident),
        Item::Enum(e) => Some(&e.ident),
        Item::Union(u) => Some(&u.ident),
        Item::Trait(t) => Some(&t.ident),
        _ => None,
    };
    if let Some(ident) = ident {
        let location = ident.span().start();
        let key = if local {
            format!(
                "{owner}::local@{}:{}::{ident}",
                location.line, location.column
            )
        } else {
            format!("{owner}::{ident}")
        };
        insert(scope, ident.to_string(), Binding::Declaration(key))?;
    }
    match item {
        Item::Use(u) if u.leading_colon.is_none() => imports(&u.tree, &mut Vec::new(), scope)?,
        Item::Use(_) => return Err("absolute imports require unsupported scope resolution".into()),
        Item::Type(t) => {
            let binding = match type_path(&t.ty) {
                Ok(target) if t.generics.params.is_empty() => Binding::Alias(target),
                _ => Binding::Unsupported,
            };
            insert(scope, t.ident.to_string(), binding)?;
        }
        Item::Mod(m) if local => {
            return Err(format!(
                "block-local module {} requires unsupported scope resolution",
                m.ident
            ));
        }
        _ => {}
    }
    Ok(())
}

fn imports(tree: &UseTree, parts: &mut Vec<String>, scope: &mut Scope) -> Result<(), String> {
    match tree {
        UseTree::Path(p) => {
            parts.push(p.ident.to_string());
            imports(&p.tree, parts, scope)?;
            parts.pop();
        }
        UseTree::Group(g) => {
            for t in &g.items {
                imports(t, parts, scope)?;
            }
        }
        UseTree::Name(n) => {
            let mut path = parts.clone();
            let name = if n.ident == "self" {
                path.last().cloned().ok_or("unqualified self import")?
            } else {
                path.push(n.ident.to_string());
                n.ident.to_string()
            };
            insert(scope, name, Binding::Alias(path.join("::")))?;
        }
        UseTree::Rename(n) => {
            let mut path = parts.clone();
            if n.ident != "self" {
                path.push(n.ident.to_string());
            }
            insert(scope, n.rename.to_string(), Binding::Alias(path.join("::")))?;
        }
        // A glob is never guessed: any target not explicitly bound fails resolution.
        UseTree::Glob(_) => {}
    }
    Ok(())
}

fn type_path(ty: &syn::Type) -> Result<String, String> {
    if let syn::Type::Path(p) = ty
        && p.qself.is_none()
        && p.path.leading_colon.is_none()
    {
        return Ok(p
            .path
            .segments
            .iter()
            .map(|s| s.ident.to_string())
            .collect::<Vec<_>>()
            .join("::"));
    }
    Err("unsupported implementation/type alias path".into())
}

fn module_target(raw: &str, module: &str) -> Result<String, String> {
    if let Some(rest) = raw.strip_prefix("crate::") {
        return Ok(if rest.contains("::") {
            rest.into()
        } else {
            format!("root::{rest}")
        });
    }
    if let Some(rest) = raw.strip_prefix("self::") {
        return Ok(format!("{module}::{rest}"));
    }
    if let Some(rest) = raw.strip_prefix("super::") {
        if module == "root" {
            return Err("super path escapes crate scope".into());
        }
        return module_target(rest, module.rsplit_once("::").map_or("root", |(p, _)| p));
    }
    Ok(format!("{module}::{raw}"))
}

struct Resolver<'a> {
    globals: &'a BTreeMap<String, Scope>,
    scopes: &'a [Scope],
    module: &'a str,
    seen: BTreeSet<String>,
}

impl Resolver<'_> {
    fn resolve(&mut self, raw: &str, depth: usize) -> Result<String, String> {
        let head = raw.split("::").next().ok_or("empty type path")?;
        if !["crate", "self", "super"].contains(&head) {
            for index in (0..depth).rev() {
                if let Some(binding) = self.scopes[index].get(head) {
                    if raw.contains("::") {
                        return Err(format!("unsupported qualified local type path {raw}"));
                    }
                    let marker = format!("local@{index}::{raw}");
                    return self.binding(binding.clone(), marker, index + 1);
                }
            }
        }
        let name = module_target(raw, self.module)?;
        let (module, ident) = name.rsplit_once("::").ok_or("missing module identity")?;
        let binding = self
            .globals
            .get(module)
            .and_then(|s| s.get(ident))
            .cloned()
            .ok_or_else(|| format!("unresolved implementation type {raw}"))?;
        if !self.seen.insert(name.clone()) {
            return Err(format!("cyclic type alias {name}"));
        }
        match binding {
            Binding::Declaration(key) => Ok(key),
            Binding::Alias(target) => {
                let mut resolver = Resolver {
                    globals: self.globals,
                    scopes: &[],
                    module,
                    seen: self.seen.clone(),
                };
                resolver.resolve(&target, 0)
            }
            Binding::Unsupported => Err(format!("unsupported type alias {name}")),
        }
    }

    fn binding(
        &mut self,
        binding: Binding,
        marker: String,
        depth: usize,
    ) -> Result<String, String> {
        if !self.seen.insert(marker.clone()) {
            return Err(format!("cyclic type alias {marker}"));
        }
        match binding {
            Binding::Declaration(key) => Ok(key),
            Binding::Alias(target) => self.resolve(&target, depth),
            Binding::Unsupported => Err(format!("unsupported type alias {marker}")),
        }
    }
}

struct Collector<'a> {
    globals: &'a BTreeMap<String, Scope>,
    scopes: Vec<Scope>,
    module: &'a str,
    path: &'a str,
    totals: &'a mut Totals,
    errors: Vec<String>,
}

impl Collector<'_> {
    fn identity(&self, raw: &str) -> Result<String, String> {
        Resolver {
            globals: self.globals,
            scopes: &self.scopes,
            module: self.module,
            seen: BTreeSet::new(),
        }
        .resolve(raw, self.scopes.len())
    }

    fn error(&mut self, error: String) {
        self.errors
            .push(format!("{}: {error}; cannot aggregate safely", self.path));
    }

    fn method(&mut self, key: &str, sig: &syn::Signature, block: &syn::Block) {
        let totals = self.totals.entry(key.into()).or_default();
        totals.0 += effective_lines(::quote::quote!(#sig #block));
        totals.1 += 1;
    }
}

impl<'ast> Visit<'ast> for Collector<'_> {
    fn visit_block(&mut self, block: &'ast syn::Block) {
        let mut scope = Scope::new();
        for statement in &block.stmts {
            if let syn::Stmt::Item(item) = statement
                && let Err(error) = declare(
                    item,
                    &format!("{}::{}", self.path, self.module),
                    &mut scope,
                    true,
                )
            {
                self.error(error);
            }
        }
        self.scopes.push(scope);
        visit::visit_block(self, block);
        self.scopes.pop();
    }

    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        match type_path(&item.self_ty).and_then(|raw| self.identity(&raw)) {
            Ok(key) => {
                for member in &item.items {
                    if let syn::ImplItem::Fn(method) = member {
                        self.method(&key, &method.sig, &method.block);
                    }
                }
            }
            Err(error) => self.error(error),
        }
        visit::visit_item_impl(self, item);
    }

    fn visit_item_trait(&mut self, item: &'ast syn::ItemTrait) {
        match self.identity(&item.ident.to_string()) {
            Ok(key) => {
                for member in &item.items {
                    if let syn::TraitItem::Fn(method) = member
                        && let Some(block) = &method.default
                    {
                        self.method(&key, &method.sig, block);
                    }
                }
            }
            Err(error) => self.error(error),
        }
        visit::visit_item_trait(self, item);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        if let Err(error) = crate::macro_policy::visit_inputs(self, mac, self.path) {
            self.error(error);
        }
    }
}
