# strate

A desktop app for watching and steering Claude Code work in real time, with audit on demand. Strate shows the live agent graph (orchestrators, subagents, teammates) with state, cost, run time and context use, gives every agent its own console, and keeps a terminal docked beside it. History is searchable, but the live view comes first. It is built for one user first and generalized later. Built with Tauri v2, React, Vite and Tailwind, styled entirely with `@crawfordyoung/ui`.

Status: the core is wired into the app (ingest, commands, events) and the app shell is in. It has one tab per live session (status dot, total $, needs-you badge; sessions that stopped drop off the strip) and a line over the canvas with running time, $ this session, $ total and needs-you count. The UI refetches on `store-changed` and never polls. Harness (left), console (right) and terminal (bottom) are resizable, collapsible panels; their sizes and open state are saved locally (`strate.layout` in localStorage) and restored on launch. Harness and Terminal start closed and open from their buttons, and the console opens when you select a node. The canvas draws the live session as a tiered graph (orchestrator, then its subagents and teammates, then their dispatches) with React Flow and an ELK layout in a web worker, each node's context gauge coloured against that agent's gear nudge/hard-stop thresholds. Panel bodies are placeholders for now. Only the dark theme exists.

The design spec and ADR-0001 (stack decision) live in the private planning-docs repo under `docs/apps/strate/specs/` (`strate-v1-design` and `adr-0001-stack`).

## Prerequisites

- Windows 11 with WebView2 (preinstalled on Windows 11)
- Rust stable, MSVC toolchain (`rust-toolchain.toml` pins the channel and components)
- Visual Studio Build Tools with the "Desktop development with C++" workload
- Bun 1.4.2+ (see `packageManager` in `package.json`); it is both package manager and runtime, no separate Node install needed

## Commands

```sh
bun install
bun run tauri dev       # run the app (Vite dev server on strict port 1420)
```

Gates (CI runs the same on `windows-latest`):

```sh
bun run typecheck
bun run lint
bun run test             # Vitest, 100% coverage thresholds
bun run build            # must run before cargo: Tauri embeds dist/
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
bun run tauri build --debug --no-bundle   # proves the app compiles
```

## Crates

- `src-tauri/`: the Tauri app. On startup it opens the store at `<app data dir>/strate.db` (`%APPDATA%\dev.crawfordyoung.strate\strate.db` on Windows) and starts one `strate-ingest` thread. That thread owns the only writer and runs the tailer, the registry poll (1 s) and, when opted in, the hook receiver. Once the initial scan is stored, it runs discovery to add dispatch and teammate edges, and runs it again for subagents not yet linked. The webview reads through async commands on a read-only connection: `status` (`indexing`, then `live`), `workstreams`, `workstream_costs`, `needs_you`, `live_graph(workstream)` and `audit_rollup(filter)` (the Audit tree and headline). `audit_refresh()` asks the engine to refetch GitHub issues, epics and PRs through `gh` for the repos of the named windows and resolves once that fetch is stored (at once when GitHub is off or there are no repos): a worker thread runs the fetch, the engine thread writes it and emits `store-changed`, requests during a fetch coalesce into one trailing fetch after it (so a window named meanwhile is fetched too), and a missing or failing `gh` is recorded per repo (listed in the audit's `githubErrors`, its issues shown with no GitHub data) instead of raised. A panic in a fetch fails that fetch's repos, and if the fetch thread ever ends, GitHub refresh turns off with one log line. Typed wrappers live in `src/api.ts`. After the store changes, a `store-changed` event (no payload, at most 4 a second) tells the UI to refetch. The engine stops on app exit. Settings come from the environment (see `.env.example`): `CLAUDE_CONFIG_DIR`, `STRATE_HOOK_PORT` (opts in to the hook receiver; a bad value fails startup instead of silently turning hooks off) and `STRATE_HOOK_TOKEN`.
- `crates/strate-core/`: the app-independent core. `discovery` resolves the Claude Code config dir (`CLAUDE_CONFIG_DIR`, else `~/.claude`) and enumerates `projects/` and `teams/` into a graph of session, subagent and team nodes. Each subagent links to its parent by dispatch (`toolUseId`), to its team as a teammate, or is kept as unresolved. It never opens `.credentials.json` or `sessions/`. Its tests run against a synthetic fixture in `crates/strate-core/tests/fixtures/claude-home/`, which `tests/fixtures/generate.sh` regenerates. `tail` follows every session and subagent transcript incrementally on a worker thread: `Tailer::start` returns at once, then parsed lines arrive in bounded batches over a bounded channel. It resumes each file from a starting byte offset and persists nothing itself: every batch carries the checkpoint it ends at, which stops at the last complete line, so a half-written line waits for its newline. A file that shrinks or is replaced is re-read from 0 and reported as a `Reset`. Changes arrive via `notify` on `projects/`, and a periodic size rescan catches any event `notify` drops. `store` is the SQLite record (rusqlite, bundled; WAL, `synchronous=NORMAL`, foreign keys on), which outlives transcript cleanup. Migrations are keyed on `PRAGMA user_version`. It holds events (raw JSON plus indexed columns, unique on `uuid`, else on file and byte offset), sessions (stub rows for orphan subagent dirs), agents, edges, offsets, GitHub links from `pr-link`, gear settings and intents, plus workstream grouping and the cost and time rollups described below. The tail consumer writes a batch's events and its checkpoint in one transaction and restarts the tailer from `Store::offsets`, so a crash before commit re-delivers the batch, and replaying it writes no duplicates. Read-only connections from `Store::open_reader` query while the writer ingests. Each ingested batch also regroups its session into workstreams in the same transaction. A transcript splits into segments at every `/rename` (`custom-title` change). Records before the first title take that title, and a `/clear` file's carried-name prelude merges forward. Each segment joins its name's workstream; a transcript with no name joins a fallback workstream keyed by its dominant cwd and branch. Sessions sharing a process root (the records' `session_id`, which every `/clear` successor keeps) are chained by continuation edges in start order. `Store::merge_workstream` merges by hand and survives regrouping. `Store::regroup_all` rebuilds everything and runs on upgrade.

