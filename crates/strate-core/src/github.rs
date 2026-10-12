//! GitHub issues, epics and PRs through the `gh` CLI, cached in the store.
//!
//! [`refresh`] fetches each repo's issues with one paginated
//! `gh api graphql` query and replaces that repo's cached rows. A failure
//! (no `gh`, not logged in, rate limit, bad JSON) is recorded on the repo
//! and leaves the old rows alone; nothing here panics or errors out for it.
//! The command runner is injected, so tests never spawn the real CLI.
//! Epics are issues with sub-issues: [`Issue::sub_total`] and
//! [`Issue::parent`] carry the tree.

use std::ffi::OsString;
use std::io::{self, Read};
use std::panic::resume_unwind;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::store::Store;

/// Runs `gh` with the given arguments and returns its stdout.
pub type Runner = Box<dyn FnMut(&[String]) -> io::Result<Vec<u8>> + Send>;

/// A PR that closes an issue.
#[derive(Debug, Clone, PartialEq)]
pub struct Pr {
    pub number: i64,
    pub issue: i64,
    /// `OPEN`, `CLOSED` or `MERGED`.
    pub state: String,
    pub created_at: String,
    pub merged_at: Option<String>,
    pub url: String,
}

/// An issue; with `sub_total > 0` it is an epic over its sub-issues, whose
/// `parent` is its number.
#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    pub number: i64,
    pub title: String,
    /// `OPEN` or `CLOSED`.
    pub state: String,
    pub created_at: String,
    pub closed_at: Option<String>,
    pub parent: Option<i64>,
    pub sub_total: i64,
    pub sub_completed: i64,
    pub prs: Vec<Pr>,
}

const QUERY: &str = "query($owner: String!, $name: String!, $after: String) {
  repository(owner: $owner, name: $name) {
    issues(first: 100, after: $after) {
      pageInfo { hasNextPage endCursor }
      nodes {
        number title state createdAt closedAt
        parent { number }
        subIssuesSummary { total completed }
        closedByPullRequestsReferences(first: 10, includeClosedPrs: true) {
          nodes { number state createdAt mergedAt url }
        }
      }
    }
  }
}";

/// Pages fetched per repo before giving up: 5000 issues.
const MAX_PAGES: usize = 50;

/// Longest a single `gh` run may take before it is killed.
const RUN_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
struct Response {
    data: Option<Data>,
    errors: Option<Vec<ApiError>>,
}

#[derive(Deserialize)]
struct ApiError {
    message: String,
}

#[derive(Deserialize)]
struct Data {
    repository: Option<Repository>,
}

#[derive(Deserialize)]
struct Repository {
    issues: IssuePage,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IssuePage {
    page_info: PageInfo,
    nodes: Vec<IssueNode>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IssueNode {
    number: i64,
    title: String,
    state: String,
    created_at: String,
    closed_at: Option<String>,
    parent: Option<Number>,
    sub_issues_summary: Option<SubIssues>,
    closed_by_pull_requests_references: Option<PrNodes>,
}

#[derive(Deserialize)]
struct Number {
    number: i64,
}

#[derive(Deserialize)]
struct SubIssues {
    total: i64,
    completed: i64,
}

#[derive(Deserialize)]
struct PrNodes {
    nodes: Vec<PrNode>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrNode {
    number: i64,
    state: String,
    created_at: String,
    merged_at: Option<String>,
    url: String,
}

impl From<IssueNode> for Issue {
    fn from(node: IssueNode) -> Self {
        let issue = node.number;
        let (sub_total, sub_completed) = node
            .sub_issues_summary
            .map_or((0, 0), |s| (s.total, s.completed));
        let prs = node
            .closed_by_pull_requests_references
            .map_or_else(Vec::new, |refs| refs.nodes)
            .into_iter()
            .map(|pr| Pr {
                number: pr.number,
                issue,
                state: pr.state,
                created_at: pr.created_at,
                merged_at: pr.merged_at,
                url: pr.url,
            })
            .collect();
        Self {
            number: issue,
            title: node.title,
            state: node.state,
            created_at: node.created_at,
            closed_at: node.closed_at,
            parent: node.parent.map(|p| p.number),
            sub_total,
            sub_completed,
            prs,
        }
    }
}

/// The `gh` binary: `gh` on `path` (the `PATH` value). Pure: the caller
/// passes the env.
pub fn resolve_gh(path: Option<OsString>) -> Option<PathBuf> {
    let name = if cfg!(windows) { "gh.exe" } else { "gh" };
    path.iter()
        .flat_map(std::env::split_paths)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// A [`Runner`] that spawns `program`, killing a run that outlasts 30 s. A
/// non-zero exit is an error carrying the start of its stderr.
pub fn cli_runner(program: PathBuf) -> Runner {
    Box::new(move |args| run(command(&program, args), RUN_TIMEOUT))
}

fn command(program: &Path, args: &[String]) -> Command {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: no console flashes up.
        command.creation_flags(0x0800_0000);
    }
    command
}

/// Drained on its own thread so a full pipe cannot stall the child.
fn drain(mut pipe: impl Read + Send + 'static) -> JoinHandle<io::Result<Vec<u8>>> {
    thread::spawn(move || {
        let mut out = Vec::new();
        pipe.read_to_end(&mut out).map(|_| out)
    })
}

fn run(mut command: Command, timeout: Duration) -> io::Result<Vec<u8>> {
    let mut child = command.spawn()?;
    let stdout = drain(child.stdout.take().expect("stdout is piped"));
    let stderr = drain(child.stderr.take().expect("stderr is piped"));
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("no answer within {timeout:?}"),
            ));
        }
        thread::sleep(Duration::from_millis(5));
    };
    let join = |reader: JoinHandle<io::Result<Vec<u8>>>| {
        reader.join().unwrap_or_else(|panic| resume_unwind(panic))
    };
    let (out, err) = (join(stdout)?, join(stderr)?);
    if status.success() {
        Ok(out)
    } else {
        let err = String::from_utf8_lossy(&err);
        let err: String = err.trim().chars().take(300).collect();
        Err(io::Error::other(format!("exited with {status}: {err}")))
    }
}

