//! Only non-executable metadata may be excluded from the source metric.
use syn::{Meta, Path, Token, ext::IdentExt, parse::Parser, punctuated::Punctuated};

#[derive(Clone, Copy, Default)]
pub(crate) struct Helpers {
    pub serde: bool,
    pub error: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Site {
    Other,
    Record,
    Variant,
    Field,
}

#[derive(Clone, Copy)]
pub(crate) enum Derive {
    Builtin,
    Serde,
    Error,
}

pub(crate) fn name(path: &Path) -> String {
    path.segments
        .iter()
        .map(|s| s.ident.unraw().to_string())
        .collect::<Vec<_>>()
        .join("::")
}

pub(crate) fn derives(meta: &Meta) -> syn::Result<Vec<Path>> {
    let Meta::List(list) = meta else {
        return Err(syn::Error::new_spanned(meta, "malformed derive"));
    };
    Ok(Punctuated::<Path, Token![,]>::parse_terminated
        .parse2(list.tokens.clone())?
        .into_iter()
        .collect())
}

pub(crate) fn extend_helpers(
    meta: &Meta,
    helpers: &mut Helpers,
    resolve: &mut impl FnMut(&Path) -> Result<Derive, String>,
    attribute: &mut impl FnMut(&Path) -> Result<(), String>,
) -> Result<(), String> {
    // Conditional derives only authorize helpers inside their own cfg_attr branch.
    if name(meta.path()) == "derive" {
        attribute(meta.path())?;
        for path in derives(meta).map_err(|e| e.to_string())? {
            match resolve(&path)? {
                Derive::Serde => helpers.serde = true,
                Derive::Error => helpers.error = true,
                Derive::Builtin => {}
            }
        }
    }
    Ok(())
}

pub(crate) fn validate(
    meta: &Meta,
    site: Site,
    helpers: Helpers,
    resolve: &mut impl FnMut(&Path) -> Result<Derive, String>,
    identity: &mut impl FnMut(&Path) -> Result<(), String>,
) -> Result<(), String> {
    let attribute = name(meta.path());
    match attribute.as_str() {
        "path" => Err("unsupported source path attribute".into()),
        "cfg_attr" => {
            let attributes = cfg_attributes(meta)?;
            let mut branch_helpers = helpers;
            if site == Site::Record {
                for attribute in &attributes {
                    extend_helpers(attribute, &mut branch_helpers, resolve, identity)?;
                }
            }
            for attribute in attributes {
                validate(&attribute, site, branch_helpers, resolve, identity)?;
            }
            Ok(())
        }
        "derive" if site == Site::Record => {
            identity(meta.path())?;
            for path in derives(meta).map_err(|e| e.to_string())? {
                resolve(&path)?;
            }
            Ok(())
        }
        "serde" if helpers.serde && site != Site::Other => {
            literal_metadata(meta).map_err(|e| e.to_string())
        }
        "error" if helpers.error && matches!(site, Site::Record | Site::Variant) => {
            error_metadata(meta)
                .map_err(|e| format!("unsupported executable/extended error helper: {e}"))
        }
        "from" | "source" | "backtrace"
            if helpers.error && site == Site::Field && matches!(meta, Meta::Path(_)) =>
        {
            Ok(())
        }
        _ => builtin_metadata(meta).map_err(|e| format!("{attribute}: {e}")),
    }
}

pub(crate) fn cfg_attributes(meta: &Meta) -> Result<Vec<Meta>, String> {
    let Meta::List(list) = meta else {
        return Err("malformed cfg_attr".into());
    };
    let arguments = Punctuated::<Meta, Token![,]>::parse_terminated
        .parse2(list.tokens.clone())
        .map_err(|e| e.to_string())?;
    if arguments.len() < 2 {
        return Err("malformed cfg_attr".into());
    }
    Ok(arguments.into_iter().skip(1).collect())
}

fn error_metadata(meta: &Meta) -> syn::Result<()> {
    let Meta::List(list) = meta else {
        return Err(syn::Error::new_spanned(meta, "unsupported error helper"));
    };
    let parser = |input: syn::parse::ParseStream<'_>| {
        if input.peek(syn::LitStr) {
            input.parse::<syn::LitStr>()?;
        } else if input.parse::<syn::Ident>()?.unraw() != "transparent" {
            return Err(input.error("only literal or transparent error metadata is supported"));
        }
        if input.peek(Token![,]) {
            input.parse::<Token![,]>()?;
        }
        Ok(())
    };
    parser.parse2(list.tokens.clone())
}

fn builtin_metadata(meta: &Meta) -> syn::Result<()> {
    match name(meta.path()).as_str() {
        "doc" => {
            if let Meta::NameValue(value) = meta {
                validate_doc_value(&value.value)?;
            }
        }
        "unsafe" => {
            let Meta::List(list) = meta else {
                return Err(syn::Error::new_spanned(meta, "malformed unsafe attribute"));
            };
            let inner = syn::parse2::<Meta>(list.tokens.clone())?;
            if !["no_mangle", "export_name", "link_section"].contains(&name(inner.path()).as_str())
            {
                return Err(syn::Error::new_spanned(
                    meta,
                    "unsupported unsafe attribute",
                ));
            }
            builtin_metadata(&inner)?;
        }
        "export_name" | "link_section" => {
            let Meta::NameValue(value) = meta else {
                return Err(syn::Error::new_spanned(meta, "expected literal metadata"));
            };
            if !matches!(&value.value, syn::Expr::Lit(v) if matches!(v.lit, syn::Lit::Str(_))) {
                return Err(syn::Error::new_spanned(
                    meta,
                    "expected string literal metadata",
                ));
            }
        }
        "cfg"
        | "repr"
        | "inline"
        | "cold"
        | "must_use"
        | "deprecated"
        | "test"
        | "ignore"
        | "should_panic"
        | "allow"
        | "expect"
        | "warn"
        | "deny"
        | "forbid"
        | "non_exhaustive"
        | "track_caller"
        | "automatically_derived"
        | "used"
        | "no_std"
        | "no_mangle" => {}
        _ => {
            return Err(syn::Error::new_spanned(
                meta,
                "unsupported attribute (expansion required or unverified helper context)",
            ));
        }
    }
    Ok(())
}

fn literal_metadata(meta: &Meta) -> syn::Result<()> {
    match meta {
        Meta::Path(_) => Ok(()),
        Meta::NameValue(value) if matches!(value.value, syn::Expr::Lit(_)) => Ok(()),
        Meta::List(list) => {
            for inner in
                Punctuated::<Meta, Token![,]>::parse_terminated.parse2(list.tokens.clone())?
            {
                literal_metadata(&inner)?;
            }
            Ok(())
        }
        _ => Err(syn::Error::new_spanned(
            meta,
            "unsupported executable helper metadata",
        )),
    }
}

fn validate_doc_value(expression: &syn::Expr) -> syn::Result<()> {
    match expression {
        syn::Expr::Lit(literal) if matches!(literal.lit, syn::Lit::Str(_)) => Ok(()),
        syn::Expr::Macro(expression)
            if ["include_str", "std::include_str", "core::include_str"]
                .contains(&name(&expression.mac.path).as_str()) =>
        {
            syn::parse2::<syn::LitStr>(expression.mac.tokens.clone()).map(|_| ())
        }
        _ => Err(syn::Error::new_spanned(
            expression,
            "unsupported doc value (expansion required)",
        )),
    }
}
