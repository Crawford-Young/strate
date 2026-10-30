//! GitHub issues, epics and PRs through the `gh` CLI, cached in the store.
//!
//! [`fetch_all`] fetches each repo's issues with one paginated
//! `gh api graphql` query, and [`store_fetched`] replaces that repo's cached
//! rows. A failure (no `gh`, not logged in, rate limit, bad JSON) is
//! recorded on the repo and leaves the old rows alone; nothing here panics
//! or errors out for it.
//! The command runner is injected, so tests never spawn the real CLI.
//! Epics are issues with sub-issues in the same repo: [`Issue::sub_total`]
//! and [`Issue::parent`] carry the tree. The first page also carries the
//! repo's merged revert PRs ([`Revert`]), so a repo costs no extra call.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::Deserialize;

use crate::proc::{self, run};
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
    /// Its head branch; `None` when not fetched (a row cached before v21,
    /// until the next refresh).
    pub head_ref: Option<String>,
}

/// An issue; with `sub_total > 0` it is an epic over its sub-issues, whose
/// `parent` is its number.
#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    pub number: i64,
    pub title: String,
    /// `OPEN` or `CLOSED`.
    pub state: String,
    /// Why it closed: `COMPLETED`, `NOT_PLANNED` or `DUPLICATE`; `None` while
    /// open.
    pub state_reason: Option<String>,
    pub created_at: String,
    pub closed_at: Option<String>,
    /// The parent issue in the same repo; a parent in another repo is not
    /// kept, so the issue has no epic.
    pub parent: Option<i64>,
    pub sub_total: i64,
    pub sub_completed: i64,
    /// How many times it was reopened; `None` when not fetched (a row
    /// cached before v8, until the next refresh).
    pub reopened: Option<i64>,
    pub prs: Vec<Pr>,
}

/// A merged PR with `revert` in its title.
#[derive(Debug, Clone, PartialEq)]
pub struct Revert {
    pub number: i64,
    /// The PR it reverts, from a `Reverts owner/name#N` (or `Reverts #N`)
    /// line in its body; `None` for a commit revert or one in another repo.
    pub reverted: Option<i64>,
    pub merged_at: String,
}

/// One repo as fetched: every issue, and up to 100 merged revert PRs.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Repo {
    pub issues: Vec<Issue>,
    pub reverts: Vec<Revert>,
}

const QUERY: &str = "query($owner: String!, $name: String!, $after: String,
    $revertSearch: String!, $withReverts: Boolean!) {
  repository(owner: $owner, name: $name) {
    issues(first: 100, after: $after) {
      pageInfo { hasNextPage endCursor }
      nodes {
        number title state stateReason createdAt closedAt
        parent { number repository { nameWithOwner } }
        subIssuesSummary { total completed }
        timelineItems(itemTypes: [REOPENED_EVENT], first: 1) { totalCount }
        closedByPullRequestsReferences(first: 10, includeClosedPrs: true) {
          nodes { number state createdAt mergedAt url headRefName }
        }
      }
    }
  }
  search(query: $revertSearch, type: ISSUE, first: 100) @include(if: $withReverts) {
    nodes { ... on PullRequest { number body mergedAt } }
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
    /// Present on the first page only.
    search: Option<Search>,
}

#[derive(Deserialize)]
struct Search {
    nodes: Vec<RevertNode>,
}

/// A search hit: a PR, or `{}` for any other node type.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RevertNode {
    number: Option<i64>,
    body: Option<String>,
    merged_at: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Count {
    total_count: i64,
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
    state_reason: Option<String>,
    created_at: String,
    closed_at: Option<String>,
    parent: Option<Parent>,
    sub_issues_summary: Option<SubIssues>,
    timeline_items: Option<Count>,
    closed_by_pull_requests_references: Option<PrNodes>,
}

#[derive(Deserialize)]
struct Parent {
    number: i64,
    repository: NameWithOwner,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NameWithOwner {
    name_with_owner: String,
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
    head_ref_name: Option<String>,
}

impl IssueNode {
    /// The issue, fetched from `repository` (`owner/name`): a parent in any
    /// other repo is dropped, so the issue sits under no epic.
    fn into_issue(self, repository: &str) -> Issue {
        let issue = self.number;
        let (sub_total, sub_completed) = self
            .sub_issues_summary
            .map_or((0, 0), |s| (s.total, s.completed));
        let prs = self
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
                head_ref: pr.head_ref_name,
            })
            .collect();
        Issue {
            number: issue,
            title: self.title,
            state: self.state,
            state_reason: self.state_reason,
            created_at: self.created_at,
            closed_at: self.closed_at,
            parent: self
                .parent
                .filter(|p| {
                    p.repository
                        .name_with_owner
                        .eq_ignore_ascii_case(repository)
                })
                .map(|p| p.number),
            sub_total,
            sub_completed,
            reopened: self.timeline_items.map(|t| t.total_count),
            prs,
        }
    }
}

