//! Inspect opaque macro inputs for hidden source expansion and suppression tokens.
use proc_macro2::{TokenStream, TokenTree};

pub fn hidden_source(tokens: TokenStream) -> bool {
    let trees: Vec<_> = tokens.into_iter().collect();
    for (index, tree) in trees.iter().enumerate() {
        if let TokenTree::Group(group) = tree
            && hidden_source(group.stream())
        {
            return true;
        }
        if let TokenTree::Ident(ident) = tree {
            let name = ident.to_string();
            if ["include", "macro_rules"].contains(&name.as_str())
                || (["allow", "expect"].contains(&name.as_str())
                    && !matches!(index.checked_sub(1).and_then(|i| trees.get(i)), Some(TokenTree::Punct(p)) if p.as_char()=='.')
                    && matches!(trees.get(index + 1), Some(TokenTree::Group(_))))
            {
                return true;
            }
            if matches!(trees.get(index+1), Some(TokenTree::Punct(p)) if p.as_char()=='!' && p.spacing()!=proc_macro2::Spacing::Joint)
                && ![
                    "format",
                    "format_args",
                    "vec",
                    "matches",
                    "env",
                    "option_env",
                    "json",
                    "params",
                    "assert",
                    "assert_eq",
                    "assert_ne",
                    "panic",
                    "unreachable",
                    "println",
                    "eprintln",
                    "write",
                    "writeln",
                ]
                .contains(&name.as_str())
            {
                return true;
            }
        }
    }
    false
}
