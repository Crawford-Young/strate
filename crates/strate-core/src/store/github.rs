//! The GitHub cache: `gh_repos`, `gh_issues`, `gh_prs` and `gh_reverts`, keyed by repo
//! slug (the repo part of a session name).

use std::collections::BTreeMap;

use rusqlite::{Connection, params};

use super::{Result, Store, model};
use crate::github::{Issue, Pr, Repo, Revert};

/// One cached repo: `fetched_at` (unix ms) is its last successful refresh,
/// `error` the failure of the latest one, if it failed.
#[derive(Debug, Clone, PartialEq)]
pub struct GhRepo {
    pub slug: String,
    pub owner: Option<String>,
    pub name: Option<String>,
    pub fetched_at: Option<i64>,
    pub error: Option<String>,
}

impl Store {
    /// The repos of the agents' name charges (a repo, or an epic, issue or
    /// PR of one), sorted and unique.
    pub fn charge_slugs(&self) -> Result<Vec<String>> {
        self.conn
            .prepare_cached(
                "SELECT DISTINCT c.repo FROM agents AS a JOIN charges AS c ON c.id = a.name_charge
                 WHERE c.repo IS NOT NULL ORDER BY c.repo",
            )?
            .query_map([], |r| r.get(0))?
            .collect()
    }

    /// The owner of `slug` as a stored `github_links.pr_repository`
    /// (`owner/name`) names it, newest link first.
    pub fn gh_owner_hint(&self, slug: &str) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT pr_repository FROM github_links WHERE pr_repository LIKE '%/%'
             ORDER BY id DESC",
        )?;
        let repositories = stmt.query_map([], |r| r.get::<_, String>(0))?;
        for repository in repositories {
            if let Some((owner, name)) = repository?.split_once('/')
                && name == slug
            {
                return Ok(Some(owner.to_owned()));
            }
        }
        Ok(None)
    }

    /// Replaces everything cached for `slug` with `repo` (its issues, their
    /// PRs and its reverts) in one transaction, clearing its error and
    /// stamping `fetched_at`.
    pub fn gh_replace(
        &mut self,
        slug: &str,
        owner: &str,
        repo: &Repo,
        fetched_at: i64,
    ) -> Result<()> {
        let tx = self.transaction()?;
        let conn = &tx.tx;
        conn.execute(
            "INSERT INTO gh_repos (slug, owner, name, fetched_at, error)
             VALUES (?1, ?2, ?1, ?3, NULL)
             ON CONFLICT (slug) DO UPDATE SET owner = excluded.owner,
                 name = excluded.name, fetched_at = excluded.fetched_at, error = NULL",
            params![slug, owner, fetched_at],
        )?;
        conn.execute("DELETE FROM gh_reverts WHERE repo = ?1", [slug])?;
        conn.execute("DELETE FROM gh_prs WHERE repo = ?1", [slug])?;
        conn.execute("DELETE FROM gh_issues WHERE repo = ?1", [slug])?;
        let mut insert_issue = conn.prepare(
            "INSERT OR REPLACE INTO gh_issues (repo, number, title, state, created_at,
                 closed_at, parent, sub_total, sub_completed, state_reason, reopened)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )?;
        let mut insert_pr = conn.prepare(
            "INSERT OR REPLACE INTO gh_prs (repo, issue, number, state, created_at,
                 merged_at, url, head_ref)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        let mut insert_revert = conn.prepare(
            "INSERT OR REPLACE INTO gh_reverts (repo, number, reverted, merged_at)
             VALUES (?1, ?2, ?3, ?4)",
        )?;
        for issue in &repo.issues {
            insert_issue.execute(params![
                slug,
                issue.number,
                issue.title,
                issue.state,
                issue.created_at,
                issue.closed_at,
                issue.parent,
                issue.sub_total,
                issue.sub_completed,
                issue.state_reason,
                issue.reopened,
            ])?;
            for pr in &issue.prs {
                insert_pr.execute(params![
                    slug,
                    pr.issue,
                    pr.number,
                    pr.state,
                    pr.created_at,
                    pr.merged_at,
                    pr.url,
                    pr.head_ref,
                ])?;
            }
        }
        for revert in &repo.reverts {
            insert_revert.execute(params![
                slug,
                revert.number,
                revert.reverted,
                revert.merged_at
            ])?;
        }
        drop((insert_issue, insert_pr, insert_revert));
        model::branched(conn)?;
        tx.commit()
    }

    /// Records a failed refresh of `slug`: sets `error` (and `owner`, when
    /// known), leaving the cached rows and `fetched_at` as they were.
    pub fn gh_record_error(&mut self, slug: &str, owner: Option<&str>, error: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO gh_repos (slug, owner, name, error) VALUES (?1, ?2, ?1, ?3)
             ON CONFLICT (slug) DO UPDATE SET owner = coalesce(excluded.owner, owner),
                 error = excluded.error",
            params![slug, owner, error],
        )?;
        Ok(())
    }
}

