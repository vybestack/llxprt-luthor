//! Resolve derive bindings before trusting builtin or registry-generated metadata.
use crate::attribute_policy::{self, Derive};
use std::{collections::BTreeMap, path::Path, process::Command};
use syn::{Path as SynPath, ext::IdentExt};

#[derive(Clone)]
enum Binding {
    Module(usize),
    Import(usize, Vec<String>),
    External(Vec<String>),
    Unsupported,
}

#[derive(Default)]
struct Scope {
    names: BTreeMap<String, Binding>,
    externs: BTreeMap<String, Vec<String>>,
    glob: bool,
    parent: Option<usize>,
    module: String,
}

pub(crate) struct Bindings {
    scopes: Vec<Scope>,
    modules: BTreeMap<String, usize>,
}

impl Bindings {
    pub(crate) fn new(modules: &[crate::modules::Module<'_>]) -> Self {
        let mut result = Self {
            scopes: Vec::new(),
            modules: BTreeMap::new(),
        };
        result.add_module("root");
        for module in modules {
            result.add_module(&module.name);
        }
        for module in modules {
            if module.name != "root" {
                let (parent, name) = module
                    .name
                    .rsplit_once("::")
                    .unwrap_or(("root", &module.name));
                let parent = result.module(parent);
                let child = result.module(&module.name);
                result.insert(parent, name.into(), Binding::Module(child));
            }
            let scope = result.module(&module.name);
            result.add_items(scope, &module.items);
        }
        result
    }

    pub(crate) fn standalone(file: &syn::File) -> Self {
        let mut result = Self {
            scopes: Vec::new(),
            modules: BTreeMap::new(),
        };
        result.add_module("root");
        result.add_items(0, &file.items);
        result
    }

    fn add_module(&mut self, name: &str) -> usize {
        let name = normalized_module(name);
        if let Some(id) = self.modules.get(&name) {
            return *id;
        }
        let id = self.scopes.len();
        self.modules.insert(name.clone(), id);
        self.scopes.push(Scope {
            module: name,
            ..Scope::default()
        });
        id
    }

    pub(crate) fn module(&self, name: &str) -> usize {
        self.modules[&normalized_module(name)]
    }

    pub(crate) fn nested(&self, scope: usize, name: &str) -> usize {
        self.module(&crate::module_bindings::child(
            &self.scopes[scope].module,
            name,
        ))
    }

    fn insert(&mut self, scope: usize, name: String, binding: Binding) {
        use std::collections::btree_map::Entry;
        match self.scopes[scope]
            .names
            .entry(name.trim_start_matches("r#").into())
        {
            Entry::Vacant(entry) => {
                entry.insert(binding);
            }
            Entry::Occupied(mut entry) => {
                entry.insert(Binding::Unsupported);
            }
        }
    }

    fn add_items(&mut self, scope: usize, items: &[syn::Item]) {
        for item in items {
            match item {
                syn::Item::Use(item) => {
                    let mut imports = Vec::new();
                    let mut prefix = if item.leading_colon.is_some() {
                        vec![String::new()]
                    } else {
                        Vec::new()
                    };
                    crate::module_bindings::imports(&item.tree, &mut prefix, &mut imports);
                    for (name, parts) in imports {
                        let clean = |s: String| s.trim_start_matches("r#").to_owned();
                        if name == "*" {
                            self.scopes[scope].glob = true;
                        } else {
                            self.insert(
                                scope,
                                clean(name),
                                Binding::Import(scope, parts.into_iter().map(clean).collect()),
                            );
                        }
                    }
                }
                syn::Item::ExternCrate(item) => {
                    let original = item.ident.unraw().to_string();
                    let alias = item
                        .rename
                        .as_ref()
                        .map_or(&item.ident, |(_, n)| n)
                        .unraw()
                        .to_string();
                    self.scopes[scope]
                        .externs
                        .insert(alias.clone(), vec![original.clone()]);
                    self.insert(scope, alias, Binding::External(vec![original]));
                }
                syn::Item::Mod(item) => {
                    let name = item.ident.unraw().to_string();
                    let module = crate::module_bindings::child(&self.scopes[scope].module, &name);
                    let id = self.add_module(&module);
                    self.insert(scope, name, Binding::Module(id));
                    if let Some((_, items)) = &item.content {
                        self.add_items(id, items);
                    }
                }
                syn::Item::Macro(item) => {
                    if let Some(ident) = &item.ident {
                        self.insert(scope, ident.unraw().to_string(), Binding::Unsupported);
                    }
                }
                _ => {}
            }
        }
    }

    pub(crate) fn block(&mut self, parent: usize, block: &syn::Block) -> usize {
        let id = self.scopes.len();
        self.scopes.push(Scope {
            parent: Some(parent),
            module: self.scopes[parent].module.clone(),
            ..Scope::default()
        });
        let items: Vec<_> = block
            .stmts
            .iter()
            .filter_map(|stmt| {
                if let syn::Stmt::Item(item) = stmt {
                    Some(item.clone())
                } else {
                    None
                }
            })
            .collect();
        self.add_items(id, &items);
        id
    }

    fn resolve(&self, scope: usize, parts: &[String], depth: usize) -> Result<Vec<String>, String> {
        if depth > self.scopes.len() + 16 {
            return Err("recursive derive binding".into());
        }
        let (first, rest) = parts.split_first().ok_or("empty derive binding")?;
        let binding = match first.as_str() {
            "" => {
                let (name, tail) = rest.split_first().ok_or("empty absolute derive binding")?;
                let path = self.scopes[0]
                    .externs
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| vec![name.clone()]);
                return Ok(path.into_iter().chain(tail.iter().cloned()).collect());
            }
            "crate" => Binding::Module(self.module("root")),
            "self" => Binding::Module(self.module(&self.scopes[scope].module)),
            "super" => {
                let module = &self.scopes[scope].module;
                if module == "root" {
                    return Err("derive binding escapes crate".into());
                }
                Binding::Module(self.module(module.rsplit_once("::").map_or("root", |(p, _)| p)))
            }
            _ => {
                let mut current = Some(scope);
                let mut found = None;
                while let Some(id) = current {
                    if let Some(binding) = self.scopes[id].names.get(first) {
                        found = Some(binding.clone());
                        break;
                    }
                    if self.scopes[id].glob {
                        return Err("unsupported glob derive binding".into());
                    }
                    current = self.scopes[id].parent;
                }
                found.unwrap_or_else(|| Binding::External(vec![first.clone()]))
            }
        };
        match binding {
            Binding::Module(id) => {
                if !rest.first().is_some_and(|n| {
                    self.scopes[id].names.contains_key(n)
                        || ["self", "super", "crate"].contains(&n.as_str())
                }) {
                    return Err("unresolved internal derive reexport".into());
                }
                self.resolve(id, rest, depth + 1)
            }
            Binding::Import(id, mut path) => {
                path.extend_from_slice(rest);
                self.resolve(id, &path, depth + 1)
            }
            Binding::External(mut path) => {
                path.extend_from_slice(rest);
                Ok(path)
            }
            Binding::Unsupported => Err("ambiguous or unsupported derive binding".into()),
        }
    }

