//! Session names as the link to GitHub: orchestrator windows are
//! `<repo>-<issue>[-sN]`, agents `<repo>-<issue>-<role>-<task>[-N]`, either
//! behind claude-config's typed prefix (`scripts/lib.mjs`, `TYPED_RE`):
//! `ticket-` and `epic-` name a repo and issue, while `director-<scope>`
//! and `general-<topic>` belong to no issue.

/// What a name says about where its work belongs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedName {
    pub repo: String,
    pub issue: u64,
    /// The `N` of a window's `-sN` suffix, when it ends the name.
    pub rotation: Option<u32>,
}

/// Parses `name`: past a `ticket-` or `epic-` prefix, the issue is the
/// first all-digit `-` segment and the repo the segments before it, joined.
/// `None` for a `director-` or `general-` name, and when there is no such
/// segment or no repo before it (the caller files the work under
/// "Unlinked"). Prefixes match in any case. A prefix with no repo after it
/// (`ticket-51`) is an untyped name, as lib.mjs's legacy form reads it.
pub fn parse_name(name: &str) -> Option<ParsedName> {
    let (kind, rest) = name.split_once('-').unwrap_or((name, ""));
    let typed = |k: &str| kind.eq_ignore_ascii_case(k) && !rest.is_empty();
    if typed("director") || typed("general") {
        return None;
    }
    if (typed("ticket") || typed("epic"))
        && let Some(parsed) = parse_untyped(rest)
    {
        return Some(parsed);
    }
    parse_untyped(name)
}

/// `<repo>-<issue>` with its `-sN` or agent suffix.
fn parse_untyped(name: &str) -> Option<ParsedName> {
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
