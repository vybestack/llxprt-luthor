//! Resolve impl aliases before aggregating responsibilities across files.
use crate::{
    metrics::{effective_lines, type_identity},
    modules::Module,
};
use quote::quote;
use std::collections::BTreeMap;
use syn::{
    Item, UseTree,
    visit::{self, Visit},
};

pub fn aggregate(
    modules: &[Module<'_>],
    prefix: &str,
) -> Result<BTreeMap<String, (usize, usize)>, String> {
    let mut declarations = BTreeMap::new();
    for module in modules {
        for item in &module.items {
            let ident = match item {
                Item::Struct(s) => Some(&s.ident),
                Item::Enum(e) => Some(&e.ident),
                Item::Trait(t) => Some(&t.ident),
                _ => None,
            };
            if let Some(ident) = ident {
                declarations.insert(
                    format!("{}::{ident}", module.name),
                    format!("{}::{}::{ident}", module.path, module.name),
                );
            }
        }
    }
    let mut output = BTreeMap::new();
    for module in modules {
        let mut aliases = BTreeMap::new();
        for item in &module.items {
            if let Item::Use(u) = item {
                imports(&u.tree, &mut Vec::new(), &mut aliases);
            }
        }
        for item in &module.items {
            match item {
                Item::Impl(i) => {
                    let raw = type_identity(&i.self_ty);
                    let target = aliases.get(&raw).unwrap_or(&raw);
                    let name = resolve(target, &module.name);
                    let key = declarations.get(&name).cloned().ok_or_else(|| {
                        format!(
                            "{}: unresolved implementation type {raw}; cannot aggregate safely",
                            module.path
                        )
                    })?;
                    let mut measure = MethodMeasure::default();
                    measure.visit_item_impl(i);
                    let totals = output.entry(format!("{prefix}{key}")).or_insert((0, 0));
                    totals.0 += measure.lines;
                    totals.1 += measure.count;
                }
                Item::Trait(t) => {
                    let key = declarations
                        .get(&format!("{}::{}", module.name, t.ident))
                        .ok_or("missing trait identity")?;
                    let mut measure = MethodMeasure::default();
                    measure.visit_item_trait(t);
                    if measure.count > 0 {
                        let totals = output.entry(format!("{prefix}{key}")).or_insert((0, 0));
                        totals.0 += measure.lines;
                        totals.1 += measure.count;
                    }
                }
                _ => {}
            }
        }
    }
    Ok(output)
}

fn resolve(raw: &str, module: &str) -> String {
    if let Some(rest) = raw.strip_prefix("crate::") {
        return rest.into();
    }
    if let Some(rest) = raw.strip_prefix("self::") {
        return format!("{module}::{rest}");
    }
    if let Some(rest) = raw.strip_prefix("super::") {
        return resolve(
            &format!("self::{rest}"),
            module.rsplit_once("::").map_or("root", |(p, _)| p),
        );
    }
    format!("{module}::{raw}")
}

fn imports(tree: &UseTree, parts: &mut Vec<String>, aliases: &mut BTreeMap<String, String>) {
    match tree {
        UseTree::Path(p) => {
            parts.push(p.ident.to_string());
            imports(&p.tree, parts, aliases);
            parts.pop();
        }
        UseTree::Group(g) => {
            for t in &g.items {
                imports(t, parts, aliases);
            }
        }
        UseTree::Name(n) => {
            let mut path = parts.clone();
            path.push(n.ident.to_string());
            aliases.insert(n.ident.to_string(), path.join("::"));
        }
        UseTree::Rename(n) => {
            let mut path = parts.clone();
            path.push(n.ident.to_string());
            aliases.insert(n.rename.to_string(), path.join("::"));
        }
        UseTree::Glob(_) => {}
    }
}

#[derive(Default)]
struct MethodMeasure {
    lines: usize,
    count: usize,
}
impl<'ast> Visit<'ast> for MethodMeasure {
    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        let sig = &item.sig;
        let block = &item.block;
        self.lines += effective_lines(quote!(#sig #block));
        self.count += 1;
        visit::visit_impl_item_fn(self, item);
    }
    fn visit_trait_item_fn(&mut self, item: &'ast syn::TraitItemFn) {
        if let Some(block) = &item.default {
            let sig = &item.sig;
            self.lines += effective_lines(quote!(#sig #block));
            self.count += 1;
        }
    }
}
