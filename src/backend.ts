/* ───────────────────────── Backend bridge ─────────────────────────
 * The ONE file that talks to the Rust backend. Every Tauri `invoke` and
 * Channel lives here, so the rest of the frontend depends on plain typed
 * functions — one place to mock, one place to handle a Tauri API change.
 *
 * When running inside the Tauri shell we call the real Rust commands. When
 * opened in a plain browser (e.g. `npm run dev`) we fall back to the sample
 * data so the UI is still fully explorable.
 * ------------------------------------------------------------------ */
import {
  type ScanOptions,
  type ScanEvent,
  type CleanEvent,
  type Location,
  type Category,
  type RuleFile,
  type RulesStatus,
} from "./types";
import { rollupCategories } from "./grouping";
import { SAMPLE, SAMPLE_CACHES } from "./sample-data";

export const IS_TAURI = "__TAURI_INTERNALS__" in window;

const delay = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Callbacks the scan drives as progress streams in. */
export interface ScanHandlers {
  onDiscovering(scanned: number): void;
  onDiscovered(total: number): void;
  onLocated(location: Location, done: number, total: number): void;
  onDone(root: string, categories: Category[], errors: number, errorSamples: string[]): void;
}

/** Route one streamed Channel event to the handler set. */
function dispatch(h: ScanHandlers, ev: ScanEvent): void {
  switch (ev.kind) {
    case "discovering":
      h.onDiscovering(ev.scanned);
      break;
    case "discovered":
      h.onDiscovered(ev.total);
      break;
    case "located":
      h.onLocated(ev.location, ev.done, ev.total);
      break;
    case "done":
      h.onDone(ev.root, ev.categories, ev.errors, ev.error_samples);
      break;
  }
}

/** Browser-preview cancel flag (the Tauri path cancels via the backend command). */
let browserScanCancelled = false;

/** Ask the running scan to stop. In the Tauri shell this signals the backend; in
 *  a plain browser it flips a flag the simulate loops check. */
export async function cancelScan(): Promise<void> {
  if (IS_TAURI) {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("cancel_scan");
    return;
  }
  browserScanCancelled = true;
}

/**
 * Run a project scan, dispatching streamed progress to `handlers`. In the Tauri
 * shell this opens a Channel to the Rust scanner; in a plain browser it replays
 * the sample data through the same handler sequence so the UI stays explorable.
 */
export async function runScan(opts: ScanOptions, handlers: ScanHandlers): Promise<void> {
  if (IS_TAURI) {
    const { invoke, Channel } = await import("@tauri-apps/api/core");
    const channel = new Channel<ScanEvent>();
    channel.onmessage = (ev) => dispatch(handlers, ev);
    await invoke("scan", { opts, onEvent: channel });
    return;
  }
  await simulateScan(handlers);
}

/** Scan the per-user system caches (the ruleset's [[global_cache]] entries). */
export async function runScanCaches(handlers: ScanHandlers): Promise<void> {
  if (IS_TAURI) {
    const { invoke, Channel } = await import("@tauri-apps/api/core");
    const channel = new Channel<ScanEvent>();
    channel.onmessage = (ev) => dispatch(handlers, ev);
    await invoke("scan_caches", { onEvent: channel });
    return;
  }
  await simulateCaches(handlers);
}

/** Browser-only: fake the streaming sequence from SAMPLE for UI preview. */
async function simulateScan(h: ScanHandlers): Promise<void> {
  browserScanCancelled = false;
  for (let i = 1; i <= 6; i++) {
    if (browserScanCancelled) return h.onDone(SAMPLE.root, [], 0, []);
    await delay(110);
    h.onDiscovering(i * 240);
  }
  const locs = SAMPLE.locations;
  h.onDiscovered(locs.length);
  let done = 0;
  for (const loc of locs) {
    if (browserScanCancelled) break;
    await delay(140);
    h.onLocated(loc, ++done, locs.length);
  }
  h.onDone(SAMPLE.root, SAMPLE.categories, 0, []);
}

/** Browser-only preview of the system-caches view. */
async function simulateCaches(h: ScanHandlers): Promise<void> {
  browserScanCancelled = false;
  const caches = SAMPLE_CACHES;
  for (let i = 1; i <= 3; i++) {
    if (browserScanCancelled) return h.onDone("System caches", [], 0, []);
    await delay(120);
    h.onDiscovering(i * 2);
  }
  h.onDiscovered(caches.length);
  let done = 0;
  for (const c of caches) {
    if (browserScanCancelled) break;
    await delay(160);
    h.onLocated(c, ++done, caches.length);
  }
  h.onDone("System caches", rollupCategories(caches), 0, []);
}