    pub(crate) fn derive_attribute(&self, scope: usize, path: &SynPath) -> Result<(), String> {
        let parts: Vec<_> = attribute_policy::name(path)
            .split("::")
            .map(str::to_owned)
            .collect();
        let resolved = self.resolve(scope, &parts, 0)?;
        if path.leading_colon.is_none() && resolved == ["derive"] {
            Ok(())
        } else {
            Err(format!(
                "unsupported/unverified derive attribute binding {}",
                resolved.join("::")
            ))
        }
    }

    pub(crate) fn derive(
        &self,
        scope: usize,
        path: &SynPath,
        dependencies: &mut Dependencies<'_>,
        file: &str,
    ) -> Result<Derive, String> {
        if path
            .segments
            .iter()
            .any(|s| !matches!(s.arguments, syn::PathArguments::None))
        {
            return Err("unsupported derive arguments".into());
        }
        let mut parts: Vec<_> = attribute_policy::name(path)
            .split("::")
            .map(str::to_owned)
            .collect();
        if path.leading_colon.is_some() {
            parts.insert(0, String::new());
        }
        let resolved = self.resolve(scope, &parts, 0)?;
        if builtin(&resolved) {
            if resolved.len() > 1 {
                dependencies.builtin(file, &resolved[0])?;
            }
            return Ok(Derive::Builtin);
        }
        dependencies.derive(file, &resolved).map_err(|e| {
            format!(
                "unsupported/unverified derive {}: {e}",
                attribute_policy::name(path)
            )
        })
    }
}

fn normalized_module(name: &str) -> String {
    name.split("::")
        .map(|part| part.trim_start_matches("r#"))
        .collect::<Vec<_>>()
        .join("::")
}

fn builtin(parts: &[String]) -> bool {
    let parts: Vec<_> = parts.iter().map(String::as_str).collect();
    matches!(
        parts.as_slice(),
        ["Clone"
            | "Copy"
            | "Debug"
            | "Default"
            | "Eq"
            | "PartialEq"
            | "Ord"
            | "PartialOrd"
            | "Hash"]
            | ["core" | "std", "clone", "Clone"]
            | ["core" | "std", "marker", "Copy"]
            | ["core" | "std", "fmt", "Debug"]
            | ["core" | "std", "default", "Default"]
            | [
                "core" | "std",
                "cmp",
                "Eq" | "PartialEq" | "Ord" | "PartialOrd"
            ]
            | ["core" | "std", "hash", "Hash"]
    )
}