/// All issues of `owner/name`, following the query's cursor page by page.
/// Errors (as text for the repo's `error` column): the runner failing, a
/// malformed or error response, a missing repo, or more than 50 pages.
pub fn fetch_repo(runner: &mut Runner, owner: &str, name: &str) -> Result<Vec<Issue>, String> {
    let mut issues = Vec::new();
    let mut after: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut args: Vec<String> = ["api", "graphql", "-f"].map(String::from).into();
        args.push(format!("query={QUERY}"));
        for (key, value) in [("owner", owner), ("name", name)] {
            args.extend(["-f".into(), format!("{key}={value}")]);
        }
        if let Some(cursor) = &after {
            args.extend(["-f".into(), format!("after={cursor}")]);
        }
        let out = runner(&args).map_err(|e| format!("gh: {e}"))?;
        let response: Response =
            serde_json::from_slice(&out).map_err(|e| format!("bad gh response: {e}"))?;
        if let Some(first) = response.errors.as_deref().and_then(<[_]>::first) {
            return Err(format!("GitHub: {}", first.message));
        }
        let page = response
            .data
            .and_then(|d| d.repository)
            .ok_or_else(|| format!("GitHub: no repository {owner}/{name}"))?
            .issues;
        issues.extend(page.nodes.into_iter().map(Issue::from));
        match page.page_info {
            PageInfo {
                has_next_page: true,
                end_cursor: Some(cursor),
            } => after = Some(cursor),
            _ => return Ok(issues),
        }
    }
    Err(format!("more than {MAX_PAGES} pages of issues"))
}

#[derive(Deserialize)]
struct User {
    login: String,
}

/// The authenticated user's login, through `gh api user`.
fn login(runner: &mut Runner) -> Result<String, String> {
    let args = ["api", "user"].map(String::from);
    let out = runner(&args).map_err(|e| format!("gh: {e}"))?;
    let user: User = serde_json::from_slice(&out).map_err(|e| format!("bad gh response: {e}"))?;
    Ok(user.login)
}

/// Refreshes the cache of every repo slug in `slugs`: each is fetched under
/// its owner (a stored `github_links` repository naming it, else the `gh`
/// login, asked for once) and its rows replaced at `now_ms`. A repo that
/// fails keeps its old rows and gets the error recorded. Returns an error
/// only when the store itself fails.
pub fn refresh(
    store: &mut Store,
    runner: &mut Runner,
    slugs: &[&str],
    now_ms: i64,
) -> rusqlite::Result<()> {
    let mut fallback: Option<Result<String, String>> = None;
    for &slug in slugs {
        let owner = match store.gh_owner_hint(slug)? {
            Some(owner) => Ok(owner),
            None => fallback.get_or_insert_with(|| login(runner)).clone(),
        };
        let fetched = owner
            .clone()
            .and_then(|owner| fetch_repo(runner, &owner, slug).map(|issues| (owner, issues)));
        match fetched {
            Ok((owner, issues)) => store.gh_replace(slug, &owner, &issues, now_ms)?,
            Err(error) => store.gh_record_error(slug, owner.as_deref().ok(), &error)?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tail::test_dir;

    #[test]
    fn gh_is_found_on_the_given_path_only() {
        let dir = test_dir("gh-resolve");
        let name = if cfg!(windows) { "gh.exe" } else { "gh" };
        assert_eq!(resolve_gh(Some(dir.clone().into_os_string())), None);
        std::fs::write(dir.join(name), b"").expect("fake gh");
        assert_eq!(
            resolve_gh(Some(dir.clone().into_os_string())),
            Some(dir.join(name))
        );
        assert_eq!(resolve_gh(None), None);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }
}