impl RevertNode {
    /// The revert, when the hit is a merged PR of `repository`.
    fn into_revert(self, repository: &str) -> Option<Revert> {
        Some(Revert {
            number: self.number?,
            reverted: self.body.as_deref().and_then(|b| reverted(b, repository)),
            merged_at: self.merged_at?,
        })
    }
}

/// The PR number a revert PR's `body` names on a line starting
/// `Reverts owner/name#N` (GitHub's own revert body) or `Reverts #N`, when
/// `owner/name` is `repository` (case-insensitive).
fn reverted(body: &str, repository: &str) -> Option<i64> {
    body.lines().find_map(|line| {
        let (repo, number) = line.trim().strip_prefix("Reverts ")?.split_once('#')?;
        if !(repo.is_empty() || repo.eq_ignore_ascii_case(repository)) {
            return None;
        }
        let digits = number
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(number.len());
        number[..digits].parse().ok()
    })
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
    proc::command(program, args, Stdio::piped())
}

/// All issues of `owner/name`, following the query's cursor page by page,
/// and (from the first page) its merged revert PRs.
/// Errors (as text for the repo's `error` column): the runner failing, a
/// malformed or error response, a missing repo, or more than 50 pages.
pub fn fetch_repo(runner: &mut Runner, owner: &str, name: &str) -> Result<Repo, String> {
    let repository = format!("{owner}/{name}");
    let revert_search = format!("repo:{repository} is:pr is:merged in:title revert");
    let mut repo = Repo::default();
    let mut after: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut args: Vec<String> = ["api", "graphql", "-f"].map(String::from).into();
        args.push(format!("query={QUERY}"));
        for (key, value) in [
            ("owner", owner),
            ("name", name),
            ("revertSearch", &revert_search),
        ] {
            args.extend(["-f".into(), format!("{key}={value}")]);
        }
        // -F is typed, so GraphQL gets a Boolean.
        args.extend(["-F".into(), format!("withReverts={}", after.is_none())]);
        if let Some(cursor) = &after {
            args.extend(["-f".into(), format!("after={cursor}")]);
        }
        let out = runner(&args).map_err(|e| format!("gh: {e}"))?;
        let response: Response =
            serde_json::from_slice(&out).map_err(|e| format!("bad gh response: {e}"))?;
        if let Some(first) = response.errors.as_deref().and_then(<[_]>::first) {
            return Err(format!("GitHub: {}", first.message));
        }
        let data = response
            .data
            .ok_or_else(|| format!("GitHub: no repository {repository}"))?;
        let page = data
            .repository
            .ok_or_else(|| format!("GitHub: no repository {repository}"))?
            .issues;
        let hits = data.search.map_or_else(Vec::new, |s| s.nodes);
        repo.reverts
            .extend(hits.into_iter().filter_map(|n| n.into_revert(&repository)));
        repo.issues
            .extend(page.nodes.into_iter().map(|n| n.into_issue(&repository)));
        match page.page_info {
            PageInfo {
                has_next_page: true,
                end_cursor: Some(cursor),
            } => after = Some(cursor),
            _ => return Ok(repo),
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

/// A repo to fetch: its slug and the owner a stored `github_links`
/// repository names for it, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub slug: String,
    pub owner: Option<String>,
}

/// What fetching one [`Target`] came to.
#[derive(Debug, Clone, PartialEq)]
pub enum Fetched {
    Cached {
        slug: String,
        owner: String,
        repo: Repo,
    },
    /// The owner is unknown when `gh api user` failed too.
    Failed {
        slug: String,
        owner: Option<String>,
        error: String,
    },
}

/// `slugs` (from [`Store::charge_slugs`]) with the owner hints the store
/// holds.
pub fn targets(store: &Store, slugs: &[String]) -> rusqlite::Result<Vec<Target>> {
    slugs
        .iter()
        .map(|slug| {
            Ok(Target {
                slug: slug.clone(),
                owner: store.gh_owner_hint(slug)?,
            })
        })
        .collect()
}

/// Fetches every target: under its owner hint, else the `gh` login, asked
/// for once. Never touches the store, so it can run off the thread that
/// owns the writer; a repo that fails comes back as [`Fetched::Failed`].
pub fn fetch_all(runner: &mut Runner, targets: &[Target]) -> Vec<Fetched> {
    let mut fallback: Option<Result<String, String>> = None;
    targets
        .iter()
        .map(|target| {
            let owner = match &target.owner {
                Some(owner) => Ok(owner.clone()),
                None => fallback.get_or_insert_with(|| login(runner)).clone(),
            };
            let slug = target.slug.clone();
            match owner {
                Ok(owner) => match fetch_repo(runner, &owner, &slug) {
                    Ok(repo) => Fetched::Cached { slug, owner, repo },
                    Err(error) => Fetched::Failed {
                        slug,
                        owner: Some(owner),
                        error,
                    },
                },
                Err(error) => Fetched::Failed {
                    slug,
                    owner: None,
                    error,
                },
            }
        })
        .collect()
}

/// Writes what [`fetch_all`] returned at `now_ms`: a fetched repo replaces
/// its cache; a failed one keeps its old rows and gets the error recorded.
/// Errors only when the store itself fails.
pub fn store_fetched(store: &mut Store, fetched: &[Fetched], now_ms: i64) -> rusqlite::Result<()> {
    for item in fetched {
        match item {
            Fetched::Cached { slug, owner, repo } => store.gh_replace(slug, owner, repo, now_ms)?,
            Fetched::Failed { slug, owner, error } => {
                store.gh_record_error(slug, owner.as_deref(), error)?;
            }
        }
    }
    Ok(())
}

/// Refreshes the cache of every repo slug in `slugs` (see [`fetch_all`]
/// and [`store_fetched`]) at `now_ms`, on the calling thread. A test and
/// probe helper: the app fetches on its own thread and stores on the
/// engine's.
pub fn refresh(
    store: &mut Store,
    runner: &mut Runner,
    slugs: &[&str],
    now_ms: i64,
) -> rusqlite::Result<()> {
    let slugs: Vec<String> = slugs.iter().map(|&slug| slug.to_owned()).collect();
    let targets = targets(store, &slugs)?;
    store_fetched(store, &fetch_all(runner, &targets), now_ms)
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

    fn scripted(
        script: Vec<io::Result<&'static str>>,
    ) -> (Runner, std::sync::Arc<std::sync::Mutex<usize>>) {
        let calls = std::sync::Arc::new(std::sync::Mutex::new(0));
        let seen = calls.clone();
        let mut script = script.into_iter();
        let runner: Runner = Box::new(move |_| {
            *seen.lock().expect("calls") += 1;
            script
                .next()
                .unwrap_or_else(|| Err(io::Error::other("exhausted")))
                .map(|json| json.as_bytes().to_vec())
        });
        (runner, calls)
    }

    const EMPTY_REPO: &str = r#"{"data":{"repository":{"issues":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[]}}}}"#;

    fn target(slug: &str, owner: Option<&str>) -> Target {
        Target {
            slug: slug.into(),
            owner: owner.map(Into::into),
        }
    }

    #[test]
    fn fetch_all_asks_for_the_login_once_and_reports_each_repo_without_failing() {
        let (mut runner, calls) = scripted(vec![
            Ok(r#"{"login":"me"}"#),
            Ok(EMPTY_REPO),
            Err(io::Error::other("rate limited")),
            Ok(EMPTY_REPO),
        ]);
        let targets = [
            target("a", None),
            target("b", None),
            target("c", Some("org")),
        ];
        let fetched = fetch_all(&mut runner, &targets);
        assert_eq!(*calls.lock().expect("calls"), 4, "login, a, b, c");
        assert!(matches!(&fetched[0], Fetched::Cached { slug, owner, repo }
            if slug == "a" && owner == "me" && repo.issues.is_empty()));
        assert!(matches!(&fetched[1], Fetched::Failed { slug, owner, error }
            if slug == "b" && owner.as_deref() == Some("me") && error.contains("rate limited")));
        assert!(matches!(&fetched[2], Fetched::Cached { slug, owner, .. }
            if slug == "c" && owner == "org"));
    }

    #[test]
    fn a_revert_names_the_pr_it_reverts_in_its_own_repo_only() {
        let repo = "example/perf";
        for (body, want) in [
            ("Reverts example/perf#2", Some(2)),
            ("Reverts Example/Perf#12\n\nLorem.", Some(12)),
            ("Lorem.\r\nReverts #7.", Some(7)),
            ("Reverts example/other#9", None),
            ("Reverts example/perf-two#9", None),
            ("This reverts commit 0000000.", None),
            ("Reverts example/perf#", None),
            ("", None),
        ] {
            assert_eq!(reverted(body, repo), want, "{body:?}");
        }
    }

    #[test]
    fn fetch_all_with_no_gh_fails_each_repo_with_no_owner() {
        let (mut runner, _) = scripted(vec![]);
        let fetched = fetch_all(&mut runner, &[target("a", None), target("b", None)]);
        assert!(fetched.iter().all(|f| matches!(f,
            Fetched::Failed { owner: None, error, .. } if error.contains("exhausted"))));
    }
}
