import { invoke } from "@tauri-apps/api/core";

type TelemetryPayload = Record<string, unknown>;

interface TelemetryEvent {
  name: string;
  payload: TelemetryPayload;
  flush: boolean;
}

export interface TelemetryAdapterOptions {
  flushSize: number;
  flushIntervalMs: number;
  enabled: boolean;
}

export interface TrackOptions {
  flush?: boolean;
}

export interface TelemetryUploadConfig {
  endpoint?: string;
  apiKey?: string;
  headers?: Record<string, string>;
  distinctId?: string;
  source?: string;
}

const FALLBACK_SALT_KEY = "gmlc-telemetry-salt";
const safeNow = () => new Date().toISOString();

export class TelemetryAdapter {
  private queue: TelemetryEvent[] = [];
  private enabled: boolean;
  private pendingFlush = false;
  private timer: number | undefined;
  private hasWarned = false;
  private uploadConfig: TelemetryUploadConfig | null = null;
  private installSalt: string | null = null;
  private fallbackSalt: string | null = null;

  constructor(private readonly options: TelemetryAdapterOptions) {
    this.enabled = options.enabled;
    if (this.enabled && options.flushIntervalMs > 0 && typeof window !== "undefined") {
      this.timer = window.setInterval(() => {
        void this.flush();
      }, options.flushIntervalMs);
    }
  }

  setEnabled(next: boolean) {
    this.enabled = next;
    if (!next) {
      this.queue = [];
    }
  }

  configureUpload(config?: TelemetryUploadConfig | null) {
    if (config?.endpoint) {
      this.uploadConfig = { ...config };
    } else {
      this.uploadConfig = null;
    }
  }

  setInstallSalt(salt: string | null) {
    this.installSalt = salt ?? null;
  }

  async hashPlaceId(placeId: string): Promise<string> {
    const activeSalt = await this.resolveSalt();
    return this.hashValue(`${activeSalt}:${placeId}`);
  }

  track(name: string, payload: TelemetryPayload = {}, options?: TrackOptions) {
    if (!this.enabled) {
      return;
    }
    const event: TelemetryEvent = {
      name,
      flush: Boolean(options?.flush),
      payload: {
        ...payload,
        clientTimestamp: safeNow(),
      },
    };
    this.queue.push(event);
    if (event.flush || this.queue.length >= this.options.flushSize) {
      void this.flush();
    }
  }

  async flush() {
    if (!this.enabled || this.pendingFlush || this.queue.length === 0) {
      return;
    }
    this.pendingFlush = true;
    const batch = [...this.queue];
    this.queue = [];

    try {
      await this.dispatchBatch(batch);
    } catch (error) {
      if (!this.hasWarned) {
        console.warn("[telemetry] failed to queue event(s); buffering for retry", error);
        this.hasWarned = true;
      }
      this.queue.unshift(...batch);
    } finally {
      this.pendingFlush = false;
    }
  }

  dispose() {
    if (this.timer) {
      clearInterval(this.timer);
      this.timer = undefined;
    }
    this.queue = [];
  }

  private async dispatchBatch(batch: TelemetryEvent[]) {
    if (this.uploadConfig?.endpoint) {
      await this.postToEndpoint(batch);
      return;
    }
    await this.enqueueWithBackend(batch);
  }

  private async enqueueWithBackend(batch: TelemetryEvent[]) {
    for (const event of batch) {
      await invoke("record_telemetry_event", {
        name: event.name,
        payload: event.payload,
        flush: event.flush,
      });
    }
  }

  private async postToEndpoint(batch: TelemetryEvent[]) {
    if (!this.uploadConfig?.endpoint) {
      return;
    }
    if (typeof fetch !== "function") {
      throw new Error("fetch is not available for telemetry uploads");
    }
    const headers: Record<string, string> = {
      "Content-Type": "application/json",
      ...(this.uploadConfig.headers ?? {}),
    };
    const envelope: Record<string, unknown> = {
      batch: batch.map((event) => ({
        event: event.name,
        properties: {
          ...event.payload,
          distinct_id: this.uploadConfig?.distinctId ?? "desktop-shell",
          source: this.uploadConfig?.source ?? "desktop-shell",
        },
        timestamp: event.payload.clientTimestamp ?? safeNow(),
      })),
      sent_at: safeNow(),
    };
    if (this.uploadConfig.apiKey) {
      envelope.api_key = this.uploadConfig.apiKey;
    }
    const response = await fetch(this.uploadConfig.endpoint, {
      method: "POST",
      headers,
      body: JSON.stringify(envelope),
    });
    if (!response.ok) {
      throw new Error(`telemetry upload failed with status ${response.status}`);
    }
  }

  private async resolveSalt(): Promise<string> {
    if (this.installSalt) {
      return this.installSalt;
    }
    if (this.fallbackSalt) {
      return this.fallbackSalt;
    }
    const stored = this.readStoredSalt();
    if (stored) {
      this.fallbackSalt = stored;
      return stored;
    }
    const generated = await this.hashValue(`${safeNow()}:${Math.random()}`);
    this.fallbackSalt = generated;
    this.persistSalt(generated);
    return generated;
  }

  private readStoredSalt(): string | null {
    if (typeof window === "undefined" || !window.localStorage) {
      return null;
    }
    try {
      return window.localStorage.getItem(FALLBACK_SALT_KEY);
    } catch {
      return null;
    }
  }

  private persistSalt(value: string) {
    if (typeof window === "undefined" || !window.localStorage) {
      return;
    }
    try {
      window.localStorage.setItem(FALLBACK_SALT_KEY, value);
    } catch {
      // ignore write failures; the salt will be regenerated later
    }
  }

  private async hashValue(input: string): Promise<string> {
    try {
      if (typeof window !== "undefined" && window.crypto?.subtle) {
        const bytes = new TextEncoder().encode(input);
        const digest = await window.crypto.subtle.digest("SHA-256", bytes);
        return bytesToBase64(new Uint8Array(digest));
      }
    } catch {
      // fall through to deterministic encoding
    }
    return bytesToBase64(new TextEncoder().encode(input));
  }
}

const bytesToBase64 = (bytes: Uint8Array): string => {
  let binary = "";
  bytes.forEach((byte) => {
    binary += String.fromCharCode(byte);
  });
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
};

export const telemetry = new TelemetryAdapter({
  flushSize: 3,
  flushIntervalMs: 5_000,
  enabled: true,
});
