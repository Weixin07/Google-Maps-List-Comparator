# Google Maps List Comparator

Hardening pass for the Sprint 0 platform baseline: a Tauri + React desktop harness with SQLCipher storage, secure key handling, and telemetry buffering that future sprints can trust without revisiting infrastructure.

> Need day-to-day instructions? See [`docs/USER_GUIDE.md`](docs/USER_GUIDE.md) for a walkthrough of setup, Drive/OAuth sign-in, importing, refreshing, filtering, and exporting data.

## Platform Baseline

- **SQLCipher bootstrap + recovery**: on every launch we derive an encryption key from the OS keychain, run migrations, verify the file header is encrypted, and automatically recycle both the database file and key if the stored material is missing or corrupted. Recovery status and key lifecycle (`created`, `retrieved`, `rotated`) surface via the `FoundationHealth` UI/state API.
- **Secure storage audit trail**: vault operations log via `tracing` (no secrets) and emit a `vault_audit` telemetry event during bootstrap so we can prove secrets only ever live in the OS keychain. Keys are generated with `OsRng` and helper methods (`rotate`, `delete`) support recovery tests.
- **Telemetry buffer hardening**: the JSONL buffer now survives restarts, rotates when it breaches configurable size limits, and gracefully reports disk-full or permission failures without losing queued events. Behavior is covered by unit tests, and the buffer location appears in the runtime panel for inspection.
- **Tooling snapshot**: lint (`pnpm lint`), typecheck (`pnpm typecheck`), unit tests (`pnpm test`), and Rust coverage (`cargo test -p tauri-app`) are all wired and documented below so contributors can reproduce CI locally.

## Sprint 2 Highlights

- **Google OAuth device flow**: `google_start_device_flow` / `google_complete_sign_in` commands drive the desktop-friendly flow, storing access + refresh tokens in the OS keychain. The React shell guides users through the verification code and shows token lifetimes.
- **Drive picker & ingestion**: the UI now lists Drive files filtered to `application/vnd.google-earth.kml+xml`, lets users assign "List A/B", and streams downloads through the Rust backend with progress notifications. Imports parse KML placemarks into normalized rows, hash every source + place identifier, and persist them into the `raw_items` table for later normalization.
- **Telemetry coverage**: signin, file selection, import start/completion, and every ingested row emit hashed identifiers into the offline telemetry queue so privacy is preserved even while offline.
- **QA harness**: `pnpm qa:drive` spins up a local HTTP stub so manual testing (or screenshots) doesn't require real Google credentials.

## Sprint 3 Highlights

- **Places normalization queue**: rows missing a `place_id` are funneled through a single-threaded queue that honors a 3 QPS budget, exponential backoff, and jitter. Lookups hit `normalization_cache` before reusing persisted `places` rows; only truly unknown rows trigger the Places Search API (or the deterministic synthetic resolver during local dev). `list_places` timestamps are rewritten atomically so downstream comparison math stays in sync.
- **Manual refresh**: the new `refresh_place_details` Tauri command reuses the same queue logic and surfaces in the UI as a "Refresh details" action. Telemetry now includes per-import stats for total rows, cache hits, Places calls, and pending lookups so rate limiters/regressions are easy to spot.
- **Comparison engine**: a `compare_lists` command computes overlap, A-only, and B-only sets directly from the normalized DB state, including pending counts derived from `raw_items`. React renders the snapshot with live counts and the top normalized places for each partition so QA can see deterministic results immediately after an import.

## Sprint 4 Highlights

- **Settings control plane**: the settings panel now surfaces SQLCipher lifecycle data, telemetry buffer paths, and drive config health while exposing a runtime telemetry toggle and Places API rate-limit slider wired to the Rust normalizer.
- **Telemetry uplink**: the TypeScript telemetry adapter hashes `place_id` values with a per-install salt, falls back to the Tauri queue when offline, and can stream buffered batches to any PostHog-compatible endpoint.
- **Packaging polish**: the NSIS template now ships the real product name/icons, embeds the WebView2 bootstrapper, and the README documents the unsigned installer expectations plus how secrets flow into `pnpm tauri build`.
- **CI/CD release hooks**: GitHub Actions lint/type/test, build the Windows installer, upload it as an artifact, and attach it to a draft GitHub Release whenever a `v*` tag lands on `main`.

## Sprint 5 Highlights

- **Import resilience**: Drive imports now stream granular progress updates, surface detailed error diagnostics per file, and offer both per-slot and "retry all failed" controls. Logs reference hashed identifiers only so Drive file IDs never leak.
- **Bulk refresh queue**: refreshing Google Places data is now a queued operation with rate-limit aware progress bars, telemetry for completion vs. cancellation, and dedicated pause/cancel controls. The UI happily works through dozens of refresh jobs without overwhelming the Places API.
- **Table & map polish**: comparison tables add keyboard navigation, category filters, and per-project persistence so filters and map layer visibility survive context switches. The map clusters dense markers and labels cluster counts for clearer exploration.
- **Docs & support**: a user guide walks through setup, OAuth, importing, refreshing, and exporting. README sections cover troubleshooting, telemetry/privacy, and a release checklist so anyone can prep the app for distribution.

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
- Keychain prerequisites: keep your login keyring/keychain unlocked (or a reasonable Secret Service running on Linux) so the SQLCipher key can be created and read. Errors surface in `foundation_health` with the recovered flag and lifecycle string.
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

## Installer Packaging

