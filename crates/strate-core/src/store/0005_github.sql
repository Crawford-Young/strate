-- Cache of GitHub issues, sub-issue epics and closing PRs, per repo slug
-- (the repo part of a session name). A refresh replaces one repo's rows
-- whole; a failed refresh sets `error` and leaves the rows as they were.
-- `owner` is NULL until it is known; `fetched_at` (unix ms) is the last
-- successful refresh.
CREATE TABLE gh_repos (
    slug TEXT PRIMARY KEY,
    owner TEXT,
    name TEXT,
    fetched_at INTEGER,
    error TEXT
) STRICT;

CREATE TABLE gh_issues (
    repo TEXT NOT NULL REFERENCES gh_repos (slug),
    number INTEGER NOT NULL,
    title TEXT NOT NULL,
    state TEXT NOT NULL,
    created_at TEXT NOT NULL,
    closed_at TEXT,
    parent INTEGER,
    sub_total INTEGER NOT NULL,
    sub_completed INTEGER NOT NULL,
    PRIMARY KEY (repo, number)
) STRICT;

-- A PR that closes several issues has one row per issue.
CREATE TABLE gh_prs (
    repo TEXT NOT NULL REFERENCES gh_repos (slug),
    issue INTEGER NOT NULL,
    number INTEGER NOT NULL,
    state TEXT NOT NULL,
    created_at TEXT NOT NULL,
    merged_at TEXT,
    url TEXT NOT NULL,
    PRIMARY KEY (repo, issue, number)
) STRICT;