/** Callbacks the clean drives as per-path results stream in. */
export interface CleanHandlers {
  onRemoving(path: string, done: number, total: number): void;
  onRemoved(path: string, done: number, total: number): void;
  onFailed(path: string, error: string, done: number, total: number): void;
  onDone(removed: number, failed: number): void;
}

/** Route one streamed clean Channel event to the handler set. */
function dispatchClean(h: CleanHandlers, ev: CleanEvent): void {
  switch (ev.kind) {
    case "removing":
      h.onRemoving(ev.path, ev.done, ev.total);
      break;
    case "removed":
      h.onRemoved(ev.path, ev.done, ev.total);
      break;
    case "failed":
      h.onFailed(ev.path, ev.error, ev.done, ev.total);
      break;
    case "done":
      h.onDone(ev.removed, ev.failed);
      break;
  }
}

/**
 * Delete `paths`, dispatching streamed per-path progress to `handlers`. In the
 * Tauri shell this opens a Channel to the Rust `clean` command; in a plain
 * browser it replays a fake sequence so the UI stays explorable.
 */
export async function runClean(
  paths: string[],
  toTrash: boolean,
  handlers: CleanHandlers,
): Promise<void> {
  if (IS_TAURI) {
    const { invoke, Channel } = await import("@tauri-apps/api/core");
    const channel = new Channel<CleanEvent>();
    channel.onmessage = (ev) => dispatchClean(handlers, ev);
    await invoke("clean", { paths, toTrash, onEvent: channel });
    return;
  }
  await simulateClean(paths, handlers);
}

/** Browser-only: fake the clean stream, failing the last path (when there is
 *  more than one) so the failed-row treatment stays explorable. */
async function simulateClean(paths: string[], h: CleanHandlers): Promise<void> {
  const total = paths.length;
  let removed = 0;
  let failed = 0;
  for (const path of paths) {
    h.onRemoving(path, removed + failed, total);
    await delay(260);
    if (total > 1 && path === paths[paths.length - 1]) {
      failed++;
      h.onFailed(path, "in use (simulated)", removed + failed, total);
    } else {
      removed++;
      h.onRemoved(path, removed + failed, total);
    }
  }
  h.onDone(removed, failed);
}

/** Native folder picker. `null` when cancelled or in the browser preview. */
export async function pickFolder(): Promise<string | null> {
  if (!IS_TAURI) return null;
  const { open } = await import("@tauri-apps/plugin-dialog");
  const sel = await open({ directory: true });
  return typeof sel === "string" ? sel : null;
}

/** Window controls for the custom titlebar (no-ops in the browser preview). */
export async function windowAction(action: "minimize" | "maximize" | "close"): Promise<void> {
  if (!IS_TAURI) return;
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  const w = getCurrentWindow();
  if (action === "minimize") await w.minimize();
  else if (action === "maximize") await w.toggleMaximize();
  else if (action === "close") await w.close();
}

/** Open the log/crash-report folder in the OS file manager and return its path.
 *  `null` in the browser preview (there is no backend, hence no logs). */
export async function openLogsDir(): Promise<string | null> {
  if (!IS_TAURI) return null;
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<string>("open_logs_dir");
}

/* ── rules commands (desktop only — callers keep their own browser fallbacks,
 *    e.g. the editor previews a sample ruleset and toasts on save) ───────── */

export async function loadRules(): Promise<RuleFile> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<RuleFile>("load_rules");
}

export async function saveRules(rules: RuleFile): Promise<void> {
  const { invoke } = await import("@tauri-apps/api/core");
  await invoke("save_rules", { rules });
}

export async function resetRules(): Promise<void> {
  const { invoke } = await import("@tauri-apps/api/core");
  await invoke("reset_rules");
}

/** Create the override file if needed and open it in the user's editor. */
export async function openRulesFile(): Promise<void> {
  const { invoke } = await import("@tauri-apps/api/core");
  await invoke("open_rules_file");
}

export async function rulesStatus(): Promise<RulesStatus> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<RulesStatus>("rules_status");
}
