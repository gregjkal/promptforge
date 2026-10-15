// Desktop update state and I/O. The service is DOM-free: views subscribe to
// snapshots and the composition root owns its lifetime.

import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type DownloadEvent, type Update } from "@tauri-apps/plugin-updater";

import { Emitter, type Event } from "@workshop/platform/event";
import { Disposable, toDisposable } from "@workshop/platform/lifecycle";
import { errorText } from "./error-catalog";

export type UpdatePhase =
  | "idle"
  | "checking"
  | "current"
  | "available"
  | "dismissed"
  | "downloading"
  | "installing"
  | "restarting"
  | "error"
  | "unsupported"
  | "browser";

/**
 * What PromptForge Gateway did when Workshop asked it to update itself after
 * Workshop's own check: the desktop app's `update_gateway` command answer.
 */
export type GatewayUpdate =
  | { readonly outcome: "updating"; readonly version: string }
  | { readonly outcome: "failed"; readonly message: string }
  | {
      readonly outcome:
        | "current"
        | "workshop_owned"
        | "busy"
        | "predates"
        | "no_local_gateway"
        | "development";
    };

export interface UpdateSnapshot {
  readonly phase: UpdatePhase;
  readonly currentVersion: string;
  readonly version: string;
  readonly notes: string;
  readonly downloaded: number;
  readonly total: number | null;
  readonly error: string;
  readonly log: readonly string[];
  /** The startup check produced this state, not a check or install the user asked for. */
  readonly background: boolean;
  /** The gateway's answer after the last check; `null` until one arrives. */
  readonly gateway: GatewayUpdate | null;
}

interface DesktopUpdate {
  readonly currentVersion: string;
  readonly version: string;
  readonly body?: string;
  downloadAndInstall(onEvent?: (event: DownloadEvent) => void): Promise<void>;
  close(): Promise<void>;
}

export interface UpdateBackend {
  readonly desktop: boolean;
  supported(): Promise<boolean>;
  currentVersion(): Promise<string>;
  check(): Promise<DesktopUpdate | null>;
  relaunch(): Promise<void>;
  /** Asks the local gateway to update itself; desktop only. */
  updateGateway(): Promise<GatewayUpdate>;
}

function defaultBackend(): UpdateBackend {
  return {
    desktop: typeof window !== "undefined" && window.__TAURI_INTERNALS__ !== undefined,
    supported: async (): Promise<boolean> => invoke<boolean>("desktop_update_supported"),
    currentVersion: getVersion,
    check: async (): Promise<Update | null> => check({ timeout: 30_000 }),
    relaunch,
    updateGateway: async (): Promise<GatewayUpdate> => invoke<GatewayUpdate>("update_gateway"),
  };
}

const EMPTY: UpdateSnapshot = {
  phase: "idle",
  currentVersion: "",
  version: "",
  notes: "",
  downloaded: 0,
  total: null,
  error: "",
  log: [],
  background: false,
  gateway: null,
};

function oneLine(text: string | undefined): string {
  return (text ?? "").split(/\r?\n/, 1)[0]?.trim() ?? "";
}

/** The log line a gateway answer adds; most outcomes add none. */
function gatewayLogLine(gateway: GatewayUpdate): string | null {
  if (gateway.outcome === "updating") {
    return `PromptForge Gateway is updating to ${gateway.version}.`;
  }
  if (gateway.outcome === "failed") {
    return `PromptForge Gateway update failed: ${gateway.message}`;
  }
  return null;
}

export class UpdateService extends Disposable {
  private readonly changes = this._register(new Emitter<UpdateSnapshot>());
  private state: UpdateSnapshot = EMPTY;
  private update: DesktopUpdate | null = null;
  private checking: Promise<boolean> | null = null;
  private gatewayRequest: Promise<void> | null = null;
  private disposed = false;

  readonly onDidChange: Event<UpdateSnapshot> = this.changes.event;

  constructor(private readonly backend: UpdateBackend = defaultBackend()) {
    super();
    if (!backend.desktop) {
      this.state = { ...EMPTY, phase: "browser" };
    }
  }

  get snapshot(): UpdateSnapshot {
    return this.state;
  }

  startAutoCheck(delayMs = 5_000): void {
    if (!this.backend.desktop) {
      return;
    }
    const timer = window.setTimeout(() => void this.checkOnce(true), delayMs);
    this._register(toDisposable(() => window.clearTimeout(timer)));
  }

  async checkNow(): Promise<void> {
    return this.checkOnce(false);
  }