/// Every cached repo, by slug. A test and probe helper: the app reads the
/// cache through [`crate::store::views::audit`].
pub fn gh_repos(conn: &Connection) -> Result<Vec<GhRepo>> {
    conn.prepare("SELECT slug, owner, name, fetched_at, error FROM gh_repos ORDER BY slug")?
        .query_map([], |r| {
            Ok(GhRepo {
                slug: r.get(0)?,
                owner: r.get(1)?,
                name: r.get(2)?,
                fetched_at: r.get(3)?,
                error: r.get(4)?,
            })
        })?
        .collect()
}

/// The cached issues of `slug` by number, each with its closing PRs. A test
/// and probe helper: the app reads the cache through
/// [`crate::store::views::audit`].
pub fn gh_issues(conn: &Connection, slug: &str) -> Result<Vec<Issue>> {
    let mut prs: BTreeMap<i64, Vec<Pr>> = BTreeMap::new();
    let mut stmt = conn.prepare(
        "SELECT issue, number, state, created_at, merged_at, url, head_ref FROM gh_prs
         WHERE repo = ?1 ORDER BY issue, number",
    )?;
    for pr in stmt.query_map([slug], |r| {
        Ok(Pr {
            issue: r.get(0)?,
            number: r.get(1)?,
            state: r.get(2)?,
            created_at: r.get(3)?,
            merged_at: r.get(4)?,
            url: r.get(5)?,
            head_ref: r.get(6)?,
        })
    })? {
        let pr = pr?;
        prs.entry(pr.issue).or_default().push(pr);
    }
    conn.prepare(
        "SELECT number, title, state, created_at, closed_at, parent, sub_total, sub_completed,
             state_reason, reopened
         FROM gh_issues WHERE repo = ?1 ORDER BY number",
    )?
    .query_map([slug], |r| {
        let number = r.get(0)?;
        Ok(Issue {
            number,
            title: r.get(1)?,
            state: r.get(2)?,
            state_reason: r.get(8)?,
            created_at: r.get(3)?,
            closed_at: r.get(4)?,
            parent: r.get(5)?,
            sub_total: r.get(6)?,
            sub_completed: r.get(7)?,
            reopened: r.get(9)?,
            prs: prs.remove(&number).unwrap_or_default(),
        })
    })?
    .collect()
}

/// The cached reverts of `slug` by number. A test and probe helper.
pub fn gh_reverts(conn: &Connection, slug: &str) -> Result<Vec<Revert>> {
    conn.prepare(
        "SELECT number, reverted, merged_at FROM gh_reverts WHERE repo = ?1 ORDER BY number",
    )?
    .query_map([slug], |r| {
        Ok(Revert {
            number: r.get(0)?,
            reverted: r.get(1)?,
            merged_at: r.get(2)?,
        })
    })?
    .collect()
}
