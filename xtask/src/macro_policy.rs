//! Parse executable macro inputs; token-construction macros are data only in xtask.
use quote::ToTokens;
use syn::{
    Expr, LitStr, Macro, Pat, Token,
    parse::{ParseStream, Parser},
    visit::Visit,
};

#[derive(Default)]
struct Inputs {
    expressions: Vec<Expr>,
    patterns: Vec<Pat>,
}

pub fn visit_inputs<V: for<'ast> Visit<'ast>>(
    visitor: &mut V,
    mac: &Macro,
    path: &str,
) -> Result<(), String> {
    let inputs = parse(mac, path).map_err(|error| {
        format!(
            "{path}:{}: unsupported or malformed macro {}: {error}",
            mac.path.segments[0].ident.span().start().line,
            mac.path.to_token_stream()
        )
    })?;
    for expression in &inputs.expressions {
        visitor.visit_expr(expression);
    }
    for pattern in &inputs.patterns {
        visitor.visit_pat(pattern);
    }
    Ok(())
}

pub(crate) fn validate_bindings(path: &str, syntax: &syn::File) -> Result<bool, String> {
    let mut guard = QuoteBindings {
        path,
        used: false,
        errors: Vec::new(),
    };
    guard.visit_file(syntax);
    if guard.errors.is_empty() {
        Ok(guard.used)
    } else {
        Err(guard.errors.join("\n"))
    }
}

struct QuoteBindings<'a> {
    path: &'a str,
    used: bool,
    errors: Vec<String>,
}

impl<'ast> Visit<'ast> for QuoteBindings<'_> {
    fn visit_item_extern_crate(&mut self, item: &'ast syn::ItemExternCrate) {
        let binding = item.rename.as_ref().map_or(&item.ident, |(_, name)| name);
        if binding == "quote" {
            self.errors.push(format!(
                "{}: quote macro crate rebinding is unsupported",
                self.path
            ));
        }
    }
    fn visit_item_macro(&mut self, _: &'ast syn::ItemMacro) {
        // Item macro expansion is rejected by the production collectors.
    }
    fn visit_macro(&mut self, mac: &'ast Macro) {
        if mac.path.segments.last().is_some_and(|s| s.ident == "quote") {
            self.used = true;
        }
        if let Err(error) = visit_inputs(self, mac, self.path) {
            self.errors.push(error);
        }
    }
}

fn parse(mac: &Macro, path: &str) -> syn::Result<Inputs> {
    let name = mac
        .path
        .segments
        .last()
        .ok_or_else(|| syn::Error::new_spanned(&mac.path, "missing macro name"))?
        .ident
        .to_string();
    let qualified = mac.path.to_token_stream().to_string().replace(' ', "");
    let namespace = match name.as_str() {
        "json" => "serde_json",
        "params" => "rusqlite",
        "quote" => "quote",
        "Token" | "braced" | "bracketed" | "parenthesized" => "syn",
        _ => "std",
    };
    if qualified != name && qualified.trim_start_matches("::") != format!("{namespace}::{name}") {
        return Err(syn::Error::new_spanned(
            &mac.path,
            "unsupported macro namespace",
        ));
    }
    if name == "quote" {
        if path.starts_with("xtask/") && qualified == "::quote::quote" {
            // Absolute paths bypass local imports; scan verifies the registry dependency.
            return Ok(Inputs::default());
        }
        return Err(syn::Error::new_spanned(
            &mac.path,
            "token data requires the verified absolute ::quote::quote macro in xtask",
        ));
    }
    if name == "Token"
        && path.starts_with("xtask/")
        && [",", ";", ":", "if", "in"].contains(&mac.tokens.to_string().as_str())
    {
        return Ok(Inputs::default());
    }
    if ["braced", "bracketed", "parenthesized"].contains(&name.as_str())
        && !path.starts_with("xtask/")
    {
        return Err(syn::Error::new_spanned(
            &mac.path,
            "syn parser macros are only supported in xtask",
        ));
    }
    let parser = |input: ParseStream<'_>| parse_supported(&name, input);
    parser.parse2(mac.tokens.clone())
}

