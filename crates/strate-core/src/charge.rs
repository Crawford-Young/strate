//! Agent names and charges: what a name says an agent works on. A port of
//! claude-config's grammar (`scripts/lib.mjs`, `parseAgentName` and
//! `chargeKey`); the two must read every name the same way.
//!
//! ```text
//! name   = [<role>_]<kind>_<target>      no role: a charge key (form charge)
//! kind   = repo | epic | issue | pr | topic
//! target = <repo>                         kind repo
//!        | <repo>-<N>                     kind epic | issue | pr (the LAST -<digits> is N)
//!        | <slug>                         kind topic
//! slug   = [a-z0-9]+(-[a-z0-9]+)*         N = [1-9][0-9]*
//! ```
//!
//! Parsing trims and case-folds, tries that grammar first, then the legacy
//! forms, which map to the same charge (K >= 2 in round and dedupe
//! suffixes):
//!
//! | legacy form | form | role | charge |
//! |---|---|---|---|
//! | `ticket-<r>-<N>[-review\|-recon][-r<K>][-<K>]` (shortest `<r>`) | typed | the suffix | issue r#N |
//! | `epic-<r>-<N>…` (same suffixes) | typed | the suffix | epic r#N |
//! | `director-<s>` | typed | director | topic s |
//! | `general-<s>` | typed | none | topic s |
//! | `<dir>_<repo>[_e<N>][_t<N>][_<impl\|review\|recon>[-r<K>]]` | tree | the worker | issue repo#tN, else epic repo#eN, else repo |
//! | `<slug>-<N>` | bare | none | issue slug#N |
//!
//! No regex: each form is matched on its `_` and `-` separated parts, which
//! is exact here because no slug holds either separator's other use.

use serde::Serialize;

/// What a charge is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChargeKind {
    Repo,
    Epic,
    Issue,
    Pr,
    Topic,
}

impl ChargeKind {
    /// Every kind, in the grammar's order.
    pub const ALL: [ChargeKind; 5] = [
        ChargeKind::Repo,
        ChargeKind::Epic,
        ChargeKind::Issue,
        ChargeKind::Pr,
        ChargeKind::Topic,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ChargeKind::Repo => "repo",
            ChargeKind::Epic => "epic",
            ChargeKind::Issue => "issue",
            ChargeKind::Pr => "pr",
            ChargeKind::Topic => "topic",
        }
    }

    /// The kind spelled `s`, if any.
    pub fn parse(s: &str) -> Option<ChargeKind> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// What an agent works on: a repo, an epic, issue or PR of a repo, or a
/// topic. `repo` and `number` are set as the kind needs; `topic` only for a
/// topic.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Charge {
    pub kind: ChargeKind,
    pub repo: Option<String>,
    pub number: Option<u64>,
    pub topic: Option<String>,
}

impl Charge {
    /// `<kind>_<target>`: the join key every form of one charge shares
    /// (lib.mjs `keyOf`).
    pub fn key(&self) -> String {
        let target = self.topic.as_deref().or(self.repo.as_deref()).unwrap_or("");
        match self.number {
            Some(n) if n > 0 => format!("{}_{target}-{n}", self.kind.as_str()),
            _ => format!("{}_{target}", self.kind.as_str()),
        }
    }

    fn repo(repo: &str) -> Charge {
        Charge {
            kind: ChargeKind::Repo,
            repo: Some(repo.to_string()),
            number: None,
            topic: None,
        }
    }

    fn topic(topic: &str) -> Charge {
        Charge {
            kind: ChargeKind::Topic,
            repo: None,
            number: None,
            topic: Some(topic.to_string()),
        }
    }

    fn numbered(kind: ChargeKind, repo: &str, number: u64) -> Charge {
        Charge {
            kind,
            repo: Some(repo.to_string()),
            number: Some(number),
            topic: None,
        }
    }
}

/// Which grammar a name was read by: `Agent` (role and charge) and `Charge`
/// (a bare charge key) are the current one; `Typed`, `Tree` and `Bare` are
/// legacy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NameForm {
    Agent,
    Charge,
    Typed,
    Tree,
    Bare,
}

/// A parsed name: its role (a free label, `None` when it names none), its
/// charge and that charge's key, and the form it was read by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedName {
    pub role: Option<String>,
    pub charge: Charge,
    pub key: String,
    pub form: NameForm,
}

/// Parses `input`, trimmed and lowercased. `None` when it is empty or no
/// form reads it.
pub fn parse_agent_name(input: &str) -> Option<ParsedName> {
    let s = js_trim(input).to_lowercase();
    let out = |form, role: Option<&str>, charge: Charge| ParsedName {
        role: role.map(str::to_string),
        key: charge.key(),
        charge,
        form,
    };
    if let Some((role, charge)) = agent(&s) {
        return Some(match role {
            Some(role) => out(NameForm::Agent, Some(role), charge),
            None => out(NameForm::Charge, None, charge),
        });
    }
    if let Some((role, charge)) = typed(&s) {
        return Some(out(NameForm::Typed, role, charge));
    }
    if let Some((role, charge)) = tree(&s) {
        return Some(out(NameForm::Tree, role, charge));
    }
    numbered(&s).map(|(repo, n)| {
        out(
            NameForm::Bare,
            None,
            Charge::numbered(ChargeKind::Issue, repo, n),
        )
    })
}

/// The grouping key of a name or title: its charge key when it parses, else
/// the trimmed title, else `None` for an empty one (lib.mjs `chargeKey`).
pub fn charge_key(title: &str) -> Option<String> {
    parse_agent_name(title).map(|p| p.key).or_else(|| {
        let t = js_trim(title);
        (!t.is_empty()).then(|| t.to_string())
    })
}

