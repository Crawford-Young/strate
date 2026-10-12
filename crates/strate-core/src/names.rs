//! Session names as the link to GitHub: orchestrator windows are
//! `<repo>-<issue>[-sN]`, agents `<repo>-<issue>-<role>-<task>[-N]`.

/// What a name says about where its work belongs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedName {
    pub repo: String,
    pub issue: u64,
    /// The `N` of a window's `-sN` suffix, when it ends the name.
    pub rotation: Option<u32>,
}

/// Parses `name`: the issue is the first all-digit `-` segment and the repo
/// the segments before it, joined. `None` when there is no such segment or
/// no repo before it (the caller files the work under "Unlinked").
pub fn parse_name(name: &str) -> Option<ParsedName> {
    let segments: Vec<&str> = name.split('-').collect();
    let at = segments.iter().position(|s| all_digits(s))?;
    let repo = segments[..at].join("-");
    if repo.is_empty() {
        return None;
    }
    let issue = segments[at].parse().ok()?;
    let rotation = match segments[at + 1..] {
        [last] => last
            .strip_prefix('s')
            .filter(|n| all_digits(n))
            .and_then(|n| n.parse().ok()),
        _ => None,
    };
    Some(ParsedName {
        repo,
        issue,
        rotation,
    })
}

fn all_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}
