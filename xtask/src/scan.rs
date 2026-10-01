use crate::{
    coupling,
    metrics::{self, Report},
};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub struct Scan {
    pub reports: Vec<Report>,
    pub feedback: BTreeSet<coupling::Edge>,
    pub cyclic: BTreeSet<coupling::Edge>,
    pub suppressions: Vec<String>,
    pub types: std::collections::BTreeMap<String, (usize, usize)>,
}

pub fn scan(root: &Path, roots: &[&str]) -> Result<Scan, String> {
    let mut paths = Vec::new();
    for required in roots {
        let before = paths.len();
        enumerate(&root.join(required), &mut paths)?;
        if paths.len() == before {
            return Err(format!("{required}: incomplete scan: no Rust inputs"));
        }
    }
    paths.sort();
    if paths.is_empty() {
        return Err("incomplete scan: no Rust inputs".into());
    }
    let mut sources = Vec::new();
    let mut reports = Vec::new();
    let mut suppressions = Vec::new();
    let mut quote_used = false;
    for path in paths {
        let relative = path
            .strip_prefix(root)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        let source =
            fs::read_to_string(&path).map_err(|e| format!("{relative}: unreadable input: {e}"))?;
        let syntax =
            syn::parse_file(&source).map_err(|e| format!("{relative}: parse error: {e}"))?;
        quote_used |= crate::macro_policy::validate_bindings(&relative, &syntax)?;
        reports.push(metrics::analyze(&relative, &source)?);
        suppressions.extend(crate::suppression::check(&relative, &source)?);
        sources.push((relative, source));
    }
    if quote_used {
        crate::quote_identity::verify(root)?;
    }
    let (feedback, cyclic, types) = graph(&sources)?;
    Ok(Scan {
        reports,
        feedback,
        cyclic,
        suppressions,
        types,
    })
}

fn enumerate(path: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| format!("{}: incomplete scan: {e}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!("{}: symlink scan input forbidden", path.display()));
    }
    if metadata.is_file() {
        if path.extension().is_some_and(|e| e == "rs") {
            files.push(path.into());
        }
    } else if metadata.is_dir() {
        let mut entries = fs::read_dir(path)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .map(|entry| entry.map(|e| e.path()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("{}: incomplete enumeration: {e}", path.display()))?;
        entries.sort();
        for entry in entries {
            enumerate(&entry, files)?;
        }
    } else {
        return Err(format!("{}: unsupported scan input", path.display()));
    }
    Ok(())
}

type GraphScan = (
    BTreeSet<coupling::Edge>,
    BTreeSet<coupling::Edge>,
    std::collections::BTreeMap<String, (usize, usize)>,
);
fn graph(sources: &[(String, String)]) -> Result<GraphScan, String> {
    use quote::ToTokens;
    let mut output = BTreeSet::new();
    let mut types = std::collections::BTreeMap::new();
    for prefix in ["src/", "xtask/src/"] {
        let inventory = crate::modules::inventory(sources, prefix)?;
        let modules = inventory.iter().map(|m| m.name.clone()).collect();
        let mut bindings = crate::module_bindings::Bindings::new(&modules, true);
        let dual_entry = inventory.iter().filter(|m| m.name == "root").count() > 1;
        for module in &inventory {
            if dual_entry && module.name == "root" && module.path.ends_with("/main.rs") {
                continue;
            }
            bindings.add_items(bindings.module(&module.name), &module.items);
        }
        types.extend(crate::type_aggregate::aggregate(&inventory, "")?);
        for module in &inventory {
            let source = module
                .items
                .iter()
                .map(|i| i.to_token_stream().to_string())
                .collect::<Vec<_>>()
                .join("\n");
            let mut binary =
                crate::module_bindings::Bindings::new(&BTreeSet::from(["root".into()]), true);
            let scope_bindings =
                if dual_entry && module.name == "root" && module.path.ends_with("/main.rs") {
                    if syn::parse_file(
                        &sources
                            .iter()
                            .find(|(p, _)| p == module.path)
                            .ok_or("missing binary source")?
                            .1,
                    )
                    .map_err(|e| e.to_string())?
                    .items
                    .iter()
                    .any(|i| matches!(i, syn::Item::Mod(_)))
                    {
                        return Err("unsupported shared lib/main module identities".into());
                    }
                    binary.add_items(binary.module("root"), &module.items);
                    &mut binary
                } else {
                    &mut bindings
                };
            for (from, to) in
                coupling::edges_in(&module.name, &source, scope_bindings, module.path)?
            {
                output.insert((format!("{prefix}{from}"), format!("{prefix}{to}")));
            }
        }
    }
    Ok((
        coupling::feedback_edges(&output),
        coupling::cyclic_edges(&output),
        types,
    ))
}