/// `s` trimmed as JS `String.prototype.trim` does: WhiteSpace (the BOM
/// included) and LineTerminator, which is Rust's whitespace plus U+FEFF
/// minus U+0085.
fn js_trim(s: &str) -> &str {
    s.trim_matches(|c: char| (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}')
}

/// A role and charge read by the current grammar.
type Read<'a> = (Option<&'a str>, Charge);

/// `[<role>_]<kind>_<target>`, when the target fits the kind.
fn agent(s: &str) -> Option<Read<'_>> {
    let parts: Vec<&str> = s.split('_').collect();
    let (role, kind, target) = match parts.as_slice() {
        [kind, target] => (None, *kind, *target),
        [role, kind, target] if slug(role) => (Some(*role), *kind, *target),
        _ => return None,
    };
    let kind = ChargeKind::parse(kind)?;
    if !slug(target) {
        return None;
    }
    let charge = match kind {
        ChargeKind::Repo => Charge::repo(target),
        ChargeKind::Topic => Charge::topic(target),
        _ => {
            let (repo, n) = numbered(target)?;
            Charge::numbered(kind, repo, n)
        }
    };
    Some((role, charge))
}

/// `director-<slug>`, `general-<slug>`, or `ticket-`/`epic-` with a repo
/// (the fewest parts that leave a valid suffix), its number and the
/// optional role, round and dedupe suffixes.
fn typed(s: &str) -> Option<Read<'_>> {
    let (prefix, rest) = s.split_once('-')?;
    match prefix {
        "director" => slug(rest).then(|| (Some("director"), Charge::topic(rest))),
        "general" => slug(rest).then(|| (None, Charge::topic(rest))),
        "ticket" | "epic" => {
            if !slug(rest) {
                return None;
            }
            let kind = match prefix {
                "ticket" => ChargeKind::Issue,
                _ => ChargeKind::Epic,
            };
            let parts: Vec<&str> = rest.split('-').collect();
            (1..parts.len()).find_map(|at| {
                let (n, role) = typed_suffix(&parts[at..])?;
                Some((role, Charge::numbered(kind, &parts[..at].join("-"), n)))
            })
        }
        _ => None,
    }
}

/// `<N>[-review|-recon][-r<K>][-<K>]` as `-` parts: the number and role.
fn typed_suffix<'a>(parts: &[&'a str]) -> Option<(u64, Option<&'a str>)> {
    let (first, mut rest) = parts.split_first()?;
    let n = number(first)?;
    let mut role = None;
    if let [r @ ("review" | "recon"), tail @ ..] = rest {
        role = Some(*r);
        rest = tail;
    }
    if let [r, tail @ ..] = rest
        && r.strip_prefix('r').is_some_and(count)
    {
        rest = tail;
    }
    if let [k, tail @ ..] = rest
        && count(k)
    {
        rest = tail;
    }
    rest.is_empty().then_some((n, role))
}

/// `<dir>_<repo>[_e<N>][_t<N>][_<worker>[-r<K>]]`.
fn tree(s: &str) -> Option<Read<'_>> {
    let parts: Vec<&str> = s.split('_').collect();
    let [dir, repo, tail @ ..] = parts.as_slice() else {
        return None;
    };
    let mut rest: &[&str] = tail;
    if !slug(dir) || !slug(repo) {
        return None;
    }
    let tagged = |part: &str, tag: char| part.strip_prefix(tag).and_then(number);
    let mut epic = None;
    if let [e, tail @ ..] = rest
        && let Some(n) = tagged(e, 'e')
    {
        epic = Some(n);
        rest = tail;
    }
    let mut ticket = None;
    if let [t, tail @ ..] = rest
        && let Some(n) = tagged(t, 't')
    {
        ticket = Some(n);
        rest = tail;
    }
    let mut worker = None;
    if let &[w, ref tail @ ..] = rest {
        let (name, round) = match w.split_once('-') {
            Some((name, round)) => (name, Some(round)),
            None => (w, None),
        };
        let round_ok = round.is_none_or(|r| r.strip_prefix('r').is_some_and(count));
        if !matches!(name, "impl" | "review" | "recon") || !round_ok {
            return None;
        }
        worker = Some(name);
        rest = tail;
    }
    if !rest.is_empty() {
        return None;
    }
    let charge = match (ticket, epic) {
        (Some(t), _) => Charge::numbered(ChargeKind::Issue, repo, t),
        (None, Some(e)) => Charge::numbered(ChargeKind::Epic, repo, e),
        (None, None) => Charge::repo(repo),
    };
    Some((worker, charge))
}

/// `<slug>-<N>`, the last `-` part being N: the repo and N.
fn numbered(s: &str) -> Option<(&str, u64)> {
    let (repo, n) = s.rsplit_once('-')?;
    slug(repo).then_some((repo, number(n)?))
}

/// `[a-z0-9]+(-[a-z0-9]+)*`.
fn slug(s: &str) -> bool {
    s.split('-').all(|p| {
        !p.is_empty()
            && p.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    })
}

/// `[1-9][0-9]*`, as a number.
fn number(s: &str) -> Option<u64> {
    let digits = !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    (digits && !s.starts_with('0')).then(|| s.parse().ok())?
}

/// A round or dedupe number: `[2-9]|[1-9][0-9]+`, so 2 and up.
fn count(s: &str) -> bool {
    let digits = !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    digits && !s.starts_with('0') && (s.len() > 1 || s >= "2")
}
