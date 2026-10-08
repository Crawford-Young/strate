# strate

A desktop app for watching and steering Claude Code work in real time, with audit on demand. Strate shows the live agent graph (orchestrators, subagents, teammates) with state, cost, run time and context use, gives every agent its own console, and keeps a terminal docked beside it. History is searchable, but the live view comes first. It is built for one user first and generalized later. Built with Tauri v2, React, Vite and Tailwind, styled entirely with `@crawfordyoung/ui`.

Status: bootstrap. The app opens an empty shell; only the dark theme exists for now.

The design spec and ADR-0001 (stack decision) live in the private planning-docs repo under `docs/apps/strate/specs/` (`strate-v1-design` and `adr-0001-stack`).

## Prerequisites

- Windows 11 with WebView2 (preinstalled on Windows 11)
- Rust stable, MSVC toolchain (`rust-toolchain.toml` pins the channel and components)
- Visual Studio Build Tools with the "Desktop development with C++" workload
- Node 22.22.1+ (see `engines` in `package.json`) and pnpm 11 (`corepack enable`)

## Commands

```sh
pnpm install
pnpm tauri dev        # run the app (Vite dev server on strict port 1420)
```

Gates (CI runs the same on `windows-latest`):

```sh
pnpm typecheck
pnpm lint
pnpm test             # Vitest, 100% coverage thresholds
pnpm build            # must run before cargo: Tauri embeds dist/
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm tauri build --debug --no-bundle   # proves the app compiles
```
