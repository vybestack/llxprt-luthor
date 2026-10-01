//! Resolve the declared source tree, including every cfg alternative, without expansion.
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
use syn::Item;

pub struct Module<'a> {
    pub path: &'a str,
    pub items: Vec<Item>,
    pub name: String,
}

pub fn inventory<'a>(
    sources: &'a [(String, String)],
    prefix: &str,
) -> Result<Vec<Module<'a>>, String> {
    let indexed: BTreeMap<_, _> = sources
        .iter()
        .filter(|(p, _)| p.starts_with(prefix))
        .map(|(p, s)| (p.as_str(), s.as_str()))
        .collect();
    let mut modules = Vec::new();
    let mut seen = BTreeSet::new();
    for entry in ["lib.rs", "main.rs"] {
        let path = format!("{prefix}{entry}");
        if let Some((&key, _)) = indexed.get_key_value(path.as_str()) {
            load(
                key,
                "root",
                prefix.trim_end_matches('/'),
                &indexed,
                &mut seen,
                &mut modules,
            )?;
        }
    }
    if seen.is_empty() && !indexed.is_empty() {
        return Err(format!(
            "{prefix}: missing crate entry; incomplete module scan"
        ));
    }
    for path in indexed.keys() {
        if !seen.contains(path) {
            return Err(format!(
                "{path}: unreachable source; incomplete module scan"
            ));
        }
    }
    Ok(modules)
}

fn load<'a>(
    path: &'a str,
    name: &str,
    directory: &str,
    indexed: &BTreeMap<&'a str, &str>,
    seen: &mut BTreeSet<&'a str>,
    modules: &mut Vec<Module<'a>>,
) -> Result<(), String> {
    if !seen.insert(path) {
        return Err(format!("{path}: repeated module source identity"));
    }
    let source = indexed
        .get(path)
        .ok_or_else(|| format!("{path}: missing declared source"))?;
    let file = syn::parse_file(source).map_err(|e| format!("{path}: {e}"))?;
    descend(path, name, directory, file.items, indexed, seen, modules)
}

fn descend<'a>(
    path: &'a str,
    name: &str,
    directory: &str,
    items: Vec<Item>,
    indexed: &BTreeMap<&'a str, &str>,
    seen: &mut BTreeSet<&'a str>,
    modules: &mut Vec<Module<'a>>,
) -> Result<(), String> {
    let mut own = Vec::new();
    for item in items {
        if let Item::Mod(module) = &item {
            let child = if name == "root" {
                module.ident.to_string()
            } else {
                format!("{name}::{}", module.ident)
            };
            let dir = format!("{directory}/{}", module.ident);
            if let Some((_, nested)) = &module.content {
                descend(path, &child, &dir, nested.clone(), indexed, seen, modules)?;
            } else {
                let flat = format!("{dir}.rs");
                let nested = format!("{dir}/mod.rs");
                let candidates: Vec<_> = [flat, nested]
                    .into_iter()
                    .filter_map(|p| indexed.get_key_value(p.as_str()).map(|(&k, _)| k))
                    .collect();
                if candidates.len() != 1 {
                    return Err(format!("{path}: module {child}: missing/ambiguous source"));
                }
                load(candidates[0], &child, &dir, indexed, seen, modules)?;
            }
        } else {
            own.push(item);
        }
    }
    modules.push(Module {
        path,
        name: name.into(),
        items: own,
    });
    Ok(())
}

pub fn module_path(path: &str) -> String {
    Path::new(path)
        .with_extension("")
        .to_string_lossy()
        .replace('/', "::")
}
