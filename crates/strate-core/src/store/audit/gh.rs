//! The GitHub cache as the Audit reads it: issues, their closing PRs, and
//! the repos whose latest refresh failed.

use std::collections::HashMap;

use rusqlite::Connection;

use super::{GithubError, Progress};
use crate::store::Result;
use crate::time::parse_timestamp;

/// The cached GitHub issues and their closing PRs.
pub(crate) struct Gh {
    issues: HashMap<(String, i64), GhIssue>,
    prs: HashMap<(String, i64), Vec<GhPr>>,
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
        let issues = conn
            .prepare_cached(
                "SELECT repo, number, title, state, closed_at, parent, sub_total, sub_completed,
                     state_reason
                 FROM gh_issues",
            )?
            .query_map([], |r| {
                let state: String = r.get(3)?;
                let closed_at: Option<String> = r.get(4)?;
                let total: i64 = r.get(6)?;
                let completed: i64 = r.get(7)?;
                Ok((
                    (r.get(0)?, r.get(1)?),
                    GhIssue {
                        title: r.get(2)?,
                        closed: (state == "CLOSED").then(|| at(closed_at)),
                        completed: r.get::<_, Option<String>>(8)?.as_deref() == Some("COMPLETED"),
                        parent: r.get(5)?,
                        progress: (total > 0).then_some(Progress { completed, total }),
                    },
                ))
            })?
            .collect::<Result<_>>()?;
        let mut prs: HashMap<(String, i64), Vec<GhPr>> = HashMap::new();
        let mut stmt = conn.prepare_cached(
            "SELECT repo, issue, number, state, merged_at, url FROM gh_prs
             ORDER BY repo, issue, number",
        )?;
        for row in stmt.query_map([], |r| {
            let issue_repo: String = r.get(0)?;
            let url: String = r.get(5)?;
            Ok((
                (issue_repo.clone(), r.get(1)?),
                GhPr {
                    repo: url_repo(&url).map_or(issue_repo, str::to_string),
                    number: r.get(2)?,
                    state: r.get(3)?,
                    merged_at: at(r.get(4)?),
                    url,
                },
            ))
        })? {
            let (key, pr) = row?;
            prs.entry(key).or_default().push(pr);
        }
        Ok(Self { issues, prs })
    }

    /// Issue `repo`#`number`, when the cache holds it.
    pub(crate) fn issue(&self, repo: &str, number: i64) -> Option<&GhIssue> {
        self.issues.get(&(repo.to_string(), number))
    }

    /// The PRs closing `repo`#`issue`, by number.
    pub(crate) fn prs(&self, repo: &str, issue: i64) -> &[GhPr] {
        self.prs
            .get(&(repo.to_string(), issue))
            .map_or(&[][..], Vec::as_slice)
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
                repo: r.get(0)?,
                error: r.get(1)?,
            })
        })?
        .collect()
}
