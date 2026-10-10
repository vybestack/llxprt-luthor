use std::collections::{BTreeMap, BTreeSet};

use proc_macro2::{Span, TokenStream, TokenTree};
use quote::ToTokens;
use syn::visit::{self, Visit};

use crate::measurement::Measurement;

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub file_lines: usize,
    pub function_lines: usize,
    pub cyclomatic: usize,
    pub cognitive: usize,
    pub type_lines: usize,
    pub type_methods: usize,
    pub module_lines: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            file_lines: 800,
            function_lines: 80,
            cyclomatic: 25,
            cognitive: 30,
            type_lines: 400,
            type_methods: 20,
            module_lines: 600,
        }
    }
}

#[derive(Debug)]
pub struct Function {
    pub symbol: String,
    pub line: usize,
    pub lines: usize,
    pub cyclomatic: usize,
    pub cognitive: usize,
}

#[derive(Debug, Default)]
pub struct Report {
    pub path: String,
    pub file_lines: usize,
    pub functions: Vec<Function>,
    pub types: BTreeMap<String, (usize, usize)>,
    pub macro_types: BTreeSet<String>,
    pub modules: BTreeMap<String, usize>,
}

pub fn effective_lines(tokens: TokenStream) -> usize {
    token_lines(tokens, &BTreeSet::new())
}

type TokenPosition = ((usize, usize), (usize, usize));

fn position(span: Span) -> TokenPosition {
    let start = span.start();
    let end = span.end();
    ((start.line, start.column), (end.line, end.column))
}

fn token_lines(tokens: TokenStream, attributes: &BTreeSet<TokenPosition>) -> usize {
    let mut lines = BTreeSet::new();
    visit_spans(tokens, &mut |span| {
        if !attributes.contains(&position(span)) {
            lines.extend(span.start().line..=span.end().line);
        }
    });
    lines.len()
}

fn visit_spans(tokens: TokenStream, visitor: &mut impl FnMut(Span)) {
    for token in tokens {
        match token {
            TokenTree::Group(group) => {
                visitor(group.span_open());
                visit_spans(group.stream(), visitor);
                visitor(group.span_close());
            }
            TokenTree::Literal(literal) => visitor(literal.span()),
            TokenTree::Ident(ident) => visitor(ident.span()),
            TokenTree::Punct(punct) => visitor(punct.span()),
        }
    }
}

pub fn analyze(path: &str, source: &str) -> Result<Report, String> {
    let syntax = syn::parse_file(source).map_err(|e| format!("{path}: parse error: {e}"))?;
    crate::attribute_validation::standalone(path, &syntax)?;
    analyze_validated(path, source)
}

pub(crate) fn analyze_validated(path: &str, source: &str) -> Result<Report, String> {
    let syntax = syn::parse_file(source).map_err(|e| format!("{path}: parse error: {e}"))?;
    crate::macro_policy::validate_bindings(path, &syntax)?;
    let mut collector = Collector {
        report: Report {
            path: path.into(),
            file_lines: 0,
            ..Report::default()
        },
        module: "root".into(),
        owner: None,
        errors: Vec::new(),
        macro_depth: 0,
        attributes: BTreeSet::new(),
    };
    collector.visit_file(&syntax);
    if collector.errors.is_empty() {
        collector.report.file_lines = token_lines(syntax.to_token_stream(), &collector.attributes);
        Ok(collector.report)
    } else {
        Err(collector.errors.join("\n"))
    }
}

struct Collector {
    report: Report,
    module: String,
    owner: Option<String>,
    errors: Vec<String>,
    macro_depth: usize,
    attributes: BTreeSet<TokenPosition>,
}

impl Collector {
    fn function(&mut self, sig: &syn::Signature, block: &syn::Block, tokens: TokenStream) {
        let _ = tokens;
        let lines = effective_lines(::quote::quote!(#sig #block));
        let mut complexity = Complexity {
            path: self.report.path.clone(),
            ..Complexity::default()
        };
        complexity.visit_block(block);
        self.errors.extend(complexity.errors);
        let symbol = format!(
            "{}::{}",
            self.owner.as_ref().unwrap_or(&self.module),
            sig.ident
        );
        self.report.functions.push(Function {
            symbol,
            line: sig.ident.span().start().line,
            lines,
            cyclomatic: complexity.branches + 1,
            cognitive: complexity.cognitive,
        });
        if let Some(owner) = &self.owner {
            let aggregate = self.report.types.entry(owner.clone()).or_default();
            aggregate.0 += lines;
            aggregate.1 += 1;
        } else {
            *self.report.modules.entry(self.module.clone()).or_default() += lines;
        }
    }
}

impl<'ast> Visit<'ast> for Collector {
    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        self.function(&item.sig, &item.block, item.to_token_stream());
        visit::visit_item_fn(self, item);
    }
    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        self.function(&item.sig, &item.block, item.to_token_stream());
        visit::visit_impl_item_fn(self, item);
    }
    fn visit_trait_item_fn(&mut self, item: &'ast syn::TraitItemFn) {
        if let Some(block) = &item.default {
            self.function(&item.sig, block, item.to_token_stream());
        }
        visit::visit_trait_item_fn(self, item);
    }
    fn visit_item_trait(&mut self, item: &'ast syn::ItemTrait) {
        let old = self.owner.take();
        let owner = format!("{}::{}", self.module, item.ident);
        if self.macro_depth > 0 {
            self.report.macro_types.insert(owner.clone());
        }
        self.owner = Some(owner);
        visit::visit_item_trait(self, item);
        self.owner = old;
    }
    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        let old = self.owner.take();
        let owner = format!("{}::{}", self.module, type_identity(&item.self_ty));
        if self.macro_depth > 0 {
            self.report.macro_types.insert(owner.clone());
        }
        self.owner = Some(owner);
        visit::visit_item_impl(self, item);
        self.owner = old;
    }
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        let old = self.module.clone();
        self.module = format!("{}::{}", old, item.ident);
        visit::visit_item_mod(self, item);
        self.module = old;
    }
    fn visit_attribute(&mut self, attribute: &'ast syn::Attribute) {
        if self.macro_depth > 0 {
            self.errors.extend(crate::suppression::attribute_findings(
                &self.report.path,
                attribute,
            ));
        }
        // Attributes have already passed binding, context and input validation.
        // Match token positions, not entire lines: adjacent code must still count.
        visit_spans(attribute.to_token_stream(), &mut |span| {
            self.attributes.insert(position(span));
        });
        visit::visit_attribute(self, attribute);
    }
    fn visit_item_macro(&mut self, item: &'ast syn::ItemMacro) {
        self.errors.push(format!(
            "{}:{}: unsupported item macro {} (expansion required)",
            self.report.path,
            item.mac.path.segments[0].ident.span().start().line,
            item.mac.path.to_token_stream()
        ));
    }
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        let path = self.report.path.clone();
        self.macro_depth += 1;
        if let Err(error) = crate::macro_policy::visit_inputs(self, mac, &path) {
            self.errors.push(error);
        }
        self.macro_depth -= 1;
    }
}

