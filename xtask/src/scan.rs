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
    for path in paths {
        let relative = path
            .strip_prefix(root)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        let source =
            fs::read_to_string(&path).map_err(|e| format!("{relative}: unreadable input: {e}"))?;
        reports.push(metrics::analyze(&relative, &source)?);
        suppressions.extend(crate::suppression::check(&relative, &source)?);
        sources.push((relative, source));
    }
    let (feedback, types) = graph(&sources)?;
    Ok(Scan {
        reports,
        feedback,
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
    std::collections::BTreeMap<String, (usize, usize)>,
);
fn graph(sources: &[(String, String)]) -> Result<GraphScan, String> {
    use quote::ToTokens;
    let mut output = BTreeSet::new();
    let mut types = std::collections::BTreeMap::new();
    for prefix in ["src/", "xtask/src/"] {
        let inventory = crate::modules::inventory(sources, prefix)?;
        let modules = inventory.iter().map(|m| m.name.clone()).collect();
        types.extend(crate::type_aggregate::aggregate(&inventory, "")?);
        for module in &inventory {
            let source = module
                .items
                .iter()
                .map(|i| i.to_token_stream().to_string())
                .collect::<Vec<_>>()
                .join("\n");
            for (from, to) in coupling::edges(&module.name, &source, &modules)? {
                output.insert((format!("{prefix}{from}"), format!("{prefix}{to}")));
            }
        }
    }
    Ok((coupling::feedback_edges(&output), types))
}
