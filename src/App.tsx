import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type React from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { save } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import type { FoundationHealth, RuntimeSettings } from "./types/foundation";
import type {
  ComparisonProjectRecord,
  ComparisonSegmentKey,
  ComparisonSegmentPage,
  ComparisonSnapshot,
  ExportSummary,
  ListSlot,
  MapStyleDescriptor,
  PlaceComparisonRow,
} from "./types/comparison";
import type { DriveFileMetadata } from "./types/drive";
import { ComparisonTable, type TableFilters } from "./components/comparison/ComparisonTable";
import { ComparisonMap } from "./components/comparison/ComparisonMap";
import "./App.css";
import "maplibre-gl/dist/maplibre-gl.css";
import { telemetry } from "./telemetry";
import type { ChecklistItem } from "./checklist";
import { resolveChecklist } from "./checklist";

type LoopbackFlowState = {
  authorization_url: string;
  redirect_url: string;
  expires_at: string;
};

type GoogleIdentity = {
  email: string;
  name?: string | null;
  picture?: string | null;
  expires_at: string;
};

type ImportProgressPayload = {
  slot: string;
  stage: string;
  message: string;
  progress: number;
  error?: string | null;
  file_name?: string | null;
  details?: string[] | null;
  processed_rows?: number | null;
  total_rows?: number | null;
  rejected_rows?: number | null;
  bytes_downloaded?: number | null;
  expected_bytes?: number | null;
  checksum?: string | null;
};

type RefreshProgressPayload = {
  slot: string;
  request_id?: string | null;
  stage: string;
  processed: number;
  total_rows: number;
  resolved: number;
  pending: number;
  rate_limit_qps: number;
  message: string;
};

type RefreshJobStatus = "queued" | "running" | "complete" | "cancelled" | "error";

type RefreshJob = {
  id: string;
  slot: ListSlot;
  status: RefreshJobStatus;
  processed: number;
  total: number;
  resolved: number;
  pending: number;
  message: string;
  startedAt?: number;
  finishedAt?: number;
};

type ImportAttemptRecord = {
  id: string;
  fileName?: string;
  finishedAt: number;
  status: "success" | "error";
  summary: string;
  details?: string[];
};

type ImportState = {
  stage: string;
  message: string;
  progress: number;
  fileName?: string;
  error?: string;
  errorDetails?: string[];
  attemptId?: string;
  startedAt?: number;
  processedRows?: number;
  totalRows?: number;
  rejectedRows?: number;
  downloadedBytes?: number;
  expectedBytes?: number;
  checksum?: string;
  history: ImportAttemptRecord[];
};

