# CLAUDE.md — strate

Stack and rationale: ADR-0001 (`docs/apps/strate/specs/2026-10-08-adr-0001-stack.md`, private docs repo). Apps-domain rules in `~/code/apps/CLAUDE.md` apply.

## Gates

`bun run typecheck`, `bun run lint`, `bun run test` (100% coverage), `bun run build`, `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `bun run tauri build --debug --no-bundle`. Run `bun run build` before cargo (`generate_context!` needs `dist/`).

Every `strate-core` wave is also checked against the real `~/.claude` with a probe: a throwaway crate in the session scratchpad with a path dependency on the worktree's `strate-core`. It prints counts only and is never committed. Fixtures encode assumptions, and the probe has confirmed or corrected the design in every wave.

## Rules

- Package manager and runtime is Bun (1.4.2, pinned in `packageManager`). Never npm, yarn or pnpm. `bunfig.toml` sets `[run] bun = true`, so every script and bin runs on the Bun runtime; no Node needed.
- TypeScript is pinned `~6.0.3` because typescript-eslint's peer range is `<6.1.0`. Don't bump it until typescript-eslint widens.
- Only the dark theme exists (`class="dark"` on `index.html`).

## Traps

- This repo is PUBLIC. Transcript fixtures are scrubbed or synthetic, never raw `~/.claude` content. `crates/strate-core/tests/fixtures/` is synthetic: real field names, placeholder values only (lorem text, `/work/...` cwds), and no real paths, prompts, emails or keys. Change it by editing `generate.sh` and re-running it, not by pasting real transcripts.
- The agent registry is `claude agents --json`, never `sessions/<pid>.json`.
- Layout is the user's call: Harness left, console right, terminal bottom, Audit the only full-screen view. Don't assume otherwise; ask.
- UI comes from `@crawfordyoung/ui`; new pieces land in the library first.
- happy-dom has no layout, so the real resizable group never reports a layout or applies `resize`/`collapse`. App tests swap in `tests/resizable-mock.tsx`, and graph tests swap in `tests/xyflow-mock.tsx` for React Flow (which never measures nodes) plus in-process `elk.bundled.js` for `./elk`; check real panel and graph behaviour in `bun run tauri dev`.
- Dev server port 1420 is strict (`vite.config.ts` and Tauri `devUrl`); don't let it fall back.
- Bun caches a local tgz by its path. To swap `vendor/*.tgz` in place, run `rm -rf ~/.bun/install/cache/@T@* node_modules/@crawfordyoung/ui`, then rewrite its sha512 `integrity` in `bun.lock` (computed with node `crypto`), then `bun install`. `bun add` on its own fails with IntegrityCheckFailed.
