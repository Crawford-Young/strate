//! The GitHub cache over scripted `gh` output: no real `gh` and no network
//! is ever touched. Fixtures are synthetic GraphQL responses.

use std::collections::VecDeque;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, params};
use strate_core::github::{Revert, Runner, cli_runner, fetch_repo, refresh};
use strate_core::store::{Store, gh_issues, gh_repos, gh_reverts};

mod common;

use common::{Temp, fixtures};

fn fixture(name: &str) -> Vec<u8> {
    let path = fixtures().join("github").join(name);
    fs::read(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"))
}

type Calls = Arc<Mutex<Vec<Vec<String>>>>;

/// A runner answering each call from `script` in order (an exhausted script
/// is an error), recording every argv.
fn scripted(script: Vec<io::Result<Vec<u8>>>) -> (Runner, Calls) {
    let calls: Calls = Arc::default();
    let seen = Arc::clone(&calls);
    let mut script: VecDeque<_> = script.into();
    let runner: Runner = Box::new(move |args| {
        seen.lock().expect("calls").push(args.to_vec());
        script
            .pop_front()
            .unwrap_or_else(|| Err(io::Error::other("script exhausted")))
    });
    (runner, calls)
}

fn ok(name: &str) -> io::Result<Vec<u8>> {
    Ok(fixture(name))
}

fn arg<'a>(call: &'a [String], key: &str) -> Option<&'a str> {
    call.iter().find_map(|a| a.strip_prefix(&format!("{key}=")))
}

#[test]
fn pages_are_followed_by_cursor_and_parse_into_issues_with_their_prs() {
    let (mut runner, calls) = scripted(vec![ok("page1.json"), ok("page2.json")]);
    let issues = fetch_repo(&mut runner, "acme", "widget")
        .expect("fetch")
        .issues;
    let numbers: Vec<_> = issues.iter().map(|i| i.number).collect();
    assert_eq!(numbers, [4, 25, 26]);

    let epic = &issues[0];
    assert_eq!(
        (epic.sub_total, epic.sub_completed, epic.parent),
        (3, 1, None)
    );
    assert_eq!(epic.state, "OPEN");
    let closed = &issues[1];
    assert_eq!(closed.parent, Some(4));
    assert_eq!(closed.closed_at.as_deref(), Some("2026-10-05T12:00:00Z"));
    let prs: Vec<_> = closed
        .prs
        .iter()
        .map(|p| (p.number, p.issue, p.state.as_str()))
        .collect();
    assert_eq!(prs, [(31, 25, "MERGED"), (30, 25, "CLOSED")]);
    assert_eq!(
        closed.prs[0].merged_at.as_deref(),
        Some("2026-10-05T12:00:00Z")
    );
    assert_eq!(closed.prs[0].url, "https://github.com/acme/widget/pull/31");
    let nulls = &issues[2];
    assert_eq!(
        (nulls.sub_total, nulls.sub_completed, nulls.prs.len()),
        (0, 0, 0)
    );

    let calls = calls.lock().expect("calls");
    assert_eq!(calls.len(), 2);
    assert_eq!(&calls[0][..2], ["api", "graphql"]);
    assert_eq!(arg(&calls[0], "owner"), Some("acme"));
    assert_eq!(arg(&calls[0], "name"), Some("widget"));
    assert_eq!(arg(&calls[0], "after"), None);
    assert_eq!(arg(&calls[1], "after"), Some("CURSOR-1"));
    assert!(arg(&calls[0], "query").is_some_and(|q| q.contains("closedByPullRequestsReferences")));
    assert_eq!(
        (arg(&calls[0], "withReverts"), arg(&calls[1], "withReverts")),
        (Some("true"), Some("false")),
        "the revert search rides on the first page only"
    );
    assert!(
        issues.iter().all(|i| i.reopened.is_none()),
        "no timelineItems: not known"
    );
}

#[test]
fn a_parent_in_another_repo_is_no_epic_and_the_close_reason_is_kept() {
    let (mut runner, calls) = scripted(vec![ok("audit-home.json")]);
    // GitHub owners are case-insensitive.
    let repo = fetch_repo(&mut runner, "Example", "demo").expect("fetch");
    let issues = &repo.issues;
    let by = |n: i64| issues.iter().find(|i| i.number == n).expect("issue");
    assert_eq!(by(9).parent, Some(8), "a parent in example/demo");
    assert_eq!(by(11).parent, None, "a parent in example/elsewhere");
    assert_eq!(by(9).state_reason.as_deref(), Some("COMPLETED"));
    assert_eq!(by(11).state_reason.as_deref(), Some("NOT_PLANNED"));
    assert_eq!(by(8).state_reason, None, "open");
    let query = calls.lock().expect("calls")[0].clone();
    assert!(
        arg(&query, "query")
            .is_some_and(|q| q.contains("stateReason") && q.contains("nameWithOwner"))
    );

    let tmp = Temp::new("reason");
    let mut store = Store::open(tmp.db()).expect("open");
    store
        .gh_replace("demo", "example", &repo, 1)
        .expect("store");
    let stored = gh_issues(&Store::open_reader(tmp.db()).expect("reader"), "demo").expect("issues");
    assert_eq!(&stored, issues, "parent and close reason round-trip");
}

