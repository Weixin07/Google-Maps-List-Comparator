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

const safeNow = () => new Date().toISOString();

export class TelemetryAdapter {
  private queue: TelemetryEvent[] = [];
  private enabled: boolean;
  private pendingFlush = false;
  private timer: number | undefined;
  private hasWarned = false;

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
      for (const event of batch) {
        await invoke("record_telemetry_event", {
          name: event.name,
          payload: event.payload,
          flush: event.flush,
        });
      }
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
}

export const telemetry = new TelemetryAdapter({
  flushSize: 3,
  flushIntervalMs: 5_000,
  enabled: true,
});
