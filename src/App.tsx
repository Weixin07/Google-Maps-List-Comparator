import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { save } from "@tauri-apps/plugin-dialog";
import { open } from "@tauri-apps/plugin-opener";
import type { FoundationHealth } from "./types/foundation";
import type {
  ComparisonProjectRecord,
  ComparisonSegmentKey,
  ComparisonSnapshot,
  ExportSummary,
  ListSlot,
  MapStyleDescriptor,
  PlaceComparisonRow,
} from "./types/comparison";
import { ComparisonTable, type TableFilters } from "./components/comparison/ComparisonTable";
import { ComparisonMap } from "./components/comparison/ComparisonMap";
import "./App.css";
import "maplibre-gl/dist/maplibre-gl.css";
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

type NormalizationStats = {
  slot: ListSlot;
  total_rows: number;
  cache_hits: number;
  places_calls: number;
  resolved: number;
  unresolved: number;
};

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

const segmentKeys: ComparisonSegmentKey[] = ["overlap", "only_a", "only_b"];

const segmentLabels: Record<ComparisonSegmentKey, string> = {
  overlap: "Shared places",
  only_a: "Only List A",
  only_b: "Only List B",
};

const segmentPropertyMap: Record<ComparisonSegmentKey, keyof ComparisonSnapshot> = {
  overlap: "overlap",
  only_a: "only_a",
  only_b: "only_b",
};

const segmentColors: Record<ComparisonSegmentKey, string> = {
  overlap: "#16a34a",
  only_a: "#0ea5e9",
  only_b: "#9333ea",
};

const initialFilters = (): Record<ComparisonSegmentKey, TableFilters> => ({
  overlap: { search: "", type: "", sortKey: "name", sortDirection: "asc" },
  only_a: { search: "", type: "", sortKey: "name", sortDirection: "asc" },
  only_b: { search: "", type: "", sortKey: "name", sortDirection: "asc" },
});

