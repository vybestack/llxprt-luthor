use quote::ToTokens;
use syn::{
    Meta, Token,
    parse::Parser,
    punctuated::Punctuated,
    visit::{self, Visit},
};

pub fn check(path: &str, source: &str) -> Result<Vec<String>, String> {
    let syntax = syn::parse_file(source).map_err(|e| format!("{path}: parse error: {e}"))?;
    crate::macro_policy::validate_bindings(path, &syntax)?;
    let mut visitor = Suppressions {
        path,
        findings: Vec::new(),
        errors: Vec::new(),
    };
    visitor.visit_file(&syntax);
    visitor.findings.sort();
    if visitor.errors.is_empty() {
        Ok(visitor.findings)
    } else {
        Err(visitor.errors.join("\n"))
    }
}

pub(crate) fn attribute_findings(path: &str, attribute: &syn::Attribute) -> Vec<String> {
    let mut visitor = Suppressions {
        path,
        findings: Vec::new(),
        errors: Vec::new(),
    };
    visitor.visit_attribute(attribute);
    visitor.findings
}

struct Suppressions<'a> {
    path: &'a str,
    findings: Vec<String>,
    errors: Vec<String>,
}

impl Suppressions<'_> {
    fn meta(&mut self, meta: &Meta) {
        if ["allow", "expect"].contains(&crate::attribute_policy::name(meta.path()).as_str()) {
            self.findings.push(format!(
                "{}:{}: forbidden lint suppression {}",
                self.path,
                meta.path().segments[0].ident.span().start().line,
                meta.to_token_stream()
            ));
        }
        if let Meta::List(list) = meta
            && crate::attribute_policy::name(&list.path) == "cfg_attr"
        {
            match Punctuated::<Meta, Token![,]>::parse_terminated.parse2(list.tokens.clone()) {
                Ok(metas) => {
                    for inner in metas.iter().skip(1) {
                        self.meta(inner);
                    }
                }
                Err(error) => self
                    .findings
                    .push(format!("{}: malformed cfg_attr: {error}", self.path)),
            }
        }
    }
}

impl<'ast> Visit<'ast> for Suppressions<'_> {
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        if let Err(error) = crate::macro_policy::visit_inputs(self, mac, self.path) {
            self.errors.push(error);
        }
    }
    fn visit_attribute(&mut self, attribute: &'ast syn::Attribute) {
        self.meta(&attribute.meta);
        visit::visit_attribute(self, attribute);
    }
}
