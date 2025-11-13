import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { FoundationHealth } from "./types/foundation";
import "./App.css";
import { telemetry } from "./telemetry";
import type { ChecklistItem } from "./checklist";
import { resolveChecklist } from "./checklist";

const checklistTemplate: ChecklistItem[] = [
  {
    id: "shell",
    label: "Tauri shell scaffolding",
    status: "done",
    note: "React + TypeScript workspace wired to the Tauri backend.",
  },
  {
    id: "secure-storage",
    label: "Secure storage stub",
    status: "done",
    note: "OS keychain wrapper ready for encrypted secrets.",
  },
  {
    id: "sqlcipher",
    label: "SQLCipher bootstrap",
    status: "pending",
    note: "Encrypted database connection + migrations created on launch.",
  },
  {
    id: "telemetry",
    label: "Telemetry adapter",
    status: "pending",
    note: "Pluggable queue prepared for offline buffering.",
  },
  {
    id: "ci",
    label: "CI smoke tests",
    status: "pending",
    note: "GitHub Actions lint/test/build workflow artifacts on push.",
  },
];

const upcomingMilestones = [
  { title: "Google Identity", detail: "OAuth desktop sign-in & Drive picker" },
  {
    title: "KML ingestion",
    detail: "Layer parsing, hashing & raw row persistence",
  },
  { title: "Places normalization", detail: "Rate-limited resolver & cache" },
];

function App() {
  const [foundationHealth, setFoundationHealth] = useState<FoundationHealth | null>(
    null,
  );
  const [bootstrapError, setBootstrapError] = useState<string | null>(null);
  const mode = import.meta.env.DEV ? "DEV MODE" : "PRODUCTION BUILD";
  const isDevMode = import.meta.env.DEV;

  useEffect(() => {
    telemetry.track("ui_boot", { mode });
    return () => {
      void telemetry.flush();
    };
  }, [mode]);

  useEffect(() => {
    let mounted = true;
    invoke<FoundationHealth>("foundation_health")
      .then((health) => {
        if (mounted) {
          setFoundationHealth(health);
        }
      })
      .catch((error) => {
        if (mounted) {
          setBootstrapError(error?.message ?? String(error));
        }
      });

    return () => {
      mounted = false;
    };
  }, []);

  useEffect(() => {
    if (foundationHealth) {
      telemetry.setEnabled(foundationHealth.config.telemetry_enabled_by_default);
      telemetry.track(
        "foundation_health_loaded",
        {
          queueDepth: foundationHealth.telemetry_queue_depth,
          recovered: foundationHealth.db_bootstrap_recovered,
        },
        { flush: true },
      );
    }
  }, [foundationHealth]);

  useEffect(() => {
    if (bootstrapError) {
      telemetry.track(
        "foundation_bootstrap_failed",
        { reason: bootstrapError },
        { flush: true },
      );
    }
  }, [bootstrapError]);

  const checklist = useMemo(
    () =>
      resolveChecklist(checklistTemplate, {
        foundationHealth,
        isDevMode,
      }),
    [foundationHealth, isDevMode],
  );

  return (
    <main className="app-shell">
      <header className="hero">
        <div>
          <p className="eyebrow">Sprint 1 foundation in place</p>
          <h1>Google Maps List Comparator</h1>
          <p className="lede">
            Desktop toolkit for comparing personal Google My Maps layers with
            SQLCipher-protected storage and telemetry-conscious instrumentation.
          </p>
        </div>
        <span className="pill">{mode}</span>
      </header>

      <section className="feature-grid">
        {upcomingMilestones.map((item) => (
          <article key={item.title} className="feature-card">
            <div className="card-header">
              <span className="badge planned">Upcoming</span>
              <h2>{item.title}</h2>
            </div>
            <p>{item.detail}</p>
          </article>
        ))}
        <article className="feature-card">
          <div className="card-header">
            <span className="badge ready">Ready</span>
            <h2>Foundation</h2>
          </div>
          <p>
            Ready-to-run Tauri harness with linting, tests, and secure database
            lifecycle. Start Sprint 2 without revisiting the platform layer.
          </p>
        </article>
      </section>

      <section className="runtime-panel">
        <div className="panel-header">
          <h2>Runtime status</h2>
          <p>Live backend health pulled via the Tauri command interface.</p>
        </div>
        {foundationHealth && (
          <dl>
            <div>
              <dt>Encrypted DB path</dt>
              <dd>{foundationHealth.db_path}</dd>
            </div>
            <div>
              <dt>Telemetry buffer</dt>
              <dd>{foundationHealth.telemetry_buffer_path}</dd>
            </div>
            <div>
              <dt>Queue depth</dt>
              <dd>{foundationHealth.telemetry_queue_depth}</dd>
            </div>
            <div>
              <dt>Last bootstrap</dt>
              <dd>
                {foundationHealth.db_bootstrap_recovered
                  ? "Recovered secure store"
                  : "Clean start"}
              </dd>
            </div>
            <div>
              <dt>Key lifecycle</dt>
              <dd>{foundationHealth.db_key_lifecycle}</dd>
            </div>
            <div>
              <dt>Google Places key</dt>
              <dd>
                {foundationHealth.config.has_google_places_key
                  ? "Stored in secure memory"
                  : "Not configured"}
              </dd>
            </div>
          </dl>
        )}
        {!foundationHealth && !bootstrapError && (
          <p>Loading secure services&hellip;</p>
        )}
        {bootstrapError && (
          <p className="error-text">Bootstrap error: {bootstrapError}</p>
        )}
      </section>

      <section className="log-panel">
        <div className="panel-header">
          <h2>Engineering checklist</h2>
          <p>Tracks the key guardrails enforced in Sprint 1.</p>
        </div>
        <ul>
          {checklist.map((item) => (
            <li key={item.label}>
              <span className={`status-dot ${item.status}`} />
              <div>
                <p className="item-label">{item.label}</p>
                <p className="item-note">{item.note}</p>
              </div>
            </li>
          ))}
        </ul>
      </section>
    </main>
  );
}

export default App;
