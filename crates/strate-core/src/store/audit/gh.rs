//! The GitHub cache as the Audit reads it: issues, their closing PRs, and
//! the repos whose latest refresh failed.

use std::collections::{HashMap, HashSet};

use rusqlite::Connection;

use super::{GithubError, Progress};
use crate::store::Result;
use crate::time::parse_timestamp;

/// The cached GitHub issues and their closing PRs.
pub(crate) struct Gh {
    issues: HashMap<(String, i64), GhIssue>,
    prs: HashMap<(String, i64), Vec<GhPr>>,
    /// The issues that are epics: named as parent by a cached issue of
    /// their repo. A parent in another repo is dropped at fetch, so these
    /// are the epics with a sub-issue in the same repo.
    epics: HashSet<(String, i64)>,
    /// Per PR of a repo (by its own URL), the lowest issue of that repo it
    /// closes.
    closes: HashMap<(String, i64), i64>,
}

pub(crate) struct GhIssue {
    pub(crate) title: String,
    /// `Some` once closed; the time is `None` if GitHub gave none.
    pub(crate) closed: Option<Option<i64>>,
    /// Closed as completed (`stateReason`), not as not planned or a
    /// duplicate: only these count in the headline.
    pub(crate) completed: bool,
    pub(crate) parent: Option<i64>,
    pub(crate) progress: Option<Progress>,
    /// Times reopened; `None` until a refresh since v8 fetched it.
    pub(crate) reopened: Option<i64>,
}

pub(crate) struct GhPr {
    /// The PR's own repo slug, from its URL; else the issue's.
    pub(crate) repo: String,
    pub(crate) number: i64,
    pub(crate) state: String,
    pub(crate) merged_at: Option<i64>,
    pub(crate) url: String,
}

impl Gh {
    pub(crate) fn load(conn: &Connection) -> Result<Self> {
        let at = |s: Option<String>| s.as_deref().and_then(parse_timestamp);
        let mut issues: HashMap<(String, i64), GhIssue> = HashMap::new();
        let mut epics: HashSet<(String, i64)> = HashSet::new();
        let mut stmt = conn.prepare_cached(
            "SELECT repo, number, title, state, closed_at, parent, sub_total, sub_completed,
                 state_reason, reopened
             FROM gh_issues",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let repo: String = r.get("repo")?;
            let number: i64 = r.get("number")?;
            let state: String = r.get("state")?;
            let total: i64 = r.get("sub_total")?;
            let completed: i64 = r.get("sub_completed")?;
            let parent: Option<i64> = r.get("parent")?;
            let closed_at: Option<String> = r.get("closed_at")?;
            // Not `total > 0`: that counts sub-issues in other repos, which
            // never sit under it, so it would be an epic over only its
            // direct work.
            if let Some(p) = parent {
                epics.insert((repo.clone(), p));
            }
            issues.insert(
                (repo, number),
                GhIssue {
                    title: r.get("title")?,
                    closed: (state == "CLOSED").then(|| at(closed_at)),
                    completed: r.get::<_, Option<String>>("state_reason")?.as_deref()
                        == Some("COMPLETED"),
                    parent,
                    progress: (total > 0).then_some(Progress { completed, total }),
                    reopened: r.get("reopened")?,
                },
            );
        }
        let mut prs: HashMap<(String, i64), Vec<GhPr>> = HashMap::new();
        let mut closes: HashMap<(String, i64), i64> = HashMap::new();
        let mut stmt = conn.prepare_cached(
            "SELECT repo, issue, number, state, merged_at, url FROM gh_prs
             ORDER BY repo, issue, number",
        )?;
        for row in stmt.query_map([], |r| {
            let issue_repo: String = r.get("repo")?;
            let url: String = r.get("url")?;
            Ok((
                (issue_repo.clone(), r.get("issue")?),
                GhPr {
                    repo: url_repo(&url).map_or(issue_repo, str::to_string),
                    number: r.get("number")?,
                    state: r.get("state")?,
                    merged_at: at(r.get("merged_at")?),
                    url,
                },
            ))
        })? {
            let (key, pr) = row?;
            if pr.repo == key.0 {
                let issue = closes.entry((pr.repo.clone(), pr.number)).or_insert(key.1);
                *issue = (*issue).min(key.1);
            }
            prs.entry(key).or_default().push(pr);
        }
        Ok(Self {
            issues,
            prs,
            epics,
            closes,
        })
    }

    /// Issue `repo`#`number`, when the cache holds it.
    pub(crate) fn issue(&self, repo: &str, number: i64) -> Option<&GhIssue> {
        self.issues.get(&(repo.to_string(), number))
    }

    /// Whether `repo`#`number` is an epic: a cached issue of the repo names
    /// it as parent.
    pub(crate) fn is_epic(&self, repo: &str, number: i64) -> bool {
        self.epics.contains(&(repo.to_string(), number))
    }

    /// The PRs closing `repo`#`issue`, by number.
    pub(crate) fn prs(&self, repo: &str, issue: i64) -> &[GhPr] {
        self.prs
            .get(&(repo.to_string(), issue))
            .map_or(&[][..], Vec::as_slice)
    }

    /// The lowest issue of `repo` that its PR `number` closes, when the
    /// cache holds one.
    pub(crate) fn closed_by(&self, repo: &str, number: i64) -> Option<i64> {
        self.closes.get(&(repo.to_string(), number)).copied()
    }

    /// PR `number` among those closing `repo`#`issue`.
    pub(crate) fn pr(&self, repo: &str, issue: i64, number: i64) -> Option<&GhPr> {
        self.prs(repo, issue).iter().find(|p| p.number == number)
    }
}

/// The repo name in a `https://github.com/<owner>/<name>/pull/<n>` URL.
fn url_repo(url: &str) -> Option<&str> {
    let path = url.strip_prefix("https://")?;
    match path.split('/').collect::<Vec<_>>()[..] {
        [_host, _owner, name, "pull", _number] => Some(name),
        _ => None,
    }
}

/// Every repo whose latest GitHub refresh failed, whatever the filter.
pub(crate) fn github_errors(conn: &Connection) -> Result<Vec<GithubError>> {
    conn.prepare_cached("SELECT slug, error FROM gh_repos WHERE error IS NOT NULL ORDER BY slug")?
        .query_map([], |r| {
            Ok(GithubError {
                repo: r.get("slug")?,
                error: r.get("error")?,
            })
        })?
        .collect()
}