fn parse_supported(name: &str, input: ParseStream<'_>) -> syn::Result<Inputs> {
    let mut inputs = Inputs::default();
    match name {
        "vec" => vector(input, &mut inputs)?,
        "params" => expressions(input, &mut inputs)?,
        "format" | "format_args" | "print" | "eprint" => formatting(input, &mut inputs)?,
        "println" | "eprintln" | "panic" | "unreachable" | "todo" => {
            if !input.is_empty() {
                formatting(input, &mut inputs)?;
            }
        }
        "write" | "writeln" => writer_formatting(name, input, &mut inputs)?,
        "assert" | "debug_assert" => assertion(input, &mut inputs, 1)?,
        "assert_eq" | "assert_ne" | "debug_assert_eq" | "debug_assert_ne" => {
            assertion(input, &mut inputs, 2)?;
        }
        "matches" => matching(input, &mut inputs)?,
        "json" => json_value(input, &mut inputs)?,
        "braced" | "bracketed" | "parenthesized" => {
            parser_input(input, &mut inputs)?;
        }
        "env" | "option_env" | "include_str" | "include_bytes" => {
            literal_input(name, input)?;
        }
        _ => return Err(input.error("expansion required; macro grammar is not supported")),
    }
    Ok(inputs)
}

fn writer_formatting(name: &str, input: ParseStream<'_>, inputs: &mut Inputs) -> syn::Result<()> {
    inputs.expressions.push(input.parse()?);
    if name == "write" || !input.is_empty() {
        input.parse::<Token![,]>()?;
        formatting(input, inputs)?;
    }
    Ok(())
}

fn parser_input(input: ParseStream<'_>, inputs: &mut Inputs) -> syn::Result<()> {
    input.parse::<syn::Ident>()?;
    input.parse::<Token![in]>()?;
    inputs.expressions.push(input.parse()?);
    Ok(())
}

fn literal_input(name: &str, input: ParseStream<'_>) -> syn::Result<()> {
    input.parse::<LitStr>()?;
    if !input.is_empty() && name == "env" {
        input.parse::<Token![,]>()?;
        input.parse::<LitStr>()?;
    }
    Ok(())
}

fn expressions(input: ParseStream<'_>, inputs: &mut Inputs) -> syn::Result<()> {
    while !input.is_empty() {
        inputs.expressions.push(input.parse()?);
        comma(input)?;
    }
    Ok(())
}

fn comma(input: ParseStream<'_>) -> syn::Result<()> {
    if !input.is_empty() {
        input.parse::<Token![,]>()?;
    }
    Ok(())
}

fn vector(input: ParseStream<'_>, inputs: &mut Inputs) -> syn::Result<()> {
    if input.is_empty() {
        return Ok(());
    }
    inputs.expressions.push(input.parse()?);
    if input.peek(Token![;]) {
        input.parse::<Token![;]>()?;
        inputs.expressions.push(input.parse()?);
    } else {
        comma(input)?;
        expressions(input, inputs)?;
    }
    Ok(())
}

fn formatting(input: ParseStream<'_>, inputs: &mut Inputs) -> syn::Result<()> {
    input.parse::<LitStr>()?;
    comma(input)?;
    expressions(input, inputs)
}

fn assertion(input: ParseStream<'_>, inputs: &mut Inputs, count: usize) -> syn::Result<()> {
    for index in 0..count {
        if index > 0 {
            input.parse::<Token![,]>()?;
        }
        inputs.expressions.push(input.parse()?);
    }
    if !input.is_empty() {
        input.parse::<Token![,]>()?;
        if !input.is_empty() {
            formatting(input, inputs)?;
        }
    }
    Ok(())
}

fn matching(input: ParseStream<'_>, inputs: &mut Inputs) -> syn::Result<()> {
    inputs.expressions.push(input.parse()?);
    input.parse::<Token![,]>()?;
    inputs
        .patterns
        .push(Pat::parse_multi_with_leading_vert(input)?);
    if input.peek(Token![if]) {
        input.parse::<Token![if]>()?;
        inputs.expressions.push(input.parse()?);
    }
    if !input.is_empty() {
        input.parse::<Token![,]>()?;
    }
    Ok(())
}

fn json_value(input: ParseStream<'_>, inputs: &mut Inputs) -> syn::Result<()> {
    if input.peek(syn::token::Brace) {
        let content;
        syn::braced!(content in input);
        while !content.is_empty() {
            inputs.expressions.push(content.parse()?);
            content.parse::<Token![:]>()?;
            json_value(&content, inputs)?;
            comma(&content)?;
        }
    } else if input.peek(syn::token::Bracket) {
        let content;
        syn::bracketed!(content in input);
        while !content.is_empty() {
            json_value(&content, inputs)?;
            comma(&content)?;
        }
    } else if input.peek(syn::Ident) && input.fork().parse::<syn::Ident>()? == "null" {
        input.parse::<syn::Ident>()?;
    } else {
        inputs.expressions.push(input.parse()?);
    }
    Ok(())
}
