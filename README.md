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
- `crates/strate-core/`: the app-independent core. `discovery` resolves the Claude Code config dir (`CLAUDE_CONFIG_DIR`, else `~/.claude`) and enumerates `projects/` and `teams/` into a graph of session, subagent and team nodes. Each subagent links to its parent by dispatch (`toolUseId`), to its team as a teammate, or is kept as unresolved. It never opens `.credentials.json` or `sessions/`. Its tests run against a synthetic fixture in `crates/strate-core/tests/fixtures/claude-home/`, which `tests/fixtures/generate.sh` regenerates. `tail` follows every session and subagent transcript incrementally on a worker thread: `Tailer::start` returns at once, then parsed lines arrive in bounded batches over a bounded channel. It resumes each file from a byte offset kept in an `OffsetStore`, and it commits only through the last complete line, so a half-written line waits for its newline. A file that shrinks or is replaced is re-read from 0 and reported as a `Reset`. Changes arrive via `notify` on `projects/`, and a periodic size rescan catches any event `notify` drops.