Cost and time port the accounting in claude-config's `scripts/audit-lib.mjs` (`strate-core`'s `cost` and `time` modules). Prices come from `crates/strate-core/data/prices.json`, a byte-for-byte copy of claude-config's `scripts/prices.json` compiled in, never stored in SQLite. Each API request counts once: of the records sharing a `requestId` (else `message.id`, else `uuid`), the one with the most output. Its $ covers input, output, cache reads, 5m and 1h cache writes (a write with no tier split is all 5m) and web searches. A model with no price is counted as unpriced, never as $0, and `<synthetic>` records are neither priced nor taken as an agent's model. Context % is the latest request's depth (input + cache read + cache write) over the model's window from `data/context-windows.json`; a model missing there gets none. Time replays each transcript the way audit.mjs does: wall clock from first to last record, active time without idle gaps over 10 minutes (subagent records count as activity), waiting time for questions to the user and for the next typed prompt, and agent time as the sum of subagent run walls. Every ingested batch recomputes the rollups (`sessions` and `agents` cost, run, wait and context columns) of the sessions it touches, from the stored events, so a re-ingest changes nothing and an incremental build equals a cold one. Per-request $ sits in the `requests` view, and `workstream_costs` sums it per workstream. `cargo test` compares strate with an audit.mjs golden over the synthetic `tests/fixtures/cost-home/` (`tests/fixtures/audit-golden.json`: $ within 1%, tokens and times exact). Two more CI jobs check out claude-config: `prices-drift` fails when the vendored price list differs, and `audit-parity` runs audit.mjs live on Node and fails when it moves more than 1% from the golden.

