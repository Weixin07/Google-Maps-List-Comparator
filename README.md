# Google Maps List Comparator

Hardening pass for the Sprint 0 platform baseline: a Tauri + React desktop harness with SQLCipher storage, secure key handling, and telemetry buffering that future sprints can trust without revisiting infrastructure.

## Platform Baseline

- **SQLCipher bootstrap + recovery**: on every launch we derive an encryption key from the OS keychain, run migrations, verify the file header is encrypted, and automatically recycle both the database file and key if the stored material is missing or corrupted. Recovery status and key lifecycle (`created`, `retrieved`, `rotated`) surface via the `FoundationHealth` UI/state API.
- **Secure storage audit trail**: vault operations log via `tracing` (no secrets) and emit a `vault_audit` telemetry event during bootstrap so we can prove secrets only ever live in the OS keychain. Keys are generated with `OsRng` and helper methods (`rotate`, `delete`) support recovery tests.
- **Telemetry buffer hardening**: the JSONL buffer now survives restarts, rotates when it breaches configurable size limits, and gracefully reports disk-full or permission failures without losing queued events. Behavior is covered by unit tests, and the buffer location appears in the runtime panel for inspection.
- **Tooling snapshot**: lint (`pnpm lint`), typecheck (`pnpm typecheck`), unit tests (`pnpm test`), and Rust coverage (`cargo test -p tauri-app`) are all wired and documented below so contributors can reproduce CI locally.

## Sprint 2 Highlights

- **Google OAuth device flow**: `google_start_device_flow` / `google_complete_sign_in` commands drive the desktop-friendly flow, storing access + refresh tokens in the OS keychain. The React shell guides users through the verification code and shows token lifetimes.
- **Drive picker & ingestion**: the UI now lists Drive files filtered to `application/vnd.google-earth.kml+xml`, lets users assign “List A/B”, and streams downloads through the Rust backend with progress notifications. Imports parse KML placemarks into normalized rows, hash every source + place identifier, and persist them into the `raw_items` table for later normalization.
- **Telemetry coverage**: signin, file selection, import start/completion, and every ingested row emit hashed identifiers into the offline telemetry queue so privacy is preserved even while offline.
- **QA harness**: `pnpm qa:drive` spins up a local HTTP stub so manual testing (or screenshots) doesn’t require real Google credentials.

## Development Workflow

Run these from the repository root:

- `pnpm dev` - Vite front-end in watch mode.
- `pnpm tauri dev` - full-stack desktop shell.
- `pnpm lint` / `pnpm typecheck` / `pnpm test` - JS/TS quality gates (also combined via `pnpm check`).
- `pnpm smoke` - one-touch harness used by CI (lint -> typecheck -> vitest run -> `cargo test -p tauri-app`).
- `pnpm qa:drive` - mock Google OAuth + Drive server that serves the `qa/*.kml` fixtures (wire your env vars to `http://localhost:8788` for stress-free manual flows).
- `cargo test -p tauri-app` - Rust unit tests (SQLCipher bootstrap, telemetry buffer, vault helpers).
- `cargo test -p tauri-app device_flow_and_import_roundtrip` - end-to-end test that exercises the mocked Drive API, token storage, KML parsing, and persistence pipeline.

> Tip: if you add new telemetry scenarios, keep JSONL lines small so the default 5 MiB rotation window isn't tripped unintentionally.

## Environment & Secrets

- `.env.development` is checked in with safe placeholder values so the desktop shell boots with telemetry enabled and stub API keys. Copy it to `.env.local` for custom tweaks.
- `.env` files are **only** loaded in debug/dev builds. Production binaries rely on real environment variables or the OS keychain. If you need `.env` during automated testing, opt-in via `ALLOW_DOTENV=1`.
- Database keys live under the OS keychain service `GoogleMapsListComparator` and are never written to disk. Deleting/corrupting the entry automatically forces a secure rebootstrap on next launch.
- Telemetry buffer defaults can be tuned with:
  - `TELEMETRY_BUFFER_MAX_BYTES` (default `5 * 1024 * 1024`)
  - `TELEMETRY_BUFFER_MAX_FILES` (default `5`, includes the live file)
- Drive import toggles read from:
  - `GOOGLE_OAUTH_CLIENT_ID` / `GOOGLE_OAUTH_CLIENT_SECRET`
  - `GOOGLE_DEVICE_CODE_ENDPOINT`, `GOOGLE_TOKEN_ENDPOINT`, `GOOGLE_USERINFO_ENDPOINT`
  - `GOOGLE_DRIVE_API_BASE` (defaults to `https://www.googleapis.com/drive/v3`)
  - `GOOGLE_DRIVE_PICKER_PAGE_SIZE` (defaults to `25` visible files)
  Configure those to point at the QA server when you don’t want to hit production Google APIs.

## Observability Aids

- The UI "Runtime status" section shows the encrypted DB path, telemetry buffer path, queue depth, bootstrap recovery status, and redacted key lifecycle so QA can quickly confirm health.
- Telemetry events (`vault_audit`, `app_start`, signin/drive/import, and per-row `raw_row_hashed`) drain to `telemetry-buffer.jsonl` and rotate to timestamped archives when full. JSONL makes it easy to `rg` or `jq` through the backlog while offline.
- A front-end telemetry adapter (see `src/telemetry/adapter.ts`) funnels UI events through a Tauri command that reuses the Rust buffer. Events are automatically throttled and flushed so future instrumentation can stay in TypeScript.

## Continuous Integration & Smoke Tests

- `.github/workflows/ci.yml` runs on every push/PR: it installs dependencies with pnpm, executes the smoke harness, then builds an unsigned Windows NSIS installer on `windows-latest`.
- The workflow uploads `src-tauri/target/release/bundle/nsis/*.exe` as an artifact named `tauri-windows-nsis`.
- The same `pnpm smoke` harness is available locally and supports `SMOKE_SKIP_JS=1` or `SMOKE_SKIP_RUST=1` if you need to focus on a subset of checks.

## IDE / Tooling

Any editor works; VS Code with the Tauri + rust-analyzer extensions provides the smoothest DX for React + Rust cross-over editing.