pub(crate) struct Dependencies<'a> {
    root: Option<&'a Path>,
    metadata: Option<serde_json::Value>,
}

impl<'a> Dependencies<'a> {
    pub(crate) fn new(root: Option<&'a Path>) -> Self {
        Self {
            root,
            metadata: None,
        }
    }

    fn metadata(&mut self) -> Result<&serde_json::Value, String> {
        if self.metadata.is_none() {
            let root = self
                .root
                .ok_or("dependency identity requires a workspace scan")?;
            let output = Command::new("cargo")
                .args([
                    "metadata",
                    "--offline",
                    "--locked",
                    "--all-features",
                    "--format-version",
                    "1",
                ])
                .current_dir(root)
                .output()
                .map_err(|e| e.to_string())?;
            if !output.status.success() {
                return Err(format!(
                    "derive identity: cargo metadata: {}",
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
            self.metadata =
                Some(serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?);
        }
        Ok(self.metadata.as_ref().unwrap())
    }

    fn package(&mut self, file: &str, binding: &str) -> Result<Option<serde_json::Value>, String> {
        let root = self
            .root
            .ok_or("dependency identity requires a workspace scan")?;
        let source = root.join(file);
        let metadata = self.metadata()?;
        let packages = metadata["packages"].as_array().ok_or("missing packages")?;
        let owner = packages
            .iter()
            .filter(|p| {
                p["manifest_path"]
                    .as_str()
                    .and_then(|s| Path::new(s).parent())
                    .is_some_and(|p| source.starts_with(p))
            })
            .max_by_key(|p| p["manifest_path"].as_str().unwrap().len())
            .ok_or("missing source package")?;
        let node = metadata["resolve"]["nodes"]
            .as_array()
            .ok_or("missing resolve nodes")?
            .iter()
            .find(|n| n["id"] == owner["id"])
            .ok_or("missing source resolve node")?;
        let dep = node["deps"]
            .as_array()
            .ok_or("missing dependencies")?
            .iter()
            .find(|d| d["name"] == binding);
        Ok(dep.and_then(|d| packages.iter().find(|p| p["id"] == d["pkg"]).cloned()))
    }

    fn builtin(&mut self, file: &str, binding: &str) -> Result<(), String> {
        if self.root.is_some_and(|r| r.join("Cargo.toml").exists())
            && self.package(file, binding)?.is_some()
        {
            return Err(format!(
                "builtin derive namespace {binding} is replaced by a dependency"
            ));
        }
        Ok(())
    }

    fn registry_tree(&mut self, package: &serde_json::Value) -> Result<(), String> {
        let metadata = self.metadata()?;
        let packages = metadata["packages"].as_array().ok_or("missing packages")?;
        let nodes = metadata["resolve"]["nodes"]
            .as_array()
            .ok_or("missing resolve nodes")?;
        let mut pending = vec![package["id"].clone()];
        let mut seen = std::collections::BTreeSet::new();
        while let Some(id) = pending.pop() {
            if !seen.insert(id.to_string()) {
                continue;
            }
            let package = packages
                .iter()
                .find(|p| p["id"] == id)
                .ok_or("missing derive dependency package")?;
            if package["source"] != "registry+https://github.com/rust-lang/crates.io-index" {
                return Err(format!(
                    "derive dependency is not the registry package (path/git replacement): {}",
                    package["name"]
                ));
            }
            let node = nodes
                .iter()
                .find(|n| n["id"] == id)
                .ok_or("missing derive dependency resolve node")?;
            for dep in node["deps"]
                .as_array()
                .ok_or("missing derive dependency edges")?
            {
                pending.push(dep["pkg"].clone());
            }
        }
        Ok(())
    }

    fn derive(&mut self, file: &str, parts: &[String]) -> Result<Derive, String> {
        let [binding, name] = parts else {
            return Err("unsupported reexport or derive path".into());
        };
        let package = self
            .package(file, binding)?
            .ok_or("missing direct derive dependency")?;
        let version = package["version"].as_str().unwrap_or("");
        self.registry_tree(&package)?;
        match (package["name"].as_str(), name.as_str()) {
            (Some("serde" | "serde_derive"), "Serialize" | "Deserialize")
                if version.starts_with("1.") =>
            {
                Ok(Derive::Serde)
            }
            (Some("thiserror" | "thiserror-impl"), "Error")
                if version.starts_with("1.") || version.starts_with("2.") =>
            {
                Ok(Derive::Error)
            }
            _ => Err("unsupported resolved derive package or version".into()),
        }
    }
}