  private async checkOnce(background: boolean): Promise<void> {
    if (this.disposed) {
      return;
    }
    if (!this.backend.desktop) {
      this.publish({ phase: "browser", error: "" });
      return;
    }
    if (this.checking !== null) {
      // The first caller's continuation runs first and starts the
      // gateway's request, which this caller then waits for too.
      await this.checking;
      await this.gatewayRequest;
      return;
    }
    // A request still in flight from an earlier check is joined rather
    // than repeated, even if it answers while Workshop's check runs.
    const joined = this.gatewayRequest;
    this.checking = this.checkWorkshop(background);
    let askGateway: boolean;
    try {
      askGateway = await this.checking;
    } finally {
      this.checking = null;
    }
    if (!askGateway || this.disposed) {
      return;
    }
    if (joined === null) {
      await this.askGateway();
      return;
    }
    await joined;
    this.restoreGatewayLine();
  }

  /**
   * Workshop's check starts a fresh log, which erases the line of a gateway
   * answer that arrived while it ran; this puts the line back.
   */
  private restoreGatewayLine(): void {
    const line = this.state.gateway === null ? null : gatewayLogLine(this.state.gateway);
    if (line !== null && !this.state.log.includes(line)) {
      this.publish({ log: [...this.state.log, line] });
    }
  }

  remindLater(): void {
    if (this.update !== null) {
      this.publish({ phase: "dismissed" });
    }
  }

  showAvailable(): void {
    if (this.update !== null) {
      this.publish({ phase: "available" });
    }
  }

  async install(): Promise<void> {
    const update = this.update;
    if (update === null) {
      return;
    }
    let downloaded = 0;
    this.publish({
      phase: "downloading",
      downloaded,
      total: null,
      error: "",
      log: [...this.state.log, `Downloading Runvil ${update.version}`],
      background: false,
    });
    try {
      await update.downloadAndInstall((event) => {
        if (event.event === "Started") {
          this.publish({
            phase: "downloading",
            total: event.data.contentLength ?? null,
          });
        } else if (event.event === "Progress") {
          downloaded += event.data.chunkLength;
          this.publish({ phase: "downloading", downloaded });
        } else {
          this.publish({
            phase: "installing",
            log: [...this.state.log, "Download complete. Installing update."],
          });
        }
      });
      this.publish({
        phase: "restarting",
        log: [...this.state.log, "Update installed. Restarting Runvil."],
      });
      await this.backend.relaunch();
    } catch (error) {
      const message = errorText(error);
      this.publish({
        phase: "error",
        error: message,
        log: [...this.state.log, `Update failed: ${message}`],
      });
    }
  }

  override dispose(): void {
    this.disposed = true;
    if (this.update !== null) {
      void this.update.close();
      this.update = null;
    }
    super.dispose();
  }

  /**
   * The gateway's update request, which can take minutes while the gateway
   * downloads, so it has its own guard: a check during it runs Workshop's
   * check again and then joins the request in flight instead of sending a
   * second one, which the gateway would only answer busy.
   */
  private askGateway(): Promise<void> {
    if (this.gatewayRequest === null) {
      this.gatewayRequest = this.updateGateway().finally(() => {
        this.gatewayRequest = null;
      });
    }
    return this.gatewayRequest;
  }

  /**
   * Workshop's own check. Resolves whether the gateway's update follows: it
   * does after a current, an available, and a failed check, and not on a
   * package-managed install, where the package manager owns updates, or
   * once the service is disposed.
   */
  private async checkWorkshop(background: boolean): Promise<boolean> {
    this.publish({ phase: "checking", error: "", background });
    try {
      const currentVersion = await this.backend.currentVersion();
      if (!(await this.backend.supported())) {
        this.publish({ phase: "unsupported", currentVersion, error: "" });
        return false;
      }
      const update = await this.backend.check();
      if (this.disposed) {
        await update?.close();
        return false;
      }
      if (this.update !== null && this.update !== update) {
        await this.update.close();
      }
      this.update = update;
      if (update === null) {
        this.publish({
          phase: "current",
          currentVersion,
          version: "",
          notes: "",
          downloaded: 0,
          total: null,
          log: [],
        });
        return true;
      }
      this.publish({
        phase: "available",
        currentVersion: update.currentVersion || currentVersion,
        version: update.version,
        notes: oneLine(update.body),
        downloaded: 0,
        total: null,
        log: [`Runvil ${update.version} is available.`],
      });
    } catch (error) {
      const message = errorText(error);
      this.publish({
        phase: "error",
        error: message,
        log: [`Update check failed: ${message}`],
      });
    }
    return true;
  }

  /**
   * Asks the local gateway to update itself and records its answer. It never
   * changes Workshop's phase or error, so no toast fires, and a rejected
   * invoke is recorded as a failure rather than thrown.
   */
  private async updateGateway(): Promise<void> {
    let gateway: GatewayUpdate;
    try {
      gateway = await this.backend.updateGateway();
    } catch (error) {
      gateway = { outcome: "failed", message: errorText(error) };
    }
    const line = gatewayLogLine(gateway);
    this.publish(line === null ? { gateway } : { gateway, log: [...this.state.log, line] });
  }

  private publish(change: Partial<UpdateSnapshot>): void {
    if (this.disposed) {
      return;
    }
    this.state = { ...this.state, ...change };
    this.changes.fire(this.state);
  }
}