type NormalizationStats = {
  slot: ListSlot;
  total_rows: number;
  cache_hits: number;
  cache_misses: number;
  stale_cache: number;
  places_calls: number;
  resolved: number;
  unresolved: number;
  places_counters: {
    total_requests: number;
    successes: number;
    quota_errors: number;
    invalid_key_errors: number;
    network_errors: number;
    other_errors: number;
  };
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

const createImportState = (): ImportState => ({
  stage: "idle",
  message: "Waiting for Drive selection",
  progress: 0,
  history: [],
});

const listSlots: ListSlot[] = ["A", "B"];

const segmentKeys: ComparisonSegmentKey[] = ["overlap", "only_a", "only_b"];

type SegmentPropertyKey = "overlap" | "only_a" | "only_b";

const segmentLabels: Record<ComparisonSegmentKey, string> = {
  overlap: "Shared places",
  only_a: "Only List A",
  only_b: "Only List B",
};

const segmentPropertyMap: Record<ComparisonSegmentKey, SegmentPropertyKey> = {
  overlap: "overlap",
  only_a: "only_a",
  only_b: "only_b",
};

const segmentColors: Record<ComparisonSegmentKey, string> = {
  overlap: "#16a34a",
  only_a: "#0ea5e9",
  only_b: "#9333ea",
};

const DEFAULT_PAGE_SIZE = 200;
const defaultSegmentPages: Record<ComparisonSegmentKey, number> = {
  overlap: 1,
  only_a: 1,
  only_b: 1,
};
const defaultSegmentLoading: Record<ComparisonSegmentKey, boolean> = {
  overlap: false,
  only_a: false,
  only_b: false,
};
const defaultLayerVisibility: Record<ComparisonSegmentKey, boolean> = {
  overlap: true,
  only_a: true,
  only_b: true,
};

type PersistedPreferences = {
  filters: Record<ComparisonSegmentKey, TableFilters>;
  layerVisibility: Record<ComparisonSegmentKey, boolean>;
};

const preferenceKey = (projectId: number) => `gmlc-preferences-${projectId}`;

const loadPreferences = (projectId: number): PersistedPreferences | null => {
  if (typeof window === "undefined" || !window.localStorage) {
    return null;
  }
  try {
    const raw = window.localStorage.getItem(preferenceKey(projectId));
    if (!raw) {
      return null;
    }
    const parsed = JSON.parse(raw) as PersistedPreferences;
    return parsed;
  } catch {
    return null;
  }
};

const formatTimestamp = (value?: string | null) => {
  if (!value) {
    return "Never compared";
  }
  const parsed = new Date(value);
  if (Number.isNaN(parsed.getTime())) {
    return value;
  }
  return parsed.toLocaleString();
};

const derivedCategories = [
  { label: "Food & Drink", matches: ["restaurant", "food", "cafe", "bar", "bakery"] },
  { label: "Coffee & Tea", matches: ["coffee", "tea"] },
  { label: "Shopping", matches: ["store", "shopping", "market", "mall"] },
  { label: "Lodging", matches: ["lodging", "hotel", "motel", "hostel"] },
  { label: "Outdoors", matches: ["park", "trail", "campground", "beach"] },
  { label: "Arts & Culture", matches: ["art", "museum", "gallery", "library"] },
  { label: "Services", matches: ["service", "bank", "atm", "post_office"] },
  { label: "Entertainment", matches: ["theater", "stadium", "amusement"] },
];

const resolveCategory = (types: string[]): string => {
  if (types.length === 0) {
    return "Uncategorized";
  }
  const lower = types.map((type) => type.toLowerCase());
  for (const group of derivedCategories) {
    if (lower.some((type) => group.matches.some((keyword) => type.includes(keyword)))) {
      return group.label;
    }
  }
  return "Other";
};

const initialFilters = (): Record<ComparisonSegmentKey, TableFilters> => ({
  overlap: { search: "", type: "", category: "", sortKey: "name", sortDirection: "asc" },
  only_a: { search: "", type: "", category: "", sortKey: "name", sortDirection: "asc" },
  only_b: { search: "", type: "", category: "", sortKey: "name", sortDirection: "asc" },
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
  const [loopbackFlow, setLoopbackFlow] = useState<LoopbackFlowState | null>(null);
  const [identity, setIdentity] = useState<GoogleIdentity | null>(null);
  const [signInError, setSignInError] = useState<string | null>(null);
  const [isRequestingCode, setIsRequestingCode] = useState(false);
  const [isCompletingSignIn, setIsCompletingSignIn] = useState(false);
  const [isRestoringIdentity, setIsRestoringIdentity] = useState(false);
  const [driveFiles, setDriveFiles] = useState<DriveFileMetadata[]>([]);
  const [pickerError, setPickerError] = useState<string | null>(null);
  const [isLoadingFiles, setIsLoadingFiles] = useState(false);
  const [fileQuery, setFileQuery] = useState("");
  const [fileTypeFilter, setFileTypeFilter] = useState<"all" | "kml" | "map">(
    "all",
  );
  const [selectedFiles, setSelectedFiles] = useState<Record<ListSlot, DriveFileMetadata | null>>({
    A: null,
    B: null,
  });
  const [selectionErrors, setSelectionErrors] = useState<
    Record<ListSlot, string | null>
  >({
    A: null,
    B: null,
  });
  const [imports, setImports] = useState<Record<ListSlot, ImportState>>(() => ({
    A: createImportState(),
    B: createImportState(),
  }));
  const [runtimeSettings, setRuntimeSettings] = useState<RuntimeSettings | null>(null);
  const [pendingRateLimit, setPendingRateLimit] = useState<number | null>(null);
  const [settingsError, setSettingsError] = useState<string | null>(null);
  const [isUpdatingSettings, setIsUpdatingSettings] = useState(false);
  const [comparison, setComparison] = useState<ComparisonSnapshot | null>(null);
  const [comparisonError, setComparisonError] = useState<string | null>(null);
  const [isLoadingComparison, setIsLoadingComparison] = useState(false);
  const [refreshQueue, setRefreshQueue] = useState<RefreshJob[]>([]);
  const [isRefreshPaused, setIsRefreshPaused] = useState(false);
  const refreshRunnerRef = useRef<string | null>(null);
  const [refreshError, setRefreshError] = useState<string | null>(null);
  const [projects, setProjects] = useState<ComparisonProjectRecord[]>([]);
  const [activeProjectId, setActiveProjectId] = useState<number | null>(null);
  const [isLoadingProjects, setIsLoadingProjects] = useState(false);
  const [newProjectName, setNewProjectName] = useState("");
  const [renameProjectName, setRenameProjectName] = useState("");
  const [isRenamingProject, setIsRenamingProject] = useState(false);
  const [projectError, setProjectError] = useState<string | null>(null);
  const [filters, setFilters] =
    useState<Record<ComparisonSegmentKey, TableFilters>>(() => initialFilters());
  const [selections, setSelections] =
    useState<Record<ComparisonSegmentKey, Set<string>>>(() => initialSelections());
  const [segmentPages, setSegmentPages] =
    useState<Record<ComparisonSegmentKey, number>>(() => ({ ...defaultSegmentPages }));
  const [segmentLoading, setSegmentLoading] =
    useState<Record<ComparisonSegmentKey, boolean>>(() => ({
      ...defaultSegmentLoading,
    }));
  const [layerVisibility, setLayerVisibility] =
    useState<Record<ComparisonSegmentKey, boolean>>({
      ...defaultLayerVisibility,
    });
  const persistPreferences = useCallback(
    (
      nextFilters: Record<ComparisonSegmentKey, TableFilters>,
      nextVisibility: Record<ComparisonSegmentKey, boolean>,
    ) => {
      if (!activeProjectId || typeof window === "undefined" || !window.localStorage) {
        return;
      }
      try {
        window.localStorage.setItem(
          preferenceKey(activeProjectId),
          JSON.stringify({ filters: nextFilters, layerVisibility: nextVisibility }),
        );
      } catch {
        // ignore storage failures
      }
    },
    [activeProjectId],
  );
  const segmentPageFor = useCallback(
    (segment: ComparisonSegmentKey) =>
      comparison ? comparison[segmentPropertyMap[segment]] : null,
    [comparison],
  );
  const rowsForSegment = useCallback(
    (segment: ComparisonSegmentKey) => segmentPageFor(segment)?.rows ?? [],
    [segmentPageFor],
  );
  const syncSegmentPages = useCallback(
    (snapshot: ComparisonSnapshot) => {
      setSegmentPages({
        overlap: snapshot.overlap.page,
        only_a: snapshot.only_a.page,
        only_b: snapshot.only_b.page,
      });
    },
    [],
  );
  const enqueueRefresh = useCallback(
    (target?: ListSlot) => {
      if (!activeProjectId) {
        setComparisonError("Create or select a comparison project first.");
        return;
      }
      const targets = target ? [target] : listSlots;
      const nextJobs: RefreshJob[] = targets.map((slot) => {
        const id =
          window.crypto?.randomUUID?.() ??
          `${slot}-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
        telemetry.track("refresh_job_enqueued", { slot });
        return {
          id,
          slot,
          status: "queued",
          processed: 0,
          total:
            slot === "A"
              ? comparison?.stats.list_a_count ?? 0
              : comparison?.stats.list_b_count ?? 0,
          resolved: 0,
          pending:
            slot === "A"
              ? comparison?.stats.pending_a ?? 0
              : comparison?.stats.pending_b ?? 0,
          message: "Queued for refresh",
        };
      });
      setRefreshQueue((prev) => [...prev, ...nextJobs]);
      setRefreshError(null);
    },
    [activeProjectId, comparison],
  );

  const toggleRefreshPause = useCallback(() => {
    setIsRefreshPaused((prev) => !prev);
  }, []);

  const cancelActiveRefresh = useCallback(() => {
    void invoke("cancel_refresh_queue").catch((error) => {
      setRefreshError(normalizeError(error));
    });
  }, []);

  const clearFinishedRefreshJobs = useCallback(() => {
    setRefreshQueue((prev) =>
      prev.filter((job) => job.status === "queued" || job.status === "running"),
    );
  }, []);
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
  const rateLimitValue =
    pendingRateLimit ??
    runtimeSettings?.places_rate_limit_qps ??
    foundationHealth?.config.places_rate_limit_qps ??
    1;
  const activeProject = useMemo(
    () => projects.find((record) => record.id === activeProjectId) ?? null,
    [projects, activeProjectId],
  );

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
        setSegmentPages({ ...defaultSegmentPages });
        return;
      }
      setComparisonError(null);
      setSegmentLoading({ ...defaultSegmentLoading });
      setIsLoadingComparison(true);
      try {
        const snapshot = await invoke<ComparisonSnapshot>("compare_lists", {
          projectId,
          page: 1,
          pageSize: DEFAULT_PAGE_SIZE,
        });
        setComparison(snapshot);
        setSelections(initialSelections());
        syncSegmentPages(snapshot);
      } catch (error) {
        setComparison(null);
        setSegmentPages({ ...defaultSegmentPages });
        setComparisonError(normalizeError(error));
      } finally {
        setIsLoadingComparison(false);
      }
    },
    [syncSegmentPages],
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
    if (activeProject) {
      setRenameProjectName(activeProject.name);
    }
  }, [activeProject]);

  const driveEnabled = foundationHealth?.config.drive_import_enabled ?? false;

  const restoreIdentity = useCallback(async () => {
    if (!driveEnabled) {
      setIdentity(null);
      return;
    }
    setIsRestoringIdentity(true);
    try {
      const profile = await invoke<GoogleIdentity>("google_current_identity");
      setIdentity(profile);
    } catch {
      setIdentity(null);
    } finally {
      setIsRestoringIdentity(false);
    }
  }, [driveEnabled]);

  useEffect(() => {
    if (!driveEnabled) {
      return;
    }
    void restoreIdentity();
  }, [driveEnabled, restoreIdentity]);

  useEffect(() => {
    if (!driveEnabled || !identity) {
      return;
    }
    const interval = window.setInterval(() => {
      void invoke<GoogleIdentity>("google_keepalive")
        .then((profile) => {
          setIdentity(profile);
        })
        .catch(async (error) => {
          const message = normalizeError(error);
          const lastFailure = await invoke<string | null>("google_refresh_status").catch(
            () => null,
          );
          setSignInError(lastFailure ?? message);
          setIdentity(null);
          telemetry.track("signin_error", { reason: lastFailure ?? message });
        });
    }, 4 * 60 * 1000);
    return () => {
      window.clearInterval(interval);
    };
  }, [driveEnabled, identity]);

  useEffect(() => {
    let mounted = true;
    const subscription = listen<ImportProgressPayload>("import://progress", (event) => {
      if (!mounted || !event.payload) {
        return;
      }
      const slot: ListSlot = event.payload.slot?.toUpperCase() === "B" ? "B" : "A";
      setImports((prev) => {
        const previous = prev[slot];
        const now = Date.now();
        const isTerminal =
          event.payload.stage === "complete" || event.payload.stage === "error";
        const attemptId = previous.attemptId ?? `${slot}-${now}`;
        const startedAt = previous.startedAt ?? now;
        const detailList =
          event.payload.stage === "error"
            ? event.payload.details?.filter((detail) => detail.trim().length > 0) ??
            (event.payload.error ? [event.payload.error] : [])
            : undefined;
        const nextHistory = isTerminal
          ? [
            {
              id: attemptId,
              fileName: event.payload.file_name ?? previous.fileName,
              finishedAt: now,
              status: event.payload.stage === "complete" ? "success" : "error",
              summary: event.payload.message,
              details: detailList,
            },
            ...previous.history,
          ].slice(0, 5)
          : previous.history;
        return {
          ...prev,
          [slot]: {
            ...previous,
            stage: event.payload.stage,
            message: event.payload.message,
            progress: event.payload.progress,
            fileName: event.payload.file_name ?? previous.fileName,
            error:
              event.payload.stage === "error"
                ? event.payload.error ?? event.payload.message
                : undefined,
            errorDetails: detailList,
            history: nextHistory,
            attemptId: isTerminal ? undefined : attemptId,
            startedAt: isTerminal ? undefined : startedAt,
            processedRows: event.payload.processed_rows ?? previous.processedRows,
            totalRows: event.payload.total_rows ?? previous.totalRows,
            rejectedRows: event.payload.rejected_rows ?? previous.rejectedRows,
            downloadedBytes: event.payload.bytes_downloaded ?? previous.downloadedBytes,
            expectedBytes: event.payload.expected_bytes ?? previous.expectedBytes,
            checksum: event.payload.checksum ?? previous.checksum,
          },
        };
      });
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
    if (!foundationHealth) {
      setRuntimeSettings(null);
      return;
    }
    setRuntimeSettings(foundationHealth.settings);
    setPendingRateLimit(foundationHealth.settings.places_rate_limit_qps);
    telemetry.setEnabled(foundationHealth.settings.telemetry_enabled);
    telemetry.setInstallSalt(foundationHealth.settings.telemetry_salt);
    telemetry.configureUpload({
      endpoint: foundationHealth.config.telemetry_endpoint ?? undefined,
      distinctId: foundationHealth.settings.telemetry_salt,
    });
    telemetry.track(
      "foundation_health_loaded",
      {
        queueDepth: foundationHealth.telemetry_queue_depth,
        recovered: foundationHealth.db_bootstrap_recovered,
        telemetryEnabled: foundationHealth.settings.telemetry_enabled,
      },
      { flush: true },
    );
  }, [foundationHealth]);

  useEffect(() => {
    if (activeProjectId) {
      void loadComparison(activeProjectId);
    } else {
      setComparison(null);
      setSegmentPages({ ...defaultSegmentPages });
      setSegmentLoading({ ...defaultSegmentLoading });
    }
  }, [activeProjectId, loadComparison]);

  useEffect(() => {
    const stored =
      activeProjectId != null ? loadPreferences(activeProjectId) : null;
    setFilters(stored?.filters ?? initialFilters());
    setLayerVisibility(stored?.layerVisibility ?? { ...defaultLayerVisibility });
    setSelections(initialSelections());
    setFocusedPlaceId(null);
    setSegmentPages({ ...defaultSegmentPages });
    setFocusPoint(null);
    setExportStatus(null);
    setSelectedFiles({ A: null, B: null });
    setSelectionErrors({ A: null, B: null });
    setImports({
      A: createImportState(),
      B: createImportState(),
    });
  }, [activeProjectId]);

  useEffect(() => {
    if (!activeProjectId) {
      setSelectedFiles({ A: null, B: null });
      return;
    }
    const project = activeProject;
    const resolveSlotSelection = (slot: ListSlot): DriveFileMetadata | null => {
      const stored =
        slot === "A"
          ? project?.list_a_drive_file ?? null
          : project?.list_b_drive_file ?? null;
      if (!stored) {
        return null;
      }
      const latest = driveFiles.find((file) => file.id === stored.id);
      return latest ?? stored;
    };
    setSelectedFiles({
      A: resolveSlotSelection("A"),
      B: resolveSlotSelection("B"),
    });
  }, [activeProject, activeProjectId, driveFiles]);

  useEffect(() => {
    setSelectionErrors({
      A: validateDriveSelection("A", selectedFiles.A),
      B: validateDriveSelection("B", selectedFiles.B),
    });
  }, [selectedFiles]);

  const handleFiltersChange = useCallback(
    (segment: ComparisonSegmentKey, nextFilters: TableFilters) => {
      setFilters((prev) => {
        const updated = {
          ...prev,
          [segment]: nextFilters,
        };
        persistPreferences(updated, layerVisibility);
        return updated;
      });
    },
    [layerVisibility, persistPreferences],
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

  const emitPlaceFocus = useCallback((source: "table" | "map", placeId: string) => {
    void telemetry
      .hashPlaceId(placeId)
      .then((placeHash) => {
        telemetry.track("place_focus", { source, placeHash });
      })
      .catch(() => {
        // intentionally ignore hashing failures to avoid blocking UI focus
      });
  }, []);

  const handleRowFocus = useCallback(
    (_segment: ComparisonSegmentKey, row: PlaceComparisonRow) => {
      setFocusedPlaceId(row.place_id);
      setFocusPoint({ lng: row.lng, lat: row.lat });
      emitPlaceFocus("table", row.place_id);
    },
    [emitPlaceFocus],
  );

  const handleMarkerFocus = useCallback(
    (placeId: string) => {
      if (!comparison) {
        return;
      }
      setFocusedPlaceId(placeId);
      const allRows = segmentKeys.flatMap((segment) => rowsForSegment(segment));
      const match = allRows.find((row) => row.place_id === placeId);
      if (match) {
        setFocusPoint({ lng: match.lng, lat: match.lat });
        emitPlaceFocus("map", placeId);
      }
    },
    [comparison, emitPlaceFocus, rowsForSegment],
  );

  const handleLayerToggle = useCallback(
    (segment: ComparisonSegmentKey) => {
      setLayerVisibility((prev) => {
        const updated = {
          ...prev,
          [segment]: !prev[segment],
        };
        persistPreferences(filters, updated);
        return updated;
      });
    },
    [filters, persistPreferences],
  );

  const handleSegmentPageChange = useCallback(
    async (segment: ComparisonSegmentKey, page: number) => {
      if (!activeProjectId) {
        setComparisonError("Create or select a comparison project first.");
        return;
      }
      setSegmentLoading((prev) => ({ ...prev, [segment]: true }));
      try {
        const pageData = await invoke<ComparisonSegmentPage>("comparison_segment_page", {
          projectId: activeProjectId,
          segment,
          page,
          pageSize: DEFAULT_PAGE_SIZE,
        });
        setComparison((prev) =>
          prev
            ? {
              ...prev,
              [segmentPropertyMap[segment]]: pageData,
            }
            : prev,
        );
        setSelections((prev) => {
          const next = { ...prev };
          const allowed = new Set(pageData.rows.map((row) => row.place_id));
          next[segment] = new Set(
            Array.from(prev[segment] ?? []).filter((id) => allowed.has(id)),
          );
          return next;
        });
        setSegmentPages((prev) => ({ ...prev, [segment]: pageData.page }));
      } catch (error) {
        setComparisonError(normalizeError(error));
      } finally {
        setSegmentLoading((prev) => ({ ...prev, [segment]: false }));
      }
    },
    [activeProjectId],
  );

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

  const handleProjectRename = useCallback(
    async (event: React.FormEvent<HTMLFormElement>) => {
      event.preventDefault();
      if (!activeProjectId) {
        setProjectError("Select a project to rename.");
        return;
      }
      if (!renameProjectName.trim()) {
        setProjectError("Enter a project name to continue.");
        return;
      }
      setIsRenamingProject(true);
      try {
        const record = await invoke<ComparisonProjectRecord>("rename_comparison_project", {
          projectId: activeProjectId,
          name: renameProjectName.trim(),
        });
        await loadProjects();
        setActiveProjectId(record.id);
        telemetry.track("project_renamed", { project_id: record.id });
        setProjectError(null);
      } catch (error) {
        setProjectError(normalizeError(error));
      } finally {
        setIsRenamingProject(false);
      }
    },
    [activeProjectId, loadProjects, renameProjectName],
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
      const baseRows = rowsForSegment(segment);
      const unique = new Set<string>();
      baseRows.forEach((row) => row.types.forEach((type) => unique.add(type)));
      acc[segment] = Array.from(unique).sort((a, b) => a.localeCompare(b));
      return acc;
    }, defaults);
  }, [comparison, rowsForSegment]);

  const totalCounts = useMemo(() => {
    if (!comparison) {
      return {
        overlap: 0,
        only_a: 0,
        only_b: 0,
      };
    }
    return {
      overlap: comparison.stats.overlap_count,
      only_a: comparison.stats.only_a_count,
      only_b: comparison.stats.only_b_count,
    };
  }, [comparison]);

  const categoryOptions = useMemo(() => {
    const defaults: Record<ComparisonSegmentKey, string[]> = {
      overlap: [],
      only_a: [],
      only_b: [],
    };
    if (!comparison) {
      return defaults;
    }
    return segmentKeys.reduce((acc, segment) => {
      const rows = rowsForSegment(segment);
      const unique = new Set<string>();
      rows.forEach((row) => unique.add(resolveCategory(row.types)));
      acc[segment] = Array.from(unique).sort((a, b) => a.localeCompare(b));
      return acc;
    }, defaults);
  }, [comparison, rowsForSegment]);

  const categoryMap = useMemo(() => {
    const map = new Map<string, string>();
    if (!comparison) {
      return map;
    }
    segmentKeys.forEach((segment) => {
      const rows = rowsForSegment(segment);
      rows.forEach((row) => {
        map.set(row.place_id, resolveCategory(row.types));
      });
    });
    return map;
  }, [comparison, rowsForSegment]);

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
      const baseRows = rowsForSegment(segment);
      const search = filters[segment].search.trim().toLowerCase();
      const typeFilter = filters[segment].type;
      const categoryFilter = filters[segment].category;
      acc[segment] = baseRows.filter((row) => {
        const matchesSearch =
          !search ||
          row.name.toLowerCase().includes(search) ||
          (row.formatted_address ?? "").toLowerCase().includes(search) ||
          row.types.some((type) => type.toLowerCase().includes(search));
        const matchesType = !typeFilter || row.types.includes(typeFilter);
        const matchesCategory =
          !categoryFilter ||
          (categoryMap.get(row.place_id) ?? resolveCategory(row.types)) ===
          categoryFilter;
        return matchesSearch && matchesType && matchesCategory;
      });
      return acc;
    }, defaults);
  }, [categoryMap, comparison, filters, rowsForSegment]);

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
      const baseRows = rowsForSegment(segment);
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
      rowsForSegment,
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

  const startLoopbackSignIn = useCallback(async () => {
    if (!driveEnabled) {
      setSignInError("Drive import is disabled in this build.");
      return;
    }
    if (isCompletingSignIn || isRequestingCode) {
      return;
    }
    setIsRequestingCode(true);
    setIsCompletingSignIn(true);
    setSignInError(null);
    try {
      const flow = await invoke<LoopbackFlowState>("google_start_loopback_flow");
      setLoopbackFlow(flow);
      await openUrl(flow.authorization_url);
      const result = await invoke<GoogleIdentity>("google_complete_loopback_sign_in", {
        timeoutSecs: 180,
      });
      setIdentity(result);
      setLoopbackFlow(null);
      telemetry.track("signin_success", { expiresAt: result.expires_at });
    } catch (error) {
      const message = normalizeError(error);
      setSignInError(message);
      setLoopbackFlow(null);
      setIdentity(null);
      telemetry.track("signin_error", { reason: message });
    } finally {
      setIsRequestingCode(false);
      setIsCompletingSignIn(false);
    }
  }, [driveEnabled, isCompletingSignIn, isRequestingCode]);

  const handleSignOut = useCallback(async () => {
    setSignInError(null);
    setLoopbackFlow(null);
    try {
      await invoke("google_sign_out");
    } catch (error) {
      setSignInError(normalizeError(error));
    } finally {
      setIdentity(null);
      setDriveFiles([]);
      setIsRequestingCode(false);
      setIsCompletingSignIn(false);
      setPickerError(null);
      setFileQuery("");
      setFileTypeFilter("all");
      setSelectionErrors({ A: null, B: null });
      setSelectedFiles({ A: null, B: null });
      setImports({
        A: createImportState(),
        B: createImportState(),
      });
    }
  }, []);

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
      const message = normalizeError(error);
      setPickerError(message);
      if (message.toLowerCase().includes("auth") || message.toLowerCase().includes("sign")) {
        setSignInError(message);
        setIdentity(null);
      }
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

  const filteredDriveFiles = useMemo(() => {
    const search = fileQuery.trim().toLowerCase();
    return driveFiles.filter((file) => {
      const matchesQuery =
        !search ||
        file.name.toLowerCase().includes(search) ||
        (file.modified_time ?? "").toLowerCase().includes(search);
      const kind = driveFileKind(file);
      const matchesType = fileTypeFilter === "all" || kind === fileTypeFilter;
      return matchesQuery && matchesType;
    });
  }, [driveFiles, fileQuery, fileTypeFilter]);

  const driveOptionsForSlot = useCallback(
    (slot: ListSlot) => {
      const current = selectedFiles[slot];
      if (!current) {
        return filteredDriveFiles;
      }
      const alreadyIncluded = filteredDriveFiles.some((file) => file.id === current.id);
      return alreadyIncluded ? filteredDriveFiles : [current, ...filteredDriveFiles];
    },
    [filteredDriveFiles, selectedFiles],
  );

  useEffect(() => {
    let mounted = true;
    const subscription = listen<RefreshProgressPayload>(
      "refresh://progress",
      (event) => {
        if (!mounted || !event.payload) {
          return;
        }
        const jobId = event.payload.request_id ?? "";
        if (!jobId) {
          return;
        }
        const stage = event.payload.stage;
        setRefreshQueue((prev) =>
          prev.map((job) => {
            if (job.id !== jobId) {
              return job;
            }
            const nextStatus: RefreshJobStatus =
              stage === "complete"
                ? "complete"
                : stage === "cancelled"
                  ? "cancelled"
                  : stage === "error"
                    ? "error"
                    : job.status;
            const finished =
              stage === "complete" || stage === "cancelled" || stage === "error"
                ? Date.now()
                : job.finishedAt;
            return {
              ...job,
              status: nextStatus,
              processed: event.payload.processed ?? job.processed,
              total: event.payload.total_rows ?? job.total,
              resolved: event.payload.resolved ?? job.resolved,
              pending: event.payload.pending ?? job.pending,
              message: event.payload.message ?? job.message,
              finishedAt: finished,
            };
          }),
        );
        if (
          ["complete", "cancelled", "error"].includes(stage) &&
          activeProjectId
        ) {
          telemetry.track("refresh_job_completed", {
            slot: event.payload.slot,
            status: stage,
            resolved: event.payload.resolved,
            pending: event.payload.pending,
          });
          void loadComparison(activeProjectId);
        }
      },
    );
    return () => {
      mounted = false;
      void subscription.then((unlisten) => unlisten());
    };
  }, [activeProjectId, loadComparison]);

  useEffect(() => {
    if (isRefreshPaused || refreshRunnerRef.current || !activeProjectId) {
      return;
    }
    const nextJob = refreshQueue.find((job) => job.status === "queued");
    if (!nextJob) {
      return;
    }
    refreshRunnerRef.current = nextJob.id;
    setRefreshQueue((prev) =>
      prev.map((job) =>
        job.id === nextJob.id
          ? {
            ...job,
            status: "running",
            startedAt: Date.now(),
            message: "Preparing refresh…",
          }
          : job,
      ),
    );
    setRefreshError(null);
    void (async () => {
      try {
        const stats = await invoke<NormalizationStats[]>("refresh_place_details", {
          projectId: activeProjectId,
          slot: nextJob.slot,
          requestId: nextJob.id,
        });
        const summary = stats.find((entry) => entry.slot === nextJob.slot);
        if (summary) {
          telemetry.track("places_refresh_triggered", {
            slot: nextJob.slot,
            refreshed: summary.resolved,
            pending: summary.unresolved,
          });
        }
        setRefreshQueue((prev) =>
          prev.map((job) => {
            if (job.id !== nextJob.id || job.status !== "running") {
              return job;
            }
            return {
              ...job,
              status: "complete",
              finishedAt: Date.now(),
              resolved: summary?.resolved ?? job.resolved,
              pending: summary?.unresolved ?? job.pending,
              processed:
                summary?.total_rows != null
                  ? Math.max(summary.total_rows - summary.unresolved, 0)
                  : job.processed,
              total: summary?.total_rows ?? job.total,
              message: summary
                ? `Refreshed ${summary.resolved} places`
                : "Refresh completed",
            };
          }),
        );
        if (activeProjectId) {
          await loadComparison(activeProjectId);
        }
      } catch (error) {
        const message = normalizeError(error);
        setRefreshError(message);
        setRefreshQueue((prev) =>
          prev.map((job) =>
            job.id === nextJob.id
              ? {
                ...job,
                status: "error",
                finishedAt: Date.now(),
                message,
              }
              : job,
          ),
        );
      } finally {
        refreshRunnerRef.current = null;
      }
    })();
  }, [refreshQueue, isRefreshPaused, activeProjectId, loadComparison]);

  const handleFileSelection = useCallback(
    (slot: ListSlot, fileId: string) => {
      const fallback = selectedFiles[slot];
      const file =
        fileId === ""
          ? null
          : driveFiles.find((entry) => entry.id === fileId) ?? fallback ?? null;
      setSelectedFiles((prev) => ({
        ...prev,
        [slot]: file,
      }));
      if (activeProjectId) {
        void invoke("drive_save_selection", {
          projectId: activeProjectId,
          slot,
          file,
        }).catch((error) => setPickerError(normalizeError(error)));
        setProjects((prev) =>
          prev.map((project) =>
            project.id === activeProjectId
              ? {
                ...project,
                [slot === "A" ? "list_a_drive_file" : "list_b_drive_file"]: file,
              }
              : project,
          ),
        );
      }
      if (file) {
        void hashIdentifier(file.id).then((hash) => {
          telemetry.track("drive_file_selected", { slot, fileHash: hash });
        });
      }
    },
    [activeProjectId, driveFiles, selectedFiles],
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
            ...prev[slot],
            stage: "idle",
            message: "Select a Drive KML before importing",
            progress: 0,
            error: undefined,
            errorDetails: undefined,
            attemptId: undefined,
            startedAt: undefined,
            processedRows: undefined,
            totalRows: undefined,
            rejectedRows: undefined,
            downloadedBytes: undefined,
            expectedBytes: undefined,
            checksum: undefined,
          },
        }));
        return;
      }
      if (selectionErrors[slot]) {
        const validationMessage = selectionErrors[slot];
        setImports((prev) => ({
          ...prev,
          [slot]: {
            ...prev[slot],
            stage: "idle",
            message: validationMessage ?? "Select a Drive KML before importing",
            progress: 0,
            error: validationMessage ?? undefined,
            errorDetails: validationMessage ? [validationMessage] : undefined,
            attemptId: undefined,
            startedAt: undefined,
            processedRows: undefined,
            totalRows: undefined,
            rejectedRows: undefined,
            downloadedBytes: undefined,
            expectedBytes: undefined,
            checksum: undefined,
          },
        }));
        return;
      }

      const attemptId = `${slot}-${Date.now()}`;
      setImports((prev) => ({
        ...prev,
        [slot]: {
          ...prev[slot],
          stage: "starting",
          message: "Preparing import…",
          progress: 0,
          fileName: file.name,
          error: undefined,
          errorDetails: undefined,
          attemptId,
          startedAt: Date.now(),
          processedRows: undefined,
          totalRows: undefined,
          rejectedRows: undefined,
          downloadedBytes: undefined,
          expectedBytes: undefined,
          checksum: undefined,
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
          mimeType: file.mime_type,
          modifiedTime: file.modified_time,
          size: file.size,
          md5Checksum: file.md5_checksum,
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
        if (message.toLowerCase().includes("auth") || message.toLowerCase().includes("sign")) {
          setSignInError(message);
          setIdentity(null);
        }
        setImports((prev) => {
          const previous = prev[slot];
          if (previous.stage === "error") {
            return prev;
          }
          const now = Date.now();
          return {
            ...prev,
            [slot]: {
              ...previous,
              stage: "error",
              message: message || "Import failed",
              error: message,
              errorDetails: message ? [message] : undefined,
              history: [
                {
                  id: previous.attemptId ?? `${slot}-${now}`,
                  fileName: previous.fileName,
                  finishedAt: now,
                  status: "error" as const,
                  summary: message || "Import failed",
                  details: message ? [message] : undefined,
                },
                ...previous.history,
              ].slice(0, 5),
              attemptId: undefined,
              startedAt: undefined,
            },
          };
        });
      }
    },
    [activeProjectId, selectedFiles, selectionErrors],
  );

  const slotBusy = (slot: ListSlot) => {
    const stage = imports[slot].stage;
    return !["idle", "complete", "error"].includes(stage);
  };

  const failedSlots = useMemo(
    () => listSlots.filter((slot) => imports[slot].stage === "error"),
    [imports],
  );

  const activeRefreshJob = useMemo(
    () => refreshQueue.find((job) => job.status === "running") ?? null,
    [refreshQueue],
  );
  const hasFinishedRefreshJobs = useMemo(
    () =>
      refreshQueue.some((job) =>
        ["complete", "cancelled", "error"].includes(job.status),
      ),
    [refreshQueue],
  );

  const applySettingsPatch = useCallback(
    async (patch: { telemetryEnabled?: boolean; placesRateLimitQps?: number }) => {
      if (!runtimeSettings) {
        return;
      }
      const payload: Record<string, unknown> = {};
      if (typeof patch.telemetryEnabled === "boolean") {
        payload.telemetryEnabled = patch.telemetryEnabled;
      }
      if (typeof patch.placesRateLimitQps === "number") {
        payload.placesRateLimitQps = patch.placesRateLimitQps;
      }
      if (Object.keys(payload).length === 0) {
        return;
      }
      setIsUpdatingSettings(true);
      setSettingsError(null);
      try {
        const updated = await invoke<RuntimeSettings>("update_runtime_settings", {
          payload,
        });
        setRuntimeSettings(updated);
        setPendingRateLimit(updated.places_rate_limit_qps);
        telemetry.setEnabled(updated.telemetry_enabled);
      } catch (error) {
        setSettingsError(normalizeError(error));
      } finally {
        setIsUpdatingSettings(false);
      }
    },
    [runtimeSettings],
  );

  const handleTelemetryToggle = useCallback(() => {
    if (!runtimeSettings) {
      return;
    }
    void applySettingsPatch({ telemetryEnabled: !runtimeSettings.telemetry_enabled });
  }, [applySettingsPatch, runtimeSettings]);

  const handleRateLimitChange = (event: React.ChangeEvent<HTMLInputElement>) => {
    setPendingRateLimit(Number(event.target.value));
  };

  const handleRateLimitApply = useCallback(() => {
    if (
      !runtimeSettings ||
      pendingRateLimit == null ||
      pendingRateLimit === runtimeSettings.places_rate_limit_qps
    ) {
      return;
    }
    void applySettingsPatch({ placesRateLimitQps: pendingRateLimit });
  }, [applySettingsPatch, pendingRateLimit, runtimeSettings]);

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

      <section className="settings-panel">
        <div className="panel-header">
          <h2>Settings</h2>
          <p>Control telemetry and Places resolver budgets.</p>
        </div>
        {!foundationHealth && !bootstrapError && <p>Loading settings&hellip;</p>}
        {foundationHealth && runtimeSettings && (
          <div className="settings-grid">
            <article className="settings-card">
              <div className="settings-card__header">
                <div>
                  <h3>Telemetry</h3>
                  <p className="muted">
                    {foundationHealth.config.telemetry_endpoint
                      ? "Events stream to the configured endpoint with offline buffering."
                      : "Events stay on disk until you opt-in to an endpoint."}
                  </p>
                </div>
                <button
                  type="button"
                  className={`toggle ${runtimeSettings.telemetry_enabled ? "on" : "off"}`}
                  onClick={handleTelemetryToggle}
                  disabled={isUpdatingSettings}
                >
                  {runtimeSettings.telemetry_enabled ? "Enabled" : "Disabled"}
                </button>
              </div>
              <dl className="settings-list">
                <div>
                  <dt>Endpoint</dt>
                  <dd>
                    {foundationHealth.config.telemetry_endpoint ?? "Offline buffer only"}
                  </dd>
                </div>
                <div>
                  <dt>Queue depth</dt>
                  <dd>{foundationHealth.telemetry_queue_depth}</dd>
                </div>
                <div>
                  <dt>Buffer path</dt>
                  <dd>{foundationHealth.telemetry_buffer_path}</dd>
                </div>
              </dl>
            </article>
            <article className="settings-card">
              <h3>Places rate limit</h3>
              <p className="muted">
                Protects the Google Places Search API quota shared across slots.
              </p>
              <input
                type="range"
                min={1}
                max={10}
                step={1}
                className="rate-slider"
                value={rateLimitValue}
                onChange={handleRateLimitChange}
                disabled={isUpdatingSettings}
              />
              <div className="rate-limit-meta">
                <span>{rateLimitValue} req/sec</span>
                <button
                  type="button"
                  className="secondary-button"
                  onClick={handleRateLimitApply}
                  disabled={
                    isUpdatingSettings ||
                    rateLimitValue === runtimeSettings.places_rate_limit_qps
                  }
                >
                  Update budget
                </button>
              </div>
              <p className="muted">
                Active normalizations honor this budget immediately; queued work is throttled.
              </p>
            </article>
            <article className="settings-card">
              <h3>SQLCipher passphrase</h3>
              <p className="muted">Signals whether the OS keychain secret is intact.</p>
              <dl className="settings-list">
                <div>
                  <dt>Lifecycle</dt>
                  <dd>{foundationHealth.db_key_lifecycle}</dd>
                </div>
                <div>
                  <dt>Recovered on last boot</dt>
                  <dd>{foundationHealth.db_bootstrap_recovered ? "Yes" : "No"}</dd>
                </div>
                <div>
                  <dt>Key in vault</dt>
                  <dd>{foundationHealth.has_encryption_key ? "Present" : "Missing"}</dd>
                </div>
                <div>
                  <dt>Database path</dt>
                  <dd>{foundationHealth.db_path}</dd>
                </div>
              </dl>
            </article>
          </div>
        )}
        {settingsError && <p className="error-text">{settingsError}</p>}
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
          <form className="project-create" onSubmit={handleProjectRename}>
            <label htmlFor="rename-project">Rename active</label>
            <div className="project-create__controls">
              <input
                id="rename-project"
                type="text"
                value={renameProjectName}
                placeholder="Rename active project"
                onChange={(event) => setRenameProjectName(event.target.value)}
                disabled={!activeProjectId}
              />
              <button
                type="submit"
                className="secondary-button"
                disabled={
                  !activeProjectId ||
                  renameProjectName.trim().length === 0 ||
                  isRenamingProject
                }
              >
                Save
              </button>
            </div>
            <p className="muted">
              Last compared: {formatTimestamp(activeProject?.last_compared_at)}
            </p>
          </form>
        </div>
        {projectError && <p className="error-text">{projectError}</p>}
        <div className="comparison-actions">
          <div>
            <p className="muted">
              Pending lookups · List A: {comparison?.stats.pending_a ?? 0} · List B:{" "}
              {comparison?.stats.pending_b ?? 0}
            </p>
            <p className="muted">Rate limit: {rateLimitValue} QPS</p>
          </div>
          <div className="refresh-controls">
            <button
              type="button"
              className="secondary-button"
              onClick={() => enqueueRefresh()}
              disabled={!activeProjectId}
            >
              Queue both lists
            </button>
            <button
              type="button"
              className="secondary-button"
              onClick={() => enqueueRefresh("A")}
              disabled={!activeProjectId}
            >
              Queue List A
            </button>
            <button
              type="button"
              className="secondary-button"
              onClick={() => enqueueRefresh("B")}
              disabled={!activeProjectId}
            >
              Queue List B
            </button>
            <button
              type="button"
              className="secondary-button"
              onClick={toggleRefreshPause}
              disabled={!refreshQueue.length}
            >
              {isRefreshPaused ? "Resume queue" : "Pause queue"}
            </button>
            <button
              type="button"
              className="secondary-button"
              onClick={cancelActiveRefresh}
              disabled={!activeRefreshJob}
            >
              Cancel active
            </button>
          </div>
        </div>
        {refreshError && <p className="error-text">{refreshError}</p>}
        <div className="refresh-queue">
          {refreshQueue.length === 0 ? (
            <p className="muted">No refresh jobs queued.</p>
          ) : (
            <ul>
              {refreshQueue.map((job) => {
                const total = job.total || 0;
                const progressPct =
                  total > 0 ? Math.min(100, Math.round((job.processed / total) * 100)) : 0;
                return (
                  <li key={job.id} className="refresh-job">
                    <div className="refresh-job__header">
                      <strong>{job.slot === "A" ? "List A" : "List B"}</strong>
                      <span className={`refresh-status status-${job.status}`}>
                        {job.status}
                      </span>
                    </div>
                    <p className="refresh-job__message">{job.message}</p>
                    <div className="progress-track">
                      <div
                        className="progress-bar"
                        style={{ width: `${progressPct}%` }}
                      />
                    </div>
                    <p className="refresh-job__metrics">
                      {job.processed}/{total || "?"} rows processed · Resolved {job.resolved} ·
                      Pending {job.pending}
                    </p>
                  </li>
                );
              })}
            </ul>
          )}
          {hasFinishedRefreshJobs && (
            <button
              type="button"
              className="link-button"
              onClick={clearFinishedRefreshJobs}
            >
              Clear finished jobs
            </button>
          )}
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
                    availableCategories={categoryOptions[segment]}
                    selectedIds={selections[segment]}
                    focusedPlaceId={focusedPlaceId}
                    page={segmentPages[segment]}
                    pageSize={segmentPageFor(segment)?.page_size ?? DEFAULT_PAGE_SIZE}
                    isLoading={segmentLoading[segment] || isLoadingComparison}
                    onFiltersChange={handleFiltersChange}
                    onSelectionChange={handleSelectionChange}
                    onRowFocus={handleRowFocus}
                    onPageChange={handleSegmentPageChange}
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
            {driveEnabled && signInError && <p className="error-text">{signInError}</p>}
            {driveEnabled && !identity && !isRestoringIdentity && (
              <>
                <p>
                  Approve Drive access in your default browser; the desktop shell listens on a local
                  loopback redirect to finish sign-in automatically.
                </p>
                <button
                  type="button"
                  className="primary-button"
                  onClick={startLoopbackSignIn}
                  disabled={isRequestingCode || isCompletingSignIn}
                >
                  {isCompletingSignIn
                    ? "Waiting for approval…"
                    : isRequestingCode
                      ? "Opening browser…"
                      : "Sign in with Google"}
                </button>
                {loopbackFlow && (
                  <p className="muted">
                    Listening for redirect via <code>{loopbackFlow.redirect_url}</code>
                  </p>
                )}
              </>
            )}
            {isRestoringIdentity && !identity && (
              <p className="muted">Restoring Google identity from the secure vault…</p>
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
                <div className="identity-actions">
                  <button
                    type="button"
                    className="secondary-button"
                    onClick={startLoopbackSignIn}
                    disabled={isRequestingCode || isCompletingSignIn}
                  >
                    {isCompletingSignIn ? "Refreshing session…" : "Re-authenticate"}
                  </button>
                  <button type="button" className="secondary-button" onClick={handleSignOut}>
                    Sign out
                  </button>
                  <button
                    type="button"
                    className="secondary-button"
                    onClick={loadDriveFiles}
                    disabled={isLoadingFiles}
                  >
                    {isLoadingFiles ? "Refreshing files…" : "Reload Drive files"}
                  </button>
                </div>
              </div>
            )}
          </article>

          <article className="file-card">
            {!identity && <p>Sign in to browse Google Drive KML exports.</p>}
            {pickerError && <p className="error-text">{pickerError}</p>}
            {identity && (
              <>
                <div className="drive-filters">
                  <div className="drive-filters__inputs">
                    <input
                      type="search"
                      placeholder="Search Drive files"
                      value={fileQuery}
                      onChange={(event) => setFileQuery(event.target.value)}
                    />
                    <select
                      value={fileTypeFilter}
                      onChange={(event) =>
                        setFileTypeFilter(
                          event.target.value === "map"
                            ? "map"
                            : event.target.value === "kml"
                              ? "kml"
                              : "all",
                        )
                      }
                    >
                      <option value="all">All types</option>
                      <option value="kml">KML</option>
                      <option value="map">My Maps</option>
                    </select>
                  </div>
                  <p className="muted">
                    Showing {filteredDriveFiles.length} of {driveFiles.length} Drive files
                  </p>
                </div>
                <div className="list-toolbar">
                  <button
                    type="button"
                    className="secondary-button"
                    onClick={() =>
                      failedSlots.forEach((slot) => {
                        void handleImport(slot);
                      })
                    }
                    disabled={
                      failedSlots.length === 0 || failedSlots.some((slot) => slotBusy(slot))
                    }
                  >
                    Retry failed imports
                  </button>
                  <p className="muted">
                    {failedSlots.length === 0
                      ? "No failed imports at the moment."
                      : `${failedSlots.length} list${failedSlots.length === 1 ? "" : "s"
                      } ready for retry.`}
                  </p>
                </div>
                <div className="list-grid">
                  {listSlots.map((slot) => (
                    <div key={slot} className="list-card">
                      <div className="list-card__header">
                        <h3>List {slot}</h3>
                        <span className="list-card__count">
                          {filteredDriveFiles.length}/{driveFiles.length} file
                          {driveFiles.length === 1 ? "" : "s"}
                        </span>
                      </div>
                      <label className="field-label" htmlFor={`slot-${slot}`}>
                        Drive KML / My Maps
                      </label>
                      <select
                        id={`slot-${slot}`}
                        value={selectedFiles[slot]?.id ?? ""}
                        onChange={(event) => handleFileSelection(slot, event.target.value)}
                        disabled={driveOptionsForSlot(slot).length === 0 || isLoadingFiles}
                      >
                        <option value="">Select a file</option>
                        {driveOptionsForSlot(slot).map((file) => (
                          <option key={`${slot}-${file.id}`} value={file.id}>
                            {formatDriveFileLabel(file)}
                          </option>
                        ))}
                      </select>
                      {selectedFiles[slot] && (
                        <div className="file-meta">
                          <p className="file-meta__name">{selectedFiles[slot]?.name}</p>
                          <p className="muted">{formatDriveFileMeta(selectedFiles[slot]!)}</p>
                        </div>
                      )}
                      {selectionErrors[slot] && (
                        <p className="error-text">{selectionErrors[slot]}</p>
                      )}
                      <button
                        type="button"
                        className="primary-button"
                        onClick={() => void handleImport(slot)}
                        disabled={
                          !selectedFiles[slot] ||
                          slotBusy(slot) ||
                          !activeProjectId ||
                          !!selectionErrors[slot]
                        }
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
                      {imports[slot].totalRows != null && (
                        <p className="muted">
                          Rows {imports[slot].processedRows ?? 0}/{imports[slot].totalRows}
                          {imports[slot].rejectedRows
                            ? ` (${imports[slot].rejectedRows} rejected)`
                            : ""}
                        </p>
                      )}
                      {imports[slot].downloadedBytes != null && (
                        <p className="muted">
                          Downloaded {formatBytes(imports[slot].downloadedBytes) ?? `${imports[slot].downloadedBytes} B`}
                          {imports[slot].expectedBytes
                            ? ` of ${formatBytes(imports[slot].expectedBytes) ?? `${imports[slot].expectedBytes} B`}`
                            : ""}
                        </p>
                      )}
                      {imports[slot].error && (
                        <div className="import-error">
                          <p className="error-text">{imports[slot].error}</p>
                          {imports[slot].errorDetails &&
                            imports[slot].errorDetails.length > 0 && (
                              <details className="import-details">
                                <summary>Details</summary>
                                <ul>
                                  {imports[slot].errorDetails.map((detail, index) => (
                                    <li key={`${slot}-detail-${index}`}>{detail}</li>
                                  ))}
                                </ul>
                              </details>
                            )}
                          <div className="import-error__actions">
                            <button
                              type="button"
                              className="secondary-button"
                              onClick={() => void handleImport(slot)}
                              disabled={slotBusy(slot)}
                            >
                              Retry this file
                            </button>
                          </div>
                        </div>
                      )}
                      {imports[slot].history.length > 0 && (
                        <details className="import-history">
                          <summary>Recent attempts</summary>
                          <ul>
                            {imports[slot].history.map((attempt) => (
                              <li key={attempt.id}>
                                <div className="import-history__meta">
                                  <strong>{attempt.fileName ?? "Unnamed file"}</strong>
                                  <span>
                                    {attempt.status === "success" ? "Succeeded" : "Failed"} ·{" "}
                                    {new Date(attempt.finishedAt).toLocaleString(undefined, {
                                      hour: "2-digit",
                                      minute: "2-digit",
                                      month: "short",
                                      day: "numeric",
                                    })}
                                  </span>
                                </div>
                                {attempt.details && attempt.details.length > 0 && (
                                  <ul className="import-history__details">
                                    {attempt.details.map((detail, index) => (
                                      <li key={`${attempt.id}-detail-${index}`}>{detail}</li>
                                    ))}
                                  </ul>
                                )}
                              </li>
                            ))}
                          </ul>
                        </details>
                      )}
                    </div>
                  ))}
                </div>
              </>
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

function driveFileKind(file: DriveFileMetadata): "kml" | "map" {
  return file.mime_type.includes("vnd.google-apps.map") ? "map" : "kml";
}

function formatBytes(size?: number | null): string | null {
  if (!size || size <= 0) {
    return null;
  }
  const units = ["B", "KB", "MB", "GB"];
  const exponent = Math.min(Math.floor(Math.log(size) / Math.log(1024)), units.length - 1);
  const value = size / 1024 ** exponent;
  return `${value.toFixed(value >= 10 ? 0 : 1)} ${units[exponent]}`;
}

function formatDriveDate(value?: string | null): string | null {
  if (!value) {
    return null;
  }
  const parsed = new Date(value);
  if (Number.isNaN(parsed.getTime())) {
    return null;
  }
  return parsed.toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

function formatDriveFileLabel(file: DriveFileMetadata): string {
  const parts = [file.name];
  parts.push(driveFileKind(file) === "map" ? "My Maps" : "KML");
  const size = formatBytes(file.size ?? null);
  const date = formatDriveDate(file.modified_time ?? null);
  if (size) {
    parts.push(size);
  }
  if (date) {
    parts.push(date);
  }
  return parts.join(" · ");
}

function formatDriveFileMeta(file: DriveFileMetadata): string {
  const parts = [driveFileKind(file) === "map" ? "My Maps layer" : "KML file"];
  const size = formatBytes(file.size ?? null);
  const date = formatDriveDate(file.modified_time ?? null);
  if (size) {
    parts.push(size);
  }
  if (date) {
    parts.push(`updated ${date}`);
  }
  return parts.join(" · ");
}

function validateDriveSelection(
  slot: ListSlot,
  file: DriveFileMetadata | null,
): string | null {
  if (!file) {
    return null;
  }
  const normalized = file.name.trim().toLowerCase();
  const expected = `list ${slot.toLowerCase()}`;
  const nameMatches = normalized.includes(expected);
  const hasValidExtension =
    normalized.endsWith(".kml") || normalized.endsWith(".kmz") || driveFileKind(file) === "map";
  if (!nameMatches || !hasValidExtension) {
    return `Name must look like "List ${slot}.kml"`;
  }
  return null;
}

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