- `src-tauri/tauri.conf.json` now carries the real product name, NSIS metadata, installer icon, and WebView2 bootstrapper so the Windows installer feels intentional.
- Builds are unsigned on purpose; expect SmartScreen prompts until you provide `TAURI_PRIVATE_KEY`/`TAURI_KEY_PASSWORD` via the environment or CI secrets for code signing.
- Supply secrets (e.g. `GOOGLE_PLACES_API_KEY`, `MAPTILER_API_KEY`, `TELEMETRY_ENDPOINT`) via environment variables before running `pnpm tauri build`. The GitHub Actions workflow passes through repository secrets automatically.
- Pushing a release tag (`v*`) triggers a draft GitHub Release with the NSIS artifact attached so QA can download the installer straight from Actions output.

## Continuous Integration & Smoke Tests

- `.github/workflows/ci.yml` runs on every push/PR: it installs dependencies with pnpm, executes the smoke harness, then builds an unsigned Windows NSIS installer on `windows-latest`.
- The workflow uploads `src-tauri/target/release/bundle/nsis/*.exe` as an artifact named `tauri-windows-nsis`.
- The same `pnpm smoke` harness is available locally and supports `SMOKE_SKIP_JS=1` or `SMOKE_SKIP_RUST=1` if you need to focus on a subset of checks.

## IDE / Tooling

Any editor works; VS Code with the Tauri + rust-analyzer extensions provides the smoothest DX for React + Rust cross-over editing.

## Setup & Configuration

1. **Install dependencies**: `pnpm install` pulls the front-end workspace while `cargo` is fetched automatically via Tauri when you run the desktop shell.
2. **Populate `.env`**: copy `.env.development` to `.env.local` for secrets you do not want checked in. At minimum set `GOOGLE_OAUTH_CLIENT_ID/SECRET`, `GOOGLE_DEVICE_CODE_ENDPOINT`, `GOOGLE_TOKEN_ENDPOINT`, and `GOOGLE_DRIVE_API_BASE` (point it at the QA stub via `pnpm qa:drive` when iterating).
3. **Provide Places API keys**: set `GOOGLE_PLACES_API_KEY` so the Places normalizer can reach Google’s API, or rely on the synthetic resolver for offline tests.
4. **Run the app**: `pnpm tauri dev` starts the native shell; the Drive panel will block you from importing until OAuth is configured.
5. **Persist preferences**: table filters and map layer visibility are saved per comparison project, so feel free to tune filters knowing they will return when you hop back to a project.

## OAuth & API Keys

- **Device flow**: the Drive panel uses Google’s OAuth device flow. When you click “Sign in with Google” we open the verification URL in your default browser and poll until you approve the request. No secrets ever sit in the repo; the token lives in the OS keychain.
- **Drive scope**: imports solely request `drive.readonly`, scoped to KML files. Every file selection and import emits hashed IDs to telemetry so we can trace behavior without exfiltrating Drive IDs.
- **Places API**: the refresh queue honors the `places_rate_limit_qps` value surfaced in the settings panel. Update it in-app or via `RuntimeSettings` to match your quota.
- **Map tiles**: configure `MAPTILER_API_KEY` (or another MapLibre-compatible style URL) via `map_style_descriptor` to render custom basemaps.

## Telemetry & Privacy

- **Hashing everywhere**: Drive file IDs and Place IDs are salted before they leave the client. Both the Tauri telemetry buffer and the optional network uploader only see hashed identifiers.
- **Offline buffer**: telemetry events (signin, import start/completion, refresh queue lifecycle, errors) drain into a JSONL buffer capped at the configured rotation window. Inspect `telemetry-buffer.jsonl` anytime you need to audit behavior.
- **Runtime toggle**: the settings pane exposes a telemetry toggle. Disabling it flushes the in-memory queue and the UI respects the setting immediately.
- **Event hints**: new events include `refresh_job_enqueued/refresh_job_completed`, `import_failed`, and per-row hashing for Drive imports so flaky flows can be diagnosed without raw IDs.

## Troubleshooting

- **Drive sign-in never completes**: ensure the device-code page displays the correct client ID; if you see an “invalid device code” error, restart the flow—codes expire quickly. Running `pnpm qa:drive` locally is handy for testing without hitting production Google endpoints.
- **Imports show “Select a Drive KML” even with a file selected**: check that your comparison project is active. Imports are project-scoped; the UI now surfaces per-slot errors with retry buttons so you can recover without reloading the app.
- **Refresh queue won’t start**: the queue pauses automatically when you click “Pause queue” or when no projects are active. Resume the queue or enqueue a new job. The “Cancel active” button sends a cancellation signal to the backend; status changes to “cancelled” once the in-flight row completes.
- **Clusters never expand**: cluster circles require at least two points. If clicking a cluster does nothing, ensure MapLibre can fetch the style URL (check your API key) and that `comparison-clusters` layer is visible (toggle one of the segment checkboxes back on).
- **Telemetry buffer errors**: if you see `telemetry upload failed` in the console, double-check write permissions for the data directory or run `pnpm tauri dev` from a writable location. Buffer rotations fall back to truncation if disk-space is exhausted.

## Release Checklist

1. **Sync secrets**: provide production `GOOGLE_*` credentials, `GOOGLE_PLACES_API_KEY`, and any map tiles keys via the environment or CI secrets.
2. **Smoke test**: run `pnpm smoke` locally to exercise lint, type-check, vitest, and the Rust test suite.
3. **Manual flows**: with the QA Drive stub running (`pnpm qa:drive`), take screenshots of the Drive import, refresh queue, and map clustering surfaces for release notes.
4. **Inspect telemetry buffer**: confirm no unexpected identifiers show up in `telemetry-buffer.jsonl` and that hashed IDs are present.
5. **Package & tag**: `pnpm tauri build` generates the NSIS installer; tag `v*` to trigger the CI workflow that uploads artifacts to a draft GitHub Release along with the updated README + user guide link.
