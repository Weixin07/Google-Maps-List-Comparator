# Google Maps List Comparator – User Guide

This guide walks through the day-to-day workflow for running the desktop app, importing Google My Maps exports, refreshing Places data, exploring results, and preparing exports for downstream stakeholders.

## Requirements

- [pnpm](https://pnpm.io/) 8+
- Rust toolchain (installed automatically by Tauri)
- Google OAuth client credentials (device flow)
- Optional: Google Places API key + MapTiler (or compatible) map tiles key

## 1. Getting Started

1. **Install dependencies**
   ```bash
   pnpm install
   ```
2. **Provide environment variables**
   - Copy `.env.development` to `.env.local` for local-only secrets.
   - Set `GOOGLE_OAUTH_CLIENT_ID`, `GOOGLE_OAUTH_CLIENT_SECRET`, and the Google OAuth endpoints (or point them at the QA stub via `pnpm qa:drive`).
   - Add `GOOGLE_PLACES_API_KEY` if you want the real Places normalizer instead of the synthetic resolver.
3. **Run the desktop shell**
   ```bash
   pnpm tauri dev
   ```
4. **Sign in with Google Drive**
   - Click **Sign in with Google** in the Drive panel.
   - Approve the device-code request in your browser.
   - The UI shows the token expiry and lets you refresh the Drive file list any time.

> Tip: Running `pnpm qa:drive` gives you a local OAuth + Drive stub so you can test the entire import/refresh loop without touching production Google APIs.

## 2. Importing Lists

1. Pick a comparison project (or create a new one) from the **Comparison** panel.
2. In the Drive panel, assign a Drive KML export to **List A** and/or **List B**.
3. Click **Import to List A/B** to start the pipeline. The progress bar and copy update at every stage:
   - Download → Parse → Persist → Normalize → Complete
4. If a step fails you’ll see a red error panel with sanitized diagnostics and a **Retry this file** button. The “Retry failed imports” button handles every slot that landed in the error state.
5. Previous attempts appear under each list so you can confirm when rates/timeouts occur and how many rows were imported.

## 3. Refreshing Places Data

1. The **Refresh details** area now queues refresh jobs. Use:
   - **Queue both lists** (adds two jobs)
   - **Queue List A/B** (adds a single job)
   - **Pause queue** / **Resume queue** to throttle long batches
   - **Cancel active** to stop the current job mid-flight (the worker finishes the row it is on)
2. Each job card shows live progress (processed/total rows, resolved vs. pending) plus a colored status pill.
3. The queue respects the rate limit slider from the settings panel. Adjust it to match your Google Places quota.
4. Every job emits telemetry (`refresh_job_enqueued`, `refresh_job_completed`) with hashed identifiers so release builds can be monitored without leaking PII.

## 4. Exploring Comparisons

- **Tables**
  - Search across names, addresses, and types.
  - Use the new **Type** and **Category** dropdowns to zero in on restaurant, lodging, or service categories.
  - Keyboard navigation: focus inside the table (click or hit `Tab`), then use `↑/↓` to move through rows and `Enter` to recenter the map.
  - Checkboxes select visible rows and the “Clear selection” link resets the set per segment.
  - Filters and layer visibility are stored per project, so switching projects restores whichever view you left earlier.
- **Map**
  - Toggle segment layers to declutter the view.
  - Dense areas now cluster automatically; click a cluster to zoom into it. Counts show how many places are inside the cluster.
  - Selecting a row or cluster highlights and pans the map; selecting a marker highlights the corresponding table row.

## 5. Exporting Results

1. Choose CSV or JSON from the **Export format** dropdown.
2. Each segment (Shared/Only A/Only B) has an Export button. Exports respect any row selection you’ve made; otherwise visible filters are used.
3. The save dialog defaults to a slugged filename (`project-segment-format`) and remembers your last destination.

## 6. Telemetry & Privacy Controls

- The runtime settings panel exposes a telemetry toggle; disabling it clears the queue immediately.
- All Drive file IDs and Place IDs are hashed with a per-install salt before they hit the telemetry buffer.
- The buffer (`telemetry-buffer.jsonl`) rotates automatically and survives restarts. Use `pnpm tauri dev` → quit → inspect the file if you need to audit flows.

## 7. Troubleshooting Quick Hits

- **OAuth loop**: restart the device-code flow; codes expire within minutes. Check the console for “invalid device code” messages.
- **Import stuck on “Preparing”**: confirm you selected a project and the Drive panel shows a file ID. Errors now surface directly under each list with retry buttons.
- **Refresh queue idle**: ensure the queue isn’t paused and that at least one job is queued. The job card shows “Paused” when the global toggle is active.
- **Clusters never expand**: verify MapLibre can fetch your style URL (run with `pnpm qa:drive` to rule out network issues) and that the Places API key is correct if you expect real coordinates.

## 8. Preparing a Release

- Run `pnpm smoke` locally to make sure linting, type-checking, vitest, and Rust tests pass.
- Perform a manual flow with the QA Drive stub: import sample KML files, queue a few refresh jobs, confirm category filters and clustering behave.
- Review `telemetry-buffer.jsonl` for unexpected identifiers.
- Provide production secrets and run `pnpm tauri build` — NSIS installers land under `src-tauri/target/release/bundle/nsis`.
- Tag `vX.Y.Z` to trigger the CI workflow that builds/upload the installer artifact to a draft GitHub Release.