#[test]
fn reopen_counts_and_merged_reverts_are_fetched_with_the_issues_and_cached() {
    let (mut runner, calls) = scripted(vec![ok("perf-home.json")]);
    let repo = fetch_repo(&mut runner, "example", "perf").expect("fetch");
    let reopened: Vec<_> = repo.issues.iter().map(|i| (i.number, i.reopened)).collect();
    assert_eq!(reopened, [(1, Some(1)), (3, Some(0))]);
    let revert = Revert {
        number: 4,
        reverted: Some(2),
        merged_at: "2026-10-06T11:40:00Z".into(),
    };
    assert_eq!(repo.reverts, std::slice::from_ref(&revert));
    let call = calls.lock().expect("calls")[0].clone();
    assert!(arg(&call, "query").is_some_and(|q| q.contains("REOPENED_EVENT")));
    assert_eq!(
        arg(&call, "revertSearch"),
        Some("repo:example/perf is:pr is:merged in:title revert")
    );

    let tmp = Temp::new("perf");
    let mut store = Store::open(tmp.db()).expect("open");
    store
        .gh_replace("perf", "example", &repo, 1)
        .expect("store");
    let reader = Store::open_reader(tmp.db()).expect("reader");
    assert_eq!(gh_issues(&reader, "perf").expect("issues"), repo.issues);
    assert_eq!(gh_reverts(&reader, "perf").expect("reverts"), [revert]);

    // A refresh replaces the reverts with the issues.
    let (mut runner, _) = scripted(vec![ok("page2.json")]);
    let empty = fetch_repo(&mut runner, "example", "perf").expect("fetch");
    store
        .gh_replace("perf", "example", &empty, 2)
        .expect("store");
    assert_eq!(gh_reverts(&reader, "perf").expect("reverts"), []);
}

#[test]
fn bad_json_graphql_errors_and_runner_failures_are_errors_not_panics() {
    for answer in [
        Ok(b"not json".to_vec()),
        Ok(b"{}".to_vec()),
        ok("not-found.json"),
        Err(io::Error::other("gh: not logged in")),
    ] {
        let (mut runner, _) = scripted(vec![answer]);
        let err = fetch_repo(&mut runner, "acme", "widget").expect_err("fails");
        assert!(!err.is_empty());
    }
    let (mut runner, _) = scripted(vec![ok("not-found.json")]);
    let err = fetch_repo(&mut runner, "acme", "nope").expect_err("not found");
    assert!(err.contains("Could not resolve"), "{err}");
}

#[test]
fn a_cursor_that_never_ends_stops_with_an_error() {
    let forever = (0..200).map(|_| ok("page1.json")).collect();
    let (mut runner, calls) = scripted(forever);
    assert!(fetch_repo(&mut runner, "acme", "widget").is_err());
    assert!(calls.lock().expect("calls").len() < 200);
}

#[test]
fn a_refresh_caches_a_repo_replacing_its_rows_and_resolving_the_owner_from_gh_user() {
    let tmp = Temp::new("refresh");
    let mut store = Store::open(tmp.db()).expect("open");
    let (mut runner, calls) = scripted(vec![ok("user.json"), ok("page1.json"), ok("page2.json")]);
    refresh(&mut store, &mut runner, &["widget"], 1_000).expect("refresh");

    assert_eq!(calls.lock().expect("calls")[0][..2], ["api", "user"]);
    let reader = Store::open_reader(tmp.db()).expect("reader");
    let repos = gh_repos(&reader).expect("repos");
    assert_eq!(repos.len(), 1);
    assert_eq!(repos[0].slug, "widget");
    assert_eq!(repos[0].owner.as_deref(), Some("acme"));
    assert_eq!(repos[0].fetched_at, Some(1_000));
    assert_eq!(repos[0].error, None);
    let issues = gh_issues(&reader, "widget").expect("issues");
    assert_eq!(
        issues.iter().map(|i| i.number).collect::<Vec<_>>(),
        [4, 25, 26]
    );
    assert_eq!(issues[1].prs.len(), 2);

    // A second refresh replaces the rows: only issue 26 remains upstream.
    let one_page = br#"{"data":{"repository":{"issues":{"pageInfo":{"hasNextPage":false,
        "endCursor":null},"nodes":[{"number":26,"title":"Renamed","state":"CLOSED",
        "createdAt":"2026-10-03T09:00:00Z","closedAt":"2026-10-06T00:00:00Z","parent":null,
        "subIssuesSummary":null,"closedByPullRequestsReferences":null}]}}}}"#;
    let (mut runner, _) = scripted(vec![ok("user.json"), Ok(one_page.to_vec())]);
    refresh(&mut store, &mut runner, &["widget"], 2_000).expect("refresh");
    let issues = gh_issues(&reader, "widget").expect("issues");
    assert_eq!(issues.len(), 1);
    assert_eq!(
        (issues[0].title.as_str(), issues[0].state.as_str()),
        ("Renamed", "CLOSED")
    );
    assert!(issues[0].prs.is_empty());
    assert_eq!(gh_repos(&reader).expect("repos")[0].fetched_at, Some(2_000));
}

