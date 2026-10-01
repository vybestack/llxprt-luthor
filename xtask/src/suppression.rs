use quote::ToTokens;
use syn::{
    Meta, Token,
    parse::Parser,
    punctuated::Punctuated,
    visit::{self, Visit},
};

pub fn check(path: &str, source: &str) -> Result<Vec<String>, String> {
    let syntax = syn::parse_file(source).map_err(|e| format!("{path}: parse error: {e}"))?;
    let mut visitor = Suppressions {
        path,
        findings: Vec::new(),
    };
    visitor.visit_file(&syntax);
    visitor.findings.sort();
    Ok(visitor.findings)
}

struct Suppressions<'a> {
    path: &'a str,
    findings: Vec<String>,
}

impl Suppressions<'_> {
    fn meta(&mut self, meta: &Meta) {
        if meta.path().is_ident("allow") || meta.path().is_ident("expect") {
            self.findings.push(format!(
                "{}:{}: forbidden lint suppression {}",
                self.path,
                meta.path().segments[0].ident.span().start().line,
                meta.to_token_stream()
            ));
        }
        if let Meta::List(list) = meta
            && list.path.is_ident("cfg_attr")
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
    fn visit_attribute(&mut self, attribute: &'ast syn::Attribute) {
        self.meta(&attribute.meta);
        visit::visit_attribute(self, attribute);
    }
}
