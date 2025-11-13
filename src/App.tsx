import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-opener";
import type { FoundationHealth } from "./types/foundation";
import "./App.css";
import { telemetry } from "./telemetry";
import type { ChecklistItem } from "./checklist";
import { resolveChecklist } from "./checklist";

type DeviceFlowState = {
  device_code: string;
  user_code: string;
  verification_url: string;
  expires_at: string;
  interval_secs: number;
};

type GoogleIdentity = {
  email: string;
  name?: string | null;
  picture?: string | null;
  expires_at: string;
};

type DriveFileMetadata = {
  id: string;
  name: string;
  mime_type: string;
  modified_time?: string | null;
  size?: number | null;
};

type ImportProgressPayload = {
  slot: string;
  stage: string;
  message: string;
  progress: number;
  error?: string | null;
  file_name?: string | null;
};

type ImportState = {
  stage: string;
  message: string;
  progress: number;
  fileName?: string;
  error?: string;
};

type ListSlot = "A" | "B";

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

const defaultImportState: ImportState = {
  stage: "idle",
  message: "Waiting for Drive selection",
  progress: 0,
};

const listSlots: ListSlot[] = ["A", "B"];

function App() {
  const [foundationHealth, setFoundationHealth] = useState<FoundationHealth | null>(
    null,
  );
  const [bootstrapError, setBootstrapError] = useState<string | null>(null);
  const [deviceFlow, setDeviceFlow] = useState<DeviceFlowState | null>(null);
  const [identity, setIdentity] = useState<GoogleIdentity | null>(null);
  const [signInError, setSignInError] = useState<string | null>(null);
  const [isRequestingCode, setIsRequestingCode] = useState(false);
  const [isCompletingSignIn, setIsCompletingSignIn] = useState(false);
  const [driveFiles, setDriveFiles] = useState<DriveFileMetadata[]>([]);
  const [pickerError, setPickerError] = useState<string | null>(null);
  const [isLoadingFiles, setIsLoadingFiles] = useState(false);
  const [selectedFiles, setSelectedFiles] = useState<Record<ListSlot, DriveFileMetadata | null>>({
    A: null,
    B: null,
  });
  const [imports, setImports] = useState<Record<ListSlot, ImportState>>({
    A: defaultImportState,
    B: defaultImportState,
  });
  const mode = import.meta.env.DEV ? "DEV MODE" : "PRODUCTION BUILD";
  const isDevMode = import.meta.env.DEV;

  useEffect(() => {
    telemetry.track("ui_boot", { mode });
    return () => {
      void telemetry.flush();
    };
  }, [mode]);

  useEffect(() => {
    const subscription = listen<ImportProgressPayload>("import://progress", (event) => {
      if (!event.payload) {
        return;
      }
      const slot: ListSlot = event.payload.slot?.toUpperCase() === "B" ? "B" : "A";
      setImports((prev) => ({
        ...prev,
        [slot]: {
          stage: event.payload.stage,
          message: event.payload.message,
          progress: event.payload.progress,
          fileName: event.payload.file_name ?? prev[slot].fileName,
          error: event.payload.error ?? undefined,
        },
      }));
    });
    return () => {
      void subscription.then((unlisten) => unlisten());
    };
  }, []);

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

  const driveEnabled = foundationHealth?.config.drive_import_enabled ?? false;

  const requestDeviceFlow = useCallback(async () => {
    if (!driveEnabled) {
      setSignInError("Drive import is disabled in this build.");
      return;
    }
    setIsRequestingCode(true);
    setSignInError(null);
    try {
      const flow = await invoke<DeviceFlowState>("google_start_device_flow");
      setDeviceFlow(flow);
      await open(flow.verification_url);
    } catch (error) {
      setSignInError(normalizeError(error));
    } finally {
      setIsRequestingCode(false);
    }
  }, [driveEnabled]);

  const completeDeviceFlow = useCallback(async () => {
    if (!deviceFlow) {
      return;
    }
    setIsCompletingSignIn(true);
    setSignInError(null);
    try {
      const result = await invoke<GoogleIdentity>("google_complete_sign_in", {
        deviceCode: deviceFlow.device_code,
        intervalSecs: deviceFlow.interval_secs,
      });
      setIdentity(result);
      telemetry.track("signin_success", { expiresAt: result.expires_at });
    } catch (error) {
      setSignInError(normalizeError(error));
    } finally {
      setIsCompletingSignIn(false);
    }
  }, [deviceFlow]);

  const loadDriveFiles = useCallback(async () => {
    if (!identity) {
      return;
    }
    setPickerError(null);
    setIsLoadingFiles(true);
    try {
      const files = await invoke<DriveFileMetadata[]>("drive_list_kml_files", {
        limit: foundationHealth?.config.drive_picker_page_size,
      });
      setDriveFiles(files);
    } catch (error) {
      setPickerError(normalizeError(error));
    } finally {
      setIsLoadingFiles(false);
    }
  }, [identity, foundationHealth]);

  useEffect(() => {
    if (!identity) {
      return;
    }
    void loadDriveFiles();
  }, [identity, loadDriveFiles]);

  const handleFileSelection = useCallback(
    (slot: ListSlot, fileId: string) => {
      const file = driveFiles.find((entry) => entry.id === fileId) ?? null;
      setSelectedFiles((prev) => ({
        ...prev,
        [slot]: file,
      }));
      if (file) {
        void hashIdentifier(file.id).then((hash) => {
          telemetry.track("drive_file_selected", { slot, fileHash: hash });
        });
      }
    },
    [driveFiles],
  );

  const handleImport = useCallback(
    async (slot: ListSlot) => {
      const file = selectedFiles[slot];
      if (!file) {
        setImports((prev) => ({
          ...prev,
          [slot]: {
            ...defaultImportState,
            message: "Select a Drive KML before importing",
          },
        }));
        return;
      }

      setImports((prev) => ({
        ...prev,
        [slot]: {
          stage: "starting",
          message: "Preparing import…",
          progress: 0,
          fileName: file.name,
        },
      }));

      const fileHashPromise = hashIdentifier(file.id).catch(() => null);
      void fileHashPromise.then((hash) => {
        if (hash) {
          telemetry.track("import_started", { slot, fileHash: hash, fileName: file.name });
        }
      });

      try {
        await invoke("drive_import_kml", {
          slot,
          fileId: file.id,
          fileName: file.name,
        });
        const hash = await fileHashPromise;
        if (hash) {
          telemetry.track("import_completed", {
            slot,
            fileHash: hash,
            fileName: file.name,
          });
        }
      } catch (error) {
        const message = normalizeError(error);
        setImports((prev) => ({
          ...prev,
          [slot]: {
            ...prev[slot],
            stage: "error",
            message: "Import failed",
            error: message,
          },
        }));
      }
    },
    [selectedFiles],
  );

  const slotBusy = (slot: ListSlot) => {
    const stage = imports[slot].stage;
    return !["idle", "complete", "error"].includes(stage);
  };

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

      <section className="drive-panel">
        <div className="panel-header">
          <h2>Drive identity & ingestion</h2>
          <p>Sign in, pick “List A/B” exports, and stream hashed rows into SQLCipher.</p>
        </div>
        <div className="drive-grid">
          <article className="identity-card">
            {!driveEnabled && (
              <p className="error-text">
                Drive OAuth credentials are missing. Define GOOGLE_OAUTH_CLIENT_ID / SECRET to enable this surface.
              </p>
            )}
            {driveEnabled && !identity && (
              <>
                <p>
                  Use the OAuth device flow to approve access from your default browser; no embedded client secrets required.
                </p>
                <button
                  type="button"
                  className="primary-button"
                  onClick={requestDeviceFlow}
                  disabled={isRequestingCode}
                >
                  {isRequestingCode ? "Requesting code…" : "Sign in with Google"}
                </button>
              </>
            )}
            {signInError && <p className="error-text">{signInError}</p>}
            {deviceFlow && !identity && (
              <div className="device-flow">
                <p>
                  Enter code <code>{deviceFlow.user_code}</code> at{" "}
                  <button
                    type="button"
                    className="link-button"
                    onClick={() => open(deviceFlow.verification_url)}
                  >
                    google.com/device
                  </button>{" "}
                  to approve Drive scope.
                </p>
                <button
                  type="button"
                  className="secondary-button"
                  onClick={completeDeviceFlow}
                  disabled={isCompletingSignIn}
                >
                  {isCompletingSignIn ? "Waiting for approval…" : "I approved the request"}
                </button>
              </div>
            )}
            {identity && (
              <div className="identity-summary">
                <p>
                  Signed in as <strong>{identity.email}</strong>
                </p>
                <p className="muted">
                  Token expires{" "}
                  {new Date(identity.expires_at).toLocaleString(undefined, {
                    hour: "2-digit",
                    minute: "2-digit",
                    month: "short",
                    day: "numeric",
                  })}
                </p>
                <button
                  type="button"
                  className="secondary-button"
                  onClick={loadDriveFiles}
                  disabled={isLoadingFiles}
                >
                  {isLoadingFiles ? "Refreshing files…" : "Reload Drive files"}
                </button>
              </div>
            )}
          </article>

          <article className="file-card">
            {!identity && <p>Sign in to browse Google Drive KML exports.</p>}
            {pickerError && <p className="error-text">{pickerError}</p>}
            {identity && (
              <div className="list-grid">
                {listSlots.map((slot) => (
                  <div key={slot} className="list-card">
                    <div className="list-card__header">
                      <h3>List {slot}</h3>
                      <span className="list-card__count">
                        {driveFiles.length} file{driveFiles.length === 1 ? "" : "s"}
                      </span>
                    </div>
                    <label className="field-label" htmlFor={`slot-${slot}`}>
                      Drive KML
                    </label>
                    <select
                      id={`slot-${slot}`}
                      value={selectedFiles[slot]?.id ?? ""}
                      onChange={(event) => handleFileSelection(slot, event.target.value)}
                      disabled={driveFiles.length === 0 || isLoadingFiles}
                    >
                      <option value="">Select a file</option>
                      {driveFiles.map((file) => (
                        <option key={`${slot}-${file.id}`} value={file.id}>
                          {file.name}
                          {file.modified_time
                            ? ` (${new Date(file.modified_time).toLocaleDateString()})`
                            : ""}
                        </option>
                      ))}
                    </select>
                    <button
                      type="button"
                      className="primary-button"
                      onClick={() => handleImport(slot)}
                      disabled={!selectedFiles[slot] || slotBusy(slot)}
                    >
                      {slotBusy(slot) ? "Importing…" : `Import to List ${slot}`}
                    </button>
                    <div className="progress-track">
                      <div
                        className="progress-bar"
                        style={{ width: `${Math.round(imports[slot].progress * 100)}%` }}
                      />
                    </div>
                    <p className="progress-copy">{imports[slot].message}</p>
                    {imports[slot].error && (
                      <p className="error-text">{imports[slot].error}</p>
                    )}
                  </div>
                ))}
              </div>
            )}
          </article>
        </div>
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

function normalizeError(error: unknown): string {
  if (error instanceof Error) {
    return error.message;
  }
  if (typeof error === "string") {
    return error;
  }
  return "Unexpected error";
}

async function hashIdentifier(value: string): Promise<string> {
  try {
    if (window.crypto?.subtle) {
      const bytes = new TextEncoder().encode(value);
      const digest = await window.crypto.subtle.digest("SHA-256", bytes);
      return bytesToBase64(new Uint8Array(digest));
    }
  } catch {
    // fall through to fallback encoder
  }
  return bytesToBase64(new TextEncoder().encode(value));
}

function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  bytes.forEach((byte) => {
    binary += String.fromCharCode(byte);
  });
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}