#[test]
fn the_owner_comes_from_a_matching_github_link_before_gh_user() {
    let tmp = Temp::new("owner");
    let mut store = Store::open(tmp.db()).expect("open");
    let conn = Connection::open(tmp.db()).expect("raw");
    conn.execute_batch("INSERT INTO sessions (session_id, path) VALUES ('s1', '/p/s1.jsonl')")
        .expect("session");
    for (url, repo) in [
        (
            "https://github.com/other/widget-extra/pull/1",
            "other/widget-extra",
        ),
        ("https://github.com/org-x/widget/pull/2", "org-x/widget"),
    ] {
        conn.execute(
            "INSERT INTO github_links (session_id, pr_url, pr_repository) VALUES ('s1', ?1, ?2)",
            params![url, repo],
        )
        .expect("link");
    }
    assert_eq!(
        store.gh_owner_hint("widget").expect("hint").as_deref(),
        Some("org-x")
    );
    assert_eq!(store.gh_owner_hint("gadget").expect("hint"), None);

    let (mut runner, calls) = scripted(vec![ok("page2.json")]);
    refresh(&mut store, &mut runner, &["widget"], 5).expect("refresh");
    let calls = calls.lock().expect("calls");
    assert_eq!(calls.len(), 1, "no gh api user call");
    assert_eq!(arg(&calls[0], "owner"), Some("org-x"));
}

#[test]
fn a_failed_refresh_records_the_error_and_keeps_the_old_rows() {
    let tmp = Temp::new("failure");
    let mut store = Store::open(tmp.db()).expect("open");
    let (mut good, _) = scripted(vec![ok("user.json"), ok("page1.json"), ok("page2.json")]);
    refresh(&mut store, &mut good, &["widget"], 1_000).expect("refresh");

    let answers: Vec<Vec<io::Result<Vec<u8>>>> = vec![
        vec![
            ok("user.json"),
            Err(io::Error::other("rate limit exceeded")),
        ],
        vec![ok("user.json"), ok("page1.json"), Ok(b"garbage".to_vec())],
        vec![Err(io::Error::new(io::ErrorKind::NotFound, "gh missing"))],
    ];
    let reader = Store::open_reader(tmp.db()).expect("reader");
    for (i, answer) in answers.into_iter().enumerate() {
        let (mut bad, _) = scripted(answer);
        refresh(&mut store, &mut bad, &["widget"], 9_000).expect("never an error");
        let repo = &gh_repos(&reader).expect("repos")[0];
        assert!(repo.error.is_some(), "case {i}");
        assert_eq!(repo.fetched_at, Some(1_000), "case {i}: last success kept");
        assert_eq!(
            gh_issues(&reader, "widget").expect("issues").len(),
            3,
            "case {i}"
        );
    }
    let error = gh_repos(&reader).expect("repos")[0].error.clone();
    assert!(error.is_some_and(|e| e.contains("gh missing")));

    // Recovery clears the error.
    let (mut good, _) = scripted(vec![ok("user.json"), ok("page2.json")]);
    refresh(&mut store, &mut good, &["widget"], 3_000).expect("refresh");
    let repo = &gh_repos(&reader).expect("repos")[0];
    assert_eq!((repo.error.clone(), repo.fetched_at), (None, Some(3_000)));
}

#[test]
fn an_unknown_owner_records_an_error_for_each_repo_and_never_panics() {
    let tmp = Temp::new("no-owner");
    let mut store = Store::open(tmp.db()).expect("open");
    let (mut runner, _) = scripted(vec![Err(io::Error::other("gh auth login required"))]);
    refresh(&mut store, &mut runner, &["widget", "gadget"], 1).expect("refresh");
    let repos = gh_repos(&Store::open_reader(tmp.db()).expect("reader")).expect("repos");
    assert_eq!(repos.len(), 2);
    for repo in &repos {
        assert_eq!(repo.owner, None);
        assert!(
            repo.error
                .as_deref()
                .is_some_and(|e| e.contains("auth login"))
        );
    }
}

#[test]
fn a_missing_gh_binary_is_an_error() {
    let mut runner = cli_runner(PathBuf::from("/no/such/dir/gh-strate-test"));
    assert!(runner(&["api".into(), "user".into()]).is_err());
}