const initialSelections = (): Record<ComparisonSegmentKey, Set<string>> => ({
  overlap: new Set(),
  only_a: new Set(),
  only_b: new Set(),
});

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
  const [comparison, setComparison] = useState<ComparisonSnapshot | null>(null);
  const [comparisonError, setComparisonError] = useState<string | null>(null);
  const [isLoadingComparison, setIsLoadingComparison] = useState(false);
  const [isRefreshingPlaces, setIsRefreshingPlaces] = useState(false);
  const [projects, setProjects] = useState<ComparisonProjectRecord[]>([]);
  const [activeProjectId, setActiveProjectId] = useState<number | null>(null);
  const [isLoadingProjects, setIsLoadingProjects] = useState(false);
  const [newProjectName, setNewProjectName] = useState("");
  const [projectError, setProjectError] = useState<string | null>(null);
  const [filters, setFilters] =
    useState<Record<ComparisonSegmentKey, TableFilters>>(() => initialFilters());
  const [selections, setSelections] =
    useState<Record<ComparisonSegmentKey, Set<string>>>(() => initialSelections());
  const [layerVisibility, setLayerVisibility] = useState<
    Record<ComparisonSegmentKey, boolean>
  >({
    overlap: true,
    only_a: true,
    only_b: true,
  });
  const [mapStyleDescriptor, setMapStyleDescriptor] =
    useState<MapStyleDescriptor | null>(null);
  const [focusedPlaceId, setFocusedPlaceId] = useState<string | null>(null);
  const [focusPoint, setFocusPoint] = useState<{ lng: number; lat: number } | null>(
    null,
  );
  const [exportFormat, setExportFormat] = useState<"csv" | "json">("csv");
  const [exportStatus, setExportStatus] = useState<string | null>(null);
  const [exportingSegment, setExportingSegment] =
    useState<ComparisonSegmentKey | null>(null);
  const mode = import.meta.env.DEV ? "DEV MODE" : "PRODUCTION BUILD";
  const isDevMode = import.meta.env.DEV;

  const loadProjects = useCallback(async () => {
    setIsLoadingProjects(true);
    setProjectError(null);
    try {
      const records = await invoke<ComparisonProjectRecord[]>(
        "list_comparison_projects",
      );
      setProjects(records);
      const active = records.find((record) => record.is_active);
      setActiveProjectId(active?.id ?? records[0]?.id ?? null);
    } catch (error) {
      setProjectError(normalizeError(error));
    } finally {
      setIsLoadingProjects(false);
    }
  }, []);

  const loadComparison = useCallback(
    async (projectId?: number | null) => {
      if (!projectId) {
        setComparison(null);
        return;
      }
      setComparisonError(null);
      setIsLoadingComparison(true);
      try {
        const snapshot = await invoke<ComparisonSnapshot>("compare_lists", {
          projectId,
        });
        setComparison(snapshot);
      } catch (error) {
        setComparison(null);
        setComparisonError(normalizeError(error));
      } finally {
        setIsLoadingComparison(false);
      }
    },
    [],
  );

  const refreshPlaces = useCallback(
    async (slot?: ListSlot) => {
      if (!activeProjectId) {
        setComparisonError("Create or select a comparison project first.");
        return;
      }
      setIsRefreshingPlaces(true);
      setComparisonError(null);
      try {
        const payload = {
          projectId: activeProjectId,
          ...(slot ? { slot } : {}),
        };
        const stats = await invoke<NormalizationStats[]>(
          "refresh_place_details",
          payload,
        );
        const refreshed = stats.reduce((total, entry) => total + entry.resolved, 0);
        const pending = stats.reduce((total, entry) => total + entry.unresolved, 0);
        telemetry.track("places_refresh_triggered", {
          slot: slot ?? "both",
          refreshed,
          pending,
        });
        await loadComparison(activeProjectId);
      } catch (error) {
        setComparisonError(normalizeError(error));
      } finally {
        setIsRefreshingPlaces(false);
      }
    },
    [activeProjectId, loadComparison],
  );

  useEffect(() => {
    telemetry.track("ui_boot", { mode });
    return () => {
      void telemetry.flush();
    };
  }, [mode]);

  useEffect(() => {
    invoke<MapStyleDescriptor>("map_style_descriptor")
      .then((descriptor) => {
        setMapStyleDescriptor(descriptor);
      })
      .catch(() => {
        setMapStyleDescriptor({ style_url: null });
      });
  }, []);

  useEffect(() => {
    void loadProjects();
  }, [loadProjects]);

  useEffect(() => {
    let mounted = true;
    const subscription = listen<ImportProgressPayload>("import://progress", (event) => {
      if (!mounted || !event.payload) {
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
      if (event.payload.stage === "complete" && activeProjectId) {
        void loadComparison(activeProjectId);
      }
    });
    return () => {
      mounted = false;
      void subscription.then((unlisten) => unlisten());
    };
  }, [activeProjectId, loadComparison]);

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
    if (activeProjectId) {
      void loadComparison(activeProjectId);
    } else {
      setComparison(null);
    }
  }, [activeProjectId, loadComparison]);

  useEffect(() => {
    setFilters(initialFilters());
    setSelections(initialSelections());
    setFocusedPlaceId(null);
    setFocusPoint(null);
    setExportStatus(null);
    setSelectedFiles({ A: null, B: null });
    setImports({
      A: defaultImportState,
      B: defaultImportState,
    });
  }, [activeProjectId]);

  const handleFiltersChange = useCallback(
    (segment: ComparisonSegmentKey, nextFilters: TableFilters) => {
      setFilters((prev) => ({
        ...prev,
        [segment]: nextFilters,
      }));
    },
    [],
  );

  const handleSelectionChange = useCallback(
    (segment: ComparisonSegmentKey, placeIds: string[], checked: boolean) => {
      setSelections((prev) => {
        const next: Record<ComparisonSegmentKey, Set<string>> = {
          ...prev,
          [segment]: new Set(prev[segment]),
        };
        if (!checked && placeIds.length === 0) {
          next[segment].clear();
          return next;
        }
        placeIds.forEach((id) => {
          if (checked) {
            next[segment].add(id);
          } else {
            next[segment].delete(id);
          }
        });
        return next;
      });
    },
    [],
  );

  const handleRowFocus = useCallback(
    (_segment: ComparisonSegmentKey, row: PlaceComparisonRow) => {
      setFocusedPlaceId(row.place_id);
      setFocusPoint({ lng: row.lng, lat: row.lat });
    },
    [],
  );

  const handleMarkerFocus = useCallback(
    (placeId: string) => {
      if (!comparison) {
        return;
      }
      setFocusedPlaceId(placeId);
      const allRows = segmentKeys.flatMap(
        (segment) => comparison[segmentPropertyMap[segment]],
      ) as PlaceComparisonRow[];
      const match = allRows.find((row) => row.place_id === placeId);
      if (match) {
        setFocusPoint({ lng: match.lng, lat: match.lat });
      }
    },
    [comparison],
  );

  const handleLayerToggle = useCallback((segment: ComparisonSegmentKey) => {
    setLayerVisibility((prev) => ({
      ...prev,
      [segment]: !prev[segment],
    }));
  }, []);

  const handleProjectChange = useCallback(async (projectId: number) => {
    if (!Number.isFinite(projectId) || projectId <= 0) {
      return;
    }
    try {
      await invoke<ComparisonProjectRecord>("set_active_comparison_project", {
        projectId,
      });
      setActiveProjectId(projectId);
    } catch (error) {
      setProjectError(normalizeError(error));
    }
  }, []);

  const handleProjectCreate = useCallback(
    async (event: React.FormEvent<HTMLFormElement>) => {
      event.preventDefault();
      if (!newProjectName.trim()) {
        setProjectError("Enter a project name to continue.");
        return;
      }
      try {
        await invoke<ComparisonProjectRecord>("create_comparison_project", {
          name: newProjectName.trim(),
          activate: true,
        });
        setNewProjectName("");
        await loadProjects();
      } catch (error) {
        setProjectError(normalizeError(error));
      }
    },
    [loadProjects, newProjectName],
  );

  const typeOptions = useMemo(() => {
    const defaults: Record<ComparisonSegmentKey, string[]> = {
      overlap: [],
      only_a: [],
      only_b: [],
    };
    if (!comparison) {
      return defaults;
    }
    return segmentKeys.reduce((acc, segment) => {
      const baseRows = comparison[segmentPropertyMap[segment]] as PlaceComparisonRow[];
      const unique = new Set<string>();
      baseRows.forEach((row) => row.types.forEach((type) => unique.add(type)));
      acc[segment] = Array.from(unique).sort((a, b) => a.localeCompare(b));
      return acc;
    }, defaults);
  }, [comparison]);

  const totalCounts = useMemo(() => {
    const counts: Record<ComparisonSegmentKey, number> = {
      overlap: 0,
      only_a: 0,
      only_b: 0,
    };
    if (!comparison) {
      return counts;
    }
    segmentKeys.forEach((segment) => {
      counts[segment] = (
        comparison[segmentPropertyMap[segment]] as PlaceComparisonRow[]
      ).length;
    });
    return counts;
  }, [comparison]);

  const filteredRows = useMemo(() => {
    const defaults: Record<ComparisonSegmentKey, PlaceComparisonRow[]> = {
      overlap: [],
      only_a: [],
      only_b: [],
    };
    if (!comparison) {
      return defaults;
    }
    return segmentKeys.reduce((acc, segment) => {
      const baseRows = comparison[segmentPropertyMap[segment]] as PlaceComparisonRow[];
      const search = filters[segment].search.trim().toLowerCase();
      const typeFilter = filters[segment].type;
      acc[segment] = baseRows.filter((row) => {
        const matchesSearch =
          !search ||
          row.name.toLowerCase().includes(search) ||
          (row.formatted_address ?? "").toLowerCase().includes(search) ||
          row.types.some((type) => type.toLowerCase().includes(search));
        const matchesType = !typeFilter || row.types.includes(typeFilter);
        return matchesSearch && matchesType;
      });
      return acc;
    }, defaults);
  }, [comparison, filters]);

  const mapData = useMemo(() => {
    return segmentKeys.reduce((acc, segment) => {
      acc[segment] = filteredRows[segment];
      return acc;
    }, {} as Record<ComparisonSegmentKey, PlaceComparisonRow[]>);
  }, [filteredRows]);

  const selectedPlaceIds = useMemo(() => {
    const aggregate = new Set<string>();
    segmentKeys.forEach((segment) => {
      selections[segment].forEach((id) => aggregate.add(id));
    });
    return aggregate;
  }, [selections]);

  const handleExport = useCallback(
    async (segment: ComparisonSegmentKey) => {
      if (!comparison || !activeProjectId) {
        setComparisonError("Import data before exporting.");
        return;
      }
      const baseRows = comparison[segmentPropertyMap[segment]] as PlaceComparisonRow[];
      const visibleRows = filteredRows[segment];
      const selectedValues = Array.from(selections[segment]);
      const placeIds =
        selectedValues.length > 0
          ? selectedValues
          : visibleRows.length !== baseRows.length
            ? visibleRows.map((row) => row.place_id)
            : undefined;
      const defaultName = `${comparison.project.name}-${segment}-${exportFormat}.${exportFormat}`;
      const destination = await save({
        defaultPath: defaultName.replace(/\s+/g, "-").toLowerCase(),
        filters:
          exportFormat === "csv"
            ? [{ name: "CSV", extensions: ["csv"] }]
            : [{ name: "JSON", extensions: ["json"] }],
      });
      if (!destination) {
        return;
      }
      setExportStatus(null);
      setExportingSegment(segment);
      try {
        await invoke<ExportSummary>("export_comparison_segment", {
          projectId: activeProjectId,
          segment,
          format: exportFormat,
          destination,
          placeIds,
        });
        const exportedCount = placeIds?.length ?? visibleRows.length;
        setExportStatus(
          `Exported ${segmentLabels[segment]} (${exportedCount} places) to ${destination}`,
        );
      } catch (error) {
        setExportStatus(`Export failed: ${normalizeError(error)}`);
      } finally {
        setExportingSegment(null);
      }
    },
    [
      activeProjectId,
      comparison,
      exportFormat,
      filteredRows,
      selections,
    ],
  );

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
      if (!activeProjectId) {
        setComparisonError("Create or select a comparison project before importing.");
        return;
      }
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
          projectId: activeProjectId,
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
    [activeProjectId, selectedFiles],
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

      <section className="comparison-panel">
        <div className="panel-header">
          <div>
            <h2>Places comparison</h2>
            <p>Set math across Lists A & B using normalized Google Places identifiers.</p>
          </div>
          {comparison && (
            <span className="project-pill">Project · {comparison.project.name}</span>
          )}
        </div>
        <div className="project-toolbar">
          <div className="project-select">
            <label htmlFor="project-select">Active comparison</label>
            <select
              id="project-select"
              value={activeProjectId ?? ""}
              onChange={(event) => handleProjectChange(Number(event.target.value))}
              disabled={isLoadingProjects || projects.length === 0}
            >
              <option value="">Select a project</option>
              {projects.map((project) => (
                <option key={project.id} value={project.id}>
                  {project.name}
                </option>
              ))}
            </select>
          </div>
          <form className="project-create" onSubmit={handleProjectCreate}>
            <label htmlFor="new-project">New comparison</label>
            <div className="project-create__controls">
              <input
                id="new-project"
                type="text"
                value={newProjectName}
                placeholder="e.g. Coffee crawl"
                onChange={(event) => setNewProjectName(event.target.value)}
              />
              <button
                type="submit"
                className="secondary-button"
                disabled={newProjectName.trim().length === 0 || isLoadingProjects}
              >
                Create
              </button>
            </div>
          </form>
        </div>
        {projectError && <p className="error-text">{projectError}</p>}
        <div className="comparison-actions">
          <p className="muted">
            Pending lookups · List A: {comparison?.stats.pending_a ?? 0} · List B:{" "}
            {comparison?.stats.pending_b ?? 0}
          </p>
          <button
            type="button"
            className="secondary-button"
            onClick={() => refreshPlaces()}
            disabled={isRefreshingPlaces || !activeProjectId}
          >
            {isRefreshingPlaces ? "Refreshing Places…" : "Refresh details"}
          </button>
        </div>
        {comparisonError && <p className="error-text">{comparisonError}</p>}
        {isLoadingComparison && <p className="muted">Loading comparison snapshot…</p>}
        {!isLoadingComparison && comparison && (
          <>
            <dl className="comparison-stats">
              <div>
                <dt>List A</dt>
                <dd>{comparison.stats.list_a_count}</dd>
              </div>
              <div>
                <dt>List B</dt>
                <dd>{comparison.stats.list_b_count}</dd>
              </div>
              <div>
                <dt>Overlap</dt>
                <dd>{comparison.stats.overlap_count}</dd>
              </div>
              <div>
                <dt>Only A</dt>
                <dd>{comparison.stats.only_a_count}</dd>
              </div>
              <div>
                <dt>Only B</dt>
                <dd>{comparison.stats.only_b_count}</dd>
              </div>
            </dl>
            <div className="comparison-layout">
              <div className="comparison-tables">
                {segmentKeys.map((segment) => (
                  <ComparisonTable
                    key={segment}
                    segment={segment}
                    title={segmentLabels[segment]}
                    rows={filteredRows[segment]}
                    totalCount={totalCounts[segment]}
                    filters={filters[segment]}
                    availableTypes={typeOptions[segment]}
                    selectedIds={selections[segment]}
                    focusedPlaceId={focusedPlaceId}
                    onFiltersChange={handleFiltersChange}
                    onSelectionChange={handleSelectionChange}
                    onRowFocus={handleRowFocus}
                  />
                ))}
              </div>
              <div className="map-panel">
                <div className="map-toggles">
                  {segmentKeys.map((segment) => (
                    <label key={`toggle-${segment}`}>
                      <input
                        type="checkbox"
                        checked={layerVisibility[segment]}
                        onChange={() => handleLayerToggle(segment)}
                      />
                      <span style={{ color: segmentColors[segment] }}>
                        {segmentLabels[segment]}
                      </span>
                    </label>
                  ))}
                </div>
                <ComparisonMap
                  styleUrl={mapStyleDescriptor?.style_url ?? undefined}
                  data={mapData}
                  selectedIds={selectedPlaceIds}
                  focusedPlaceId={focusedPlaceId}
                  focusPoint={focusPoint}
                  visibility={layerVisibility}
                  onMarkerFocus={handleMarkerFocus}
                />
              </div>
            </div>
            <div className="comparison-export">
              <div className="export-controls">
                <label htmlFor="export-format">Export format</label>
                <select
                  id="export-format"
                  value={exportFormat}
                  onChange={(event) =>
                    setExportFormat(event.target.value === "json" ? "json" : "csv")
                  }
                >
                  <option value="csv">CSV</option>
                  <option value="json">JSON</option>
                </select>
              </div>
              <div className="export-buttons">
                {segmentKeys.map((segment) => (
                  <button
                    key={`export-${segment}`}
                    type="button"
                    className="secondary-button"
                    onClick={() => handleExport(segment)}
                    disabled={exportingSegment === segment}
                  >
                    {exportingSegment === segment
                      ? "Exporting…"
                      : `Export ${segmentLabels[segment]}`}
                  </button>
                ))}
              </div>
              {exportStatus && <p className="muted">{exportStatus}</p>}
            </div>
          </>
        )}
        {!isLoadingComparison && !comparison && (
          <p className="muted">Import Drive KML files to generate comparison insights.</p>
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
                      disabled={!selectedFiles[slot] || slotBusy(slot) || !activeProjectId}
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