#[derive(Default)]
struct Complexity {
    path: String,
    errors: Vec<String>,
    branches: usize,
    cognitive: usize,
    depth: usize,
}

impl Complexity {
    fn branch(&mut self) {
        self.branches += 1;
        self.cognitive += 1 + self.depth;
    }
}

impl<'ast> Visit<'ast> for Complexity {
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        let path = self.path.clone();
        if let Err(error) = crate::macro_policy::visit_inputs(self, mac, &path) {
            self.errors.push(error);
        }
    }
    fn visit_expr_if(&mut self, expression: &'ast syn::ExprIf) {
        self.branch();
        if expression.else_branch.is_some() {
            self.cognitive += 1;
        }
        self.depth += 1;
        visit::visit_expr_if(self, expression);
        self.depth -= 1;
    }
    fn visit_expr_match(&mut self, expression: &'ast syn::ExprMatch) {
        self.cognitive += 1 + self.depth;
        self.branches += expression.arms.len().saturating_sub(1);
        self.depth += 1;
        visit::visit_expr_match(self, expression);
        self.depth -= 1;
    }
    fn visit_expr_for_loop(&mut self, expression: &'ast syn::ExprForLoop) {
        self.branch();
        self.depth += 1;
        visit::visit_expr_for_loop(self, expression);
        self.depth -= 1;
    }
    fn visit_expr_while(&mut self, expression: &'ast syn::ExprWhile) {
        self.branch();
        self.depth += 1;
        visit::visit_expr_while(self, expression);
        self.depth -= 1;
    }
    fn visit_expr_loop(&mut self, expression: &'ast syn::ExprLoop) {
        self.branch();
        self.depth += 1;
        visit::visit_expr_loop(self, expression);
        self.depth -= 1;
    }
    fn visit_expr_binary(&mut self, expression: &'ast syn::ExprBinary) {
        if matches!(expression.op, syn::BinOp::And(_) | syn::BinOp::Or(_)) {
            self.branches += 1;
            self.cognitive += 1;
        }
        visit::visit_expr_binary(self, expression);
    }
    fn visit_expr_try(&mut self, expression: &'ast syn::ExprTry) {
        self.branches += 1;
        visit::visit_expr_try(self, expression);
    }
}

impl Report {
    pub fn measurements(&self, limits: Limits) -> Vec<Measurement> {
        let mut output =
            vec![self.measure("file", "file_lines", self.file_lines, limits.file_lines)];
        for f in &self.functions {
            for (metric, value, limit) in [
                ("function_lines", f.lines, limits.function_lines),
                ("cyclomatic", f.cyclomatic, limits.cyclomatic),
                ("cognitive", f.cognitive, limits.cognitive),
            ] {
                output.push(self.measure(&f.symbol, metric, value, limit));
            }
        }
        for (symbol, (lines, methods)) in &self.types {
            output.push(self.measure(symbol, "type_lines", *lines, limits.type_lines));
            output.push(self.measure(symbol, "type_methods", *methods, limits.type_methods));
        }
        for (symbol, lines) in &self.modules {
            output.push(self.measure(symbol, "module_lines", *lines, limits.module_lines));
        }
        output
    }
    fn measure(&self, symbol: &str, metric: &str, value: usize, limit: usize) -> Measurement {
        Measurement {
            key: format!("{}::{symbol}:{metric}", self.path),
            value,
            limit,
        }
    }
    pub fn violations(&self, limits: Limits) -> Vec<String> {
        self.measurements(limits)
            .into_iter()
            .filter(|m| m.value > m.limit)
            .map(|m| m.diagnostic())
            .collect()
    }
}

pub fn type_identity(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Path(p) => p
            .path
            .segments
            .iter()
            .map(|s| s.ident.to_string())
            .collect::<Vec<_>>()
            .join("::"),
        _ => ty.to_token_stream().to_string(),
    }
}
