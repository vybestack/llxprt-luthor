//! Closing references derived from the verified tracker and code mapping.
pub(crate) fn closing_reference(tracker: &str, code: &str, issue_number: u64) -> String {
    if tracker == code {
        format!("Fixes #{issue_number}")
    } else {
        format!("Fixes {tracker}#{issue_number}")
    }
}

pub(crate) fn prompt(args: &[String]) -> Option<&str> {
    let mut matches = args
        .windows(2)
        .filter(|pair| matches!(pair[0].as_str(), "-p" | "--prompt"))
        .map(|pair| pair[1].as_str());
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}
