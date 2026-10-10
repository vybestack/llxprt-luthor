//! Validate attributes in their lexical and derive-helper scopes, without expansion.
use crate::{
    attribute_policy::{self, Helpers, Site},
    derive_identity::{Bindings, Dependencies},
};
use syn::visit::{self, Visit};

pub(crate) fn standalone(path: &str, syntax: &syn::File) -> Result<(), String> {
    let mut bindings = Bindings::standalone(syntax);
    validate(path, syntax, &mut bindings, 0, &mut Dependencies::new(None))
}

pub(crate) fn files(root: &std::path::Path, sources: &[(String, String)]) -> Result<(), String> {
    let mut dependencies = Dependencies::new(Some(root));
    for prefix in ["src/", "xtask/src/"] {
        let modules = crate::modules::inventory(sources, prefix)?;
        let mut bindings = Bindings::new(&modules);
        for (path, source) in sources.iter().filter(|(p, _)| p.starts_with(prefix)) {
            let syntax = syn::parse_file(source).map_err(|e| e.to_string())?;
            if path.ends_with("/main.rs") {
                let mut binary = Bindings::standalone(&syntax);
                validate(path, &syntax, &mut binary, 0, &mut dependencies)?;
            } else {
                let module = modules
                    .iter()
                    .filter(|m| m.path == path)
                    .min_by_key(|m| (m.name != "root", m.name.split("::").count()))
                    .ok_or("missing attribute module identity")?;
                let scope = bindings.module(&module.name);
                validate(path, &syntax, &mut bindings, scope, &mut dependencies)?;
            }
        }
    }
    for (path, source) in sources
        .iter()
        .filter(|(p, _)| !p.starts_with("src/") && !p.starts_with("xtask/src/"))
    {
        let syntax = syn::parse_file(source).map_err(|e| e.to_string())?;
        let mut bindings = Bindings::standalone(&syntax);
        validate(path, &syntax, &mut bindings, 0, &mut dependencies)?;
    }
    Ok(())
}

fn validate(
    path: &str,
    syntax: &syn::File,
    bindings: &mut Bindings,
    scope: usize,
    dependencies: &mut Dependencies<'_>,
) -> Result<(), String> {
    let mut validator = Validator {
        path,
        bindings,
        dependencies,
        scope,
        site: Site::Other,
        helpers: Helpers::default(),
        errors: Vec::new(),
    };
    validator.visit_file(syntax);
    if validator.errors.is_empty() {
        Ok(())
    } else {
        Err(validator.errors.join("\n"))
    }
}

struct Validator<'a, 'b> {
    path: &'a str,
    bindings: &'a mut Bindings,
    dependencies: &'a mut Dependencies<'b>,
    scope: usize,
    site: Site,
    helpers: Helpers,
    errors: Vec<String>,
}

impl Validator<'_, '_> {
    fn helpers(&mut self, meta: &syn::Meta, helpers: &mut Helpers) -> Result<(), String> {
        attribute_policy::extend_helpers(
            meta,
            helpers,
            &mut |path| {
                self.bindings
                    .derive(self.scope, path, self.dependencies, self.path)
            },
            &mut |path| self.bindings.derive_attribute(self.scope, path),
        )
    }

    fn record(&mut self, attributes: &[syn::Attribute], visit: impl FnOnce(&mut Self)) {
        let old = (self.site, self.helpers);
        self.site = Site::Record;
        let mut helpers = Helpers::default();
        for attribute in attributes {
            if let Err(e) = self.helpers(&attribute.meta, &mut helpers) {
                self.errors.push(format!("{}: {e}", self.path));
            }
        }
        self.helpers = helpers;
        visit(self);
        (self.site, self.helpers) = old;
    }
}

impl<'ast> Visit<'ast> for Validator<'_, '_> {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        let old = (self.site, self.helpers);
        self.site = Site::Other;
        self.helpers = Helpers::default();
        visit::visit_item(self, item);
        (self.site, self.helpers) = old;
    }
    fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
        self.record(&item.attrs, |v| visit::visit_item_struct(v, item));
    }
    fn visit_item_enum(&mut self, item: &'ast syn::ItemEnum) {
        self.record(&item.attrs, |v| visit::visit_item_enum(v, item));
    }
    fn visit_item_union(&mut self, item: &'ast syn::ItemUnion) {
        self.record(&item.attrs, |v| visit::visit_item_union(v, item));
    }
    fn visit_variant(&mut self, variant: &'ast syn::Variant) {
        let old = self.site;
        self.site = Site::Variant;
        visit::visit_variant(self, variant);
        self.site = old;
    }
    fn visit_field(&mut self, field: &'ast syn::Field) {
        let old = self.site;
        self.site = Site::Field;
        visit::visit_field(self, field);
        self.site = old;
    }
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        for attribute in &item.attrs {
            self.visit_attribute(attribute);
        }
        if let Some((_, items)) = &item.content {
            let old = self.scope;
            self.scope = self.bindings.nested(old, &item.ident.to_string());
            for item in items {
                self.visit_item(item);
            }
            self.scope = old;
        }
    }
    fn visit_block(&mut self, block: &'ast syn::Block) {
        let old = self.scope;
        self.scope = self.bindings.block(old, block);
        visit::visit_block(self, block);
        self.scope = old;
    }
    fn visit_attribute(&mut self, attribute: &'ast syn::Attribute) {
        let scope = self.scope;
        let path = self.path;
        let bindings = &self.bindings;
        let dependencies = &mut self.dependencies;
        if let Err(e) = attribute_policy::validate(
            &attribute.meta,
            self.site,
            self.helpers,
            &mut |p| bindings.derive(scope, p, dependencies, path),
            &mut |p| bindings.derive_attribute(scope, p),
        ) {
            self.errors.push(format!("{path}: {e}"));
        }
    }
    fn visit_item_macro(&mut self, _: &'ast syn::ItemMacro) {}
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        if let Err(e) = crate::macro_policy::visit_inputs(self, mac, self.path) {
            self.errors.push(e);
        }
    }
}
