# strate

A desktop app for watching and steering Claude Code work in real time, with audit on demand. Strate shows the live agent graph (orchestrators, subagents, teammates) with state, cost, run time and context use, gives every agent its own console, and keeps a terminal docked beside it. History is searchable, but the live view comes first. It is built for one user first and generalized later. Built with Tauri v2, React, Vite and Tailwind, styled entirely with `@crawfordyoung/ui`.

Status: bootstrap. The app opens an empty shell; only the dark theme exists for now.

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

- `src-tauri/`: the Tauri app shell.
- `crates/strate-core/`: the app-independent core. `discovery` resolves the Claude Code config dir (`CLAUDE_CONFIG_DIR`, else `~/.claude`) and enumerates `projects/` and `teams/` into a graph of session, subagent and team nodes. Each subagent links to its parent by dispatch (`toolUseId`), to its team as a teammate, or is kept as unresolved. It never opens `.credentials.json` or `sessions/`. Its tests run against a synthetic fixture in `crates/strate-core/tests/fixtures/claude-home/`, which `tests/fixtures/generate.sh` regenerates. `tail` follows every session and subagent transcript incrementally on a worker thread: `Tailer::start` returns at once, then parsed lines arrive in bounded batches over a bounded channel. It resumes each file from a starting byte offset and persists nothing itself: every batch carries the checkpoint it ends at, which stops at the last complete line, so a half-written line waits for its newline. A file that shrinks or is replaced is re-read from 0 and reported as a `Reset`. Changes arrive via `notify` on `projects/`, and a periodic size rescan catches any event `notify` drops. `store` is the SQLite record (rusqlite, bundled; WAL, `synchronous=NORMAL`, foreign keys on), which outlives transcript cleanup. Migrations are keyed on `PRAGMA user_version`. It holds events (raw JSON plus indexed columns, unique on `uuid`, else on file and byte offset), sessions (stub rows for orphan subagent dirs), agents, edges, offsets, GitHub links from `pr-link`, gear settings and intents, plus workstream grouping and the cost and time rollups described below. The tail consumer writes a batch's events and its checkpoint in one transaction and restarts the tailer from `Store::offsets`, so a crash before commit re-delivers the batch, and replaying it writes no duplicates. Read-only connections from `Store::open_reader` query while the writer ingests. Each ingested batch also regroups its session into workstreams in the same transaction. A transcript splits into segments at every `/rename` (`custom-title` change). Records before the first title take that title, and a `/clear` file's carried-name prelude merges forward. Each segment joins its name's workstream; a transcript with no name joins a fallback workstream keyed by its dominant cwd and branch. Sessions sharing a process root (the records' `session_id`, which every `/clear` successor keeps) are chained by continuation edges in start order. `Store::merge_workstream` merges by hand and survives regrouping. `Store::regroup_all` rebuilds everything and runs on upgrade.

Cost and time port the accounting in claude-config's `scripts/audit-lib.mjs` (`strate-core`'s `cost` and `time` modules). Prices come from `crates/strate-core/data/prices.json`, a byte-for-byte copy of claude-config's `scripts/prices.json` compiled in, never stored in SQLite. Each API request counts once: of the records sharing a `requestId` (else `message.id`, else `uuid`), the one with the most output. Its $ covers input, output, cache reads, 5m and 1h cache writes (a write with no tier split is all 5m) and web searches. A model with no price is counted as unpriced, never as $0, and `<synthetic>` records are neither priced nor taken as an agent's model. Context % is the latest request's depth (input + cache read + cache write) over the model's window from `data/context-windows.json`; a model missing there gets none. Time replays each transcript the way audit.mjs does: wall clock from first to last record, active time without idle gaps over 10 minutes (subagent records count as activity), waiting time for questions to the user and for the next typed prompt, and agent time as the sum of subagent run walls. Every ingested batch recomputes the rollups (`sessions` and `agents` cost, run, wait and context columns) of the sessions it touches, from the stored events, so a re-ingest changes nothing and an incremental build equals a cold one. Per-request $ sits in the `requests` view, and `workstream_costs` sums it per workstream. `cargo test` compares strate with an audit.mjs golden over the synthetic `tests/fixtures/cost-home/` (`tests/fixtures/audit-golden.json`: $ within 1%, tokens and times exact). Two more CI jobs check out claude-config: `prices-drift` fails when the vendored price list differs, and `audit-parity` runs audit.mjs live on Node and fails when it moves more than 1% from the golden.

Live state comes from two sources. `registry` polls `claude agents --json --all` once a second on a worker thread (the binary is an explicit path, else `claude` on `PATH`, else `~/.local/bin/claude`) and emits what appeared, changed or went away; a failed or malformed poll is an error event that keeps the last snapshot. `Store::apply_registry` keeps one `registry` row per entry (kind, `busy`/`waiting`/`idle` status or background state, `waitingFor`, name, pid or id, last seen, gone) and fills the orchestrator's `registry_name` and `registry_state`; a session listed before its transcript exists gets a stub. The registry never lists subagents, so a new subagent shows up through the tail first, typically well under a second after its transcript appears. `hooks` is an optional receiver for Claude Code `http` hooks, off unless started: it binds 127.0.0.1 only, takes POST only with a capped body and an optional bearer token, and answers `204` at once, before any store work. `Store::apply_hook` turns SubagentStart into the subagent's row before its transcript exists, PermissionRequest into needs-you on the agent (or the orchestrator), SubagentStop into done and TeammateIdle into idle; other events are ignored. The `needs_you` view lists everything waiting on you: live registry entries with status `waiting` and a `waitingFor`, plus agents with an unanswered PermissionRequest. Strate never edits settings.json; to use the receiver, add hooks like this to it yourself (the port is the one you start the receiver on, and the low `timeout` keeps a stopped receiver from holding Claude Code up):

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