The Audit (#26) sums `audit_cells` (migration 0006): one row per session, agent, `/rename` window, local day and model, computed beside the cost rollups. Time is cut at local midnights and window starts, and each request's $ and tokens go on its own model, day and window. Days are local to the store's zone: `Store::open` uses the machine's IANA zone (UTC, logged once, when it has none to name), `Store::open_in(path, zone)` injects one, and a change in meta `tz` recomputes every cell, as a new price list (meta `prices`) does. Zones come from jiff (`tzdb-bundle-platform`, since Windows has no system tzdb). A cell with a NULL day (a request with no usable timestamp) keeps the sums exact but falls in no date range. `names` parses window names as `<repo>-<issue>[-sN]`: a parsed window sits under its repo and issue whether or not GitHub knows the issue, and only an unnamed window or a name that doesn't parse goes to Unlinked. The GitHub cache (`gh_repos`, `gh_issues`, `gh_prs`; migration 0005, plus `state_reason` in 0007) holds each repo's issues, their sub-issue parents (a parent in another repo means no epic level) and closing PRs, fetched with `gh api graphql` at 100 issues a page, up to 50 pages. The owner comes from the newest `github_links` repository of that name, else the `gh` login; a failure is recorded on that repo only and keeps its old rows. `proc` is the spawn helper `gh` and `claude agents` share: no console window, stderr drained on its own thread, and a deadline that kills the child. `views::audit` rolls the cells in a filter (local days, repos, group-by) into a Repo > Epic > Issue > PR > Session > Agent tree, skipping idle-only cells, so a window that only sat open over a weekend shows nothing there. Its headline counts the issues closed as completed (not as not planned or duplicate) with work in the range: their median elapsed over the whole issue life (first window ever to close), their mean in-range $, and the mean $ per merged PR, where a PR closing several issues counts once.

The Audit view (#27) is the full-screen view behind the header's Audit button; its one bar holds the controls, Refresh GitHub, Export CSV and "‹ Live" (where Audit was). It reads when shown, when a control changes and after Refresh GitHub (which waits for the fetch, then rereads), never on a store change or a timer, and nothing while hidden. Its tree table keys rows by node path, so expanded rows stay put when a refresh inserts a sibling. Selecting a row's name docks its detail at the bottom (38%, resizable, closable): a per-day timeline from `audit_timeline(filter, path)` of the row's active, waiting and agent spans plus issue-closed and PR-merged marks, reread with the rollup. The timeline's agent spans are non-idle subagent time, while the table's "Agent-hours (wall)" is wall time, so the two differ. A mark names its issue or PR without the repo; a day, agent type or model row spans repos and says so. Export CSV writes the full tree under the current filter, one row per node (the root's own total is left out): a kind column, one level column per tree depth, the node path as JSON (`[[kind, key], …]`, which tells same-named siblings apart), GitHub, then every metric in raw units (ms, counts, and USD to four decimals), with a UTF-8 BOM, RFC 4180 quoting and a leading `'` on a name that would otherwise run as a spreadsheet formula. The webview hands only the text and a suggested file name to the `write_export(text, suggestedName)` command; the app opens the native save dialog itself (`tauri-plugin-dialog` on the Rust side) and writes where the user picks, so the webview never names a path. Cancelling writes nothing. There is no fs plugin, and no dialog capability: capabilities gate the plugin's webview commands, and the dialog is never opened from the webview.

Live state comes from two sources. `registry` polls `claude agents --json --all` once a second on a worker thread (the binary is an explicit path, else `claude` on `PATH`, else `~/.local/bin/claude`) and emits what appeared, changed or went away; a failed or malformed poll is an error event that keeps the last snapshot. `Store::apply_registry` keeps one `registry` row per entry (kind, `busy`/`waiting`/`idle` status or background state, `waitingFor`, name, pid or id, last seen, gone) and fills the orchestrator's `registry_name` and `registry_state`; a session listed before its transcript exists gets a stub. The registry never lists subagents, so a new subagent shows up through the tail first, typically well under a second after its transcript appears. `hooks` is an optional receiver for Claude Code `http` hooks, off unless started: it binds 127.0.0.1 only, takes POST only with a capped body and an optional bearer token, and answers `204` at once, before any store work. `Store::apply_hook` turns SubagentStart into the subagent's row before its transcript exists, PermissionRequest into needs-you on the agent (or the orchestrator), SubagentStop into done and TeammateIdle into idle; other events are ignored. The `needs_you` view lists everything waiting on you: live registry entries with status `waiting` and a `waitingFor`, plus agents with an unanswered PermissionRequest. Strate never edits settings.json. To use the receiver, start strate with `STRATE_HOOK_PORT=47615`, and optionally `STRATE_HOOK_TOKEN` set to a secret. Set the same `STRATE_HOOK_TOKEN` where Claude Code runs, then add hooks like this to settings.json yourself. The URL's port must match `STRATE_HOOK_PORT`, and the low `timeout` keeps a stopped receiver from holding Claude Code up:

```json
{
  "hooks": {
    "SubagentStart": [
      {
        "hooks": [
          {
            "type": "http",
            "url": "http://127.0.0.1:47615/hooks",
            "timeout": 2,
            "headers": { "Authorization": "Bearer $STRATE_HOOK_TOKEN" },
            "allowedEnvVars": ["STRATE_HOOK_TOKEN"]
          }
        ]
      }
    ],
    "SubagentStop": [
      {
        "hooks": [
          {
            "type": "http",
            "url": "http://127.0.0.1:47615/hooks",
            "timeout": 2,
            "headers": { "Authorization": "Bearer $STRATE_HOOK_TOKEN" },
            "allowedEnvVars": ["STRATE_HOOK_TOKEN"]
          }
        ]
      }
    ],
    "PermissionRequest": [
      {
        "hooks": [
          {
            "type": "http",
            "url": "http://127.0.0.1:47615/hooks",
            "timeout": 2,
            "headers": { "Authorization": "Bearer $STRATE_HOOK_TOKEN" },
            "allowedEnvVars": ["STRATE_HOOK_TOKEN"]
          }
        ]
      }
    ],
    "TeammateIdle": [
      {
        "hooks": [
          {
            "type": "http",
            "url": "http://127.0.0.1:47615/hooks",
            "timeout": 2,
            "headers": { "Authorization": "Bearer $STRATE_HOOK_TOKEN" },
            "allowedEnvVars": ["STRATE_HOOK_TOKEN"]
          }
        ]
      }
    ]
  }
}
```
