# CLAUDE.md — strate

Stack and rationale: ADR-0001 (`docs/apps/strate/specs/2026-10-08-adr-0001-stack.md`, private docs repo). Apps-domain rules in `~/code/apps/CLAUDE.md` apply.

## Gates

`pnpm typecheck`, `pnpm lint`, `pnpm test` (100% coverage), `pnpm build`, `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `pnpm tauri build --debug --no-bundle`. Run `pnpm build` before cargo (`generate_context!` needs `dist/`).

## Rules

- Package manager is pnpm (11). Never npm or yarn.
- TypeScript is pinned `~6.0.3` because typescript-eslint's peer range is `<6.1.0`. Don't bump it until typescript-eslint widens.
- `src/cyui-tailwind.d.ts` shims a declaration `@crawfordyoung/ui@0.29.1` fails to publish. Delete it once the library ships `dist/tailwind/index.d.ts`.
- Only the dark theme exists (`class="dark"` on `index.html`).

## Traps

- This repo is PUBLIC. Transcript fixtures are scrubbed or synthetic, never raw `~/.claude` content.
- The agent registry is `claude agents --json`, never `sessions/<pid>.json`.
- Layout is the user's call: Harness left, console right, terminal bottom, Audit the only full-screen view. Don't assume otherwise; ask.
- UI comes from `@crawfordyoung/ui`; new pieces land in the library first.
- Dev server port 1420 is strict (`vite.config.ts` and Tauri `devUrl`); don't let it fall back.
