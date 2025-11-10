# Google Maps List Comparator

Hardening pass for the Sprint 0 platform baseline: a Tauri + React desktop harness with SQLCipher storage, secure key handling, and telemetry buffering that future sprints can trust without revisiting infrastructure.

## Platform Baseline

- **SQLCipher bootstrap + recovery**: on every launch we derive an encryption key from the OS keychain, run migrations, verify the file header is encrypted, and automatically recycle both the database file and key if the stored material is missing or corrupted. Recovery status and key lifecycle (`created`, `retrieved`, `rotated`) surface via the `FoundationHealth` UI/state API.
- **Secure storage audit trail**: vault operations log via `tracing` (no secrets) and emit a `vault_audit` telemetry event during bootstrap so we can prove secrets only ever live in the OS keychain. Keys are generated with `OsRng` and helper methods (`rotate`, `delete`) support recovery tests.
- **Telemetry buffer hardening**: the JSONL buffer now survives restarts, rotates when it breaches configurable size limits, and gracefully reports disk-full or permission failures without losing queued events. Behavior is covered by unit tests, and the buffer location appears in the runtime panel for inspection.
- **Tooling snapshot**: lint (`pnpm lint`), typecheck (`pnpm typecheck`), unit tests (`pnpm test`), and Rust coverage (`cargo test -p tauri-app`) are all wired and documented below so contributors can reproduce CI locally.

## Development Workflow

Run these from the repository root:

- `pnpm dev` – Vite front-end in watch mode.
- `pnpm tauri dev` – full-stack desktop shell.
- `pnpm lint` / `pnpm typecheck` / `pnpm test` – JS/TS quality gates (also combined via `pnpm check`).
- `cargo test -p tauri-app` – Rust unit tests (SQLCipher bootstrap, telemetry buffer, vault helpers).

> Tip: if you add new telemetry scenarios, keep JSONL lines small so the default 5 MiB rotation window isn’t tripped unintentionally.

## Environment & Secrets

- `.env` files are **only** loaded in debug/dev builds. Production binaries rely on real environment variables or the OS keychain. If you need `.env` during automated testing, opt-in via `ALLOW_DOTENV=1`.
- Database keys live under the OS keychain service `GoogleMapsListComparator` and are never written to disk. Deleting/corrupting the entry automatically forces a secure rebootstrap on next launch.
- Telemetry buffer defaults can be tuned with:
  - `TELEMETRY_BUFFER_MAX_BYTES` (default `5 * 1024 * 1024`)
  - `TELEMETRY_BUFFER_MAX_FILES` (default `5`, includes the live file)

## Observability Aids

- The UI “Runtime status” section shows the encrypted DB path, telemetry buffer path, queue depth, bootstrap recovery status, and redacted key lifecycle so QA can quickly confirm health.
- Telemetry events (`vault_audit`, `app_start`, etc.) drain to `telemetry-buffer.jsonl` and rotate to timestamped archives when full. JSONL makes it easy to `rg` or `jq` through the backlog while offline.

## IDE / Tooling

Any editor works; VS Code with the Tauri + rust-analyzer extensions provides the smoothest DX for React + Rust cross-over editing.
