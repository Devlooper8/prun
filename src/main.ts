import "./styles.css";
import {
  type ScanResult,
  type ScanOptions,
  type ScanEvent,
  type CleanEvent,
  type Location,
  type Category,
  type CategoryId,
  categoryColor,
  categoryLabel,
} from "./types";
import { enterRulesView } from "./rules-editor";

/* ───────────────────────── Tauri bridge ─────────────────────────
 * When running inside the Tauri shell we call the real Rust scanner.
 * When opened in a plain browser (e.g. `vite` preview) we fall back to
 * the sample data so the UI is still fully explorable.
 * ----------------------------------------------------------------- */
const IS_TAURI = "__TAURI_INTERNALS__" in window;

/** Callbacks the scan drives as progress streams in. */
interface ScanHandlers {
  onDiscovering(scanned: number): void;
  onDiscovered(total: number): void;
  onLocated(location: Location, done: number, total: number): void;
  onDone(root: string, categories: Category[]): void;
}

/** Route one streamed Channel event to the handler set. */
function dispatch(h: ScanHandlers, ev: ScanEvent): void {
  switch (ev.kind) {
    case "discovering": h.onDiscovering(ev.scanned); break;
    case "discovered": h.onDiscovered(ev.total); break;
    case "located": h.onLocated(ev.location, ev.done, ev.total); break;
    case "done": h.onDone(ev.root, ev.categories); break;
  }
}

/**
 * Run a project scan, dispatching streamed progress to `handlers`. In the Tauri
 * shell this opens a Channel to the Rust scanner; in a plain browser it replays
 * the sample data through the same handler sequence so the UI stays explorable.
 */
async function runScan(opts: ScanOptions, handlers: ScanHandlers): Promise<void> {
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
async function runScanCaches(handlers: ScanHandlers): Promise<void> {
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
  for (let i = 1; i <= 6; i++) {
    await delay(110);
    h.onDiscovering(i * 240);
  }
  const locs = SAMPLE.locations;
  h.onDiscovered(locs.length);
  let done = 0;
  for (const loc of locs) {
    await delay(140);
    h.onLocated(loc, ++done, locs.length);
  }
  h.onDone(SAMPLE.root, SAMPLE.categories);
}

/** Browser-only preview of the system-caches view. */
async function simulateCaches(h: ScanHandlers): Promise<void> {
  const GB = 1e9;
  const caches: Location[] = [
    { path: "~/.cargo/registry/cache", project: "Cargo registry & git cache", artifact: "/cache", category: "rust", size: 3.4 * GB, age_secs: 90 * 86400, git_ignored: true },
    { path: "~/.gradle/caches", project: "Gradle cache", artifact: "/caches", category: "jvm", size: 2.1 * GB, age_secs: 45 * 86400, git_ignored: true },
    { path: "~/.npm/_cacache", project: "npm cache", artifact: "/_cacache", category: "node", size: 1.2 * GB, age_secs: 60 * 86400, git_ignored: true },
  ];
  for (let i = 1; i <= 3; i++) {
    await delay(120);
    h.onDiscovering(i * 2);
  }
  h.onDiscovered(caches.length);
  let done = 0;
  for (const c of caches) {
    await delay(160);
    h.onLocated(c, ++done, caches.length);
  }
  h.onDone("System caches", rollupCategories(caches));
}

/** Callbacks the clean drives as per-path results stream in. */
interface CleanHandlers {
  onRemoving(path: string, done: number, total: number): void;
  onRemoved(path: string, done: number, total: number): void;
  onFailed(path: string, error: string, done: number, total: number): void;
  onDone(removed: number, failed: number): void;
}

/** Route one streamed clean Channel event to the handler set. */
function dispatchClean(h: CleanHandlers, ev: CleanEvent): void {
  switch (ev.kind) {
    case "removing": h.onRemoving(ev.path, ev.done, ev.total); break;
    case "removed": h.onRemoved(ev.path, ev.done, ev.total); break;
    case "failed": h.onFailed(ev.path, ev.error, ev.done, ev.total); break;
    case "done": h.onDone(ev.removed, ev.failed); break;
  }
}

/**
 * Delete `paths`, dispatching streamed per-path progress to `handlers`. In the
 * Tauri shell this opens a Channel to the Rust `clean` command; in a plain
 * browser it replays a fake sequence so the UI stays explorable.
 */
async function runClean(
  paths: string[],
  toTrash: boolean,
  handlers: CleanHandlers
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

async function pickFolder(): Promise<string | null> {
  if (!IS_TAURI) return null;
  const { open } = await import("@tauri-apps/plugin-dialog");
  const sel = await open({ directory: true });
  return typeof sel === "string" ? sel : null;
}

/* window controls (custom titlebar) */
async function windowAction(action: "minimize" | "maximize" | "close") {
  if (!IS_TAURI) return;
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  const w = getCurrentWindow();
  if (action === "minimize") await w.minimize();
  else if (action === "maximize") await w.toggleMaximize();
  else if (action === "close") await w.close();
}

/* ───────────────────────── State ─────────────────────────────── */
const state = {
  result: null as ScanResult | null,
  selected: new Set<string>(), // selected location paths
  catsOn: new Set<string>(), // enabled category ids
  filters: { age: false, git: false, prunignore: false },
  ageDays: 14,
  scanning: false, // guards against overlapping scans
  cleaning: false, // guards against overlapping cleans / scans during a clean
  failed: new Map<string, string>(), // path → error for rows a clean couldn't remove
  expanded: new Set<string>(), // project groups currently expanded
  mode: "scan" as "scan" | "caches", // scan list vs system-caches list
  view: "clean" as "clean" | "rules", // top-level screen (left nav rail)
};

/* ───────────────────────── DOM refs ──────────────────────────── */
const $ = <T extends Element>(s: string) => document.querySelector<T>(s)!;
const catsList = $<HTMLUListElement>("#cats-list");
const locsList = $<HTMLUListElement>("#locs-list");
const selCount = $<HTMLSpanElement>("#sel-count");
const selSize = $<HTMLSpanElement>("#sel-size");
const rootInput = $<HTMLInputElement>("#root");
const ageInput = $<HTMLInputElement>("#age-days");
const cleanBtn = $<HTMLButtonElement>("#clean");
const trashCb = $<HTMLInputElement>("#trash");
const rescanBtn = $<HTMLButtonElement>("#rescan");
const cachesBtn = $<HTMLButtonElement>("#caches");
const scanbar = $<HTMLDivElement>("#scanbar");
const scanFill = $<HTMLDivElement>("#scan-fill");
const scanRoot = $<HTMLSpanElement>("#scan-root");
const scanPct = $<HTMLSpanElement>("#scan-pct");
const viewClean = $<HTMLElement>("#view-clean");
const viewRules = $<HTMLElement>("#view-rules");

/* ───────────────────────── Helpers ───────────────────────────── */
const delay = (ms: number) => new Promise((r) => setTimeout(r, ms));

function fmtSize(bytes: number): string {
  if (bytes >= 1e9) return `${(bytes / 1e9).toFixed(1)} GB`;
  if (bytes >= 1e6) return `${(bytes / 1e6).toFixed(0)} MB`;
  if (bytes >= 1e3) return `${(bytes / 1e3).toFixed(0)} KB`;
  return `${bytes} B`;
}

function visibleLocations(): Location[] {
  if (!state.result) return [];
  return state.result.locations
    .filter((loc) => {
      if (state.catsOn.size && !state.catsOn.has(loc.category)) return false;
      if (state.filters.age && loc.age_secs < state.ageDays * 86400) return false;
      if (state.filters.git && !loc.git_ignored) return false;
      return true;
    })
    .sort((a, b) => b.size - a.size); // biggest first; results stream in unsorted
}

/** Build the category roll-up from current locations — live, as a scan streams. */
function rollupCategories(locations: Location[]): Category[] {
  const totals = new Map<CategoryId, number>();
  for (const l of locations)
    totals.set(l.category, (totals.get(l.category) ?? 0) + l.size);
  return [...totals]
    .map(([id, size]) => ({ id, label: categoryLabel(id), size }))
    .sort((a, b) => b.size - a.size);
}

const escMap: Record<string, string> = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" };
const esc = (s: string) => s.replace(/[&<>"]/g, (c) => escMap[c]);

/* ── Project grouping ───────────────────────────────────────────── */
interface ProjectGroup {
  name: string; // top-level folder under the scan root
  locations: Location[];
  size: number;
}

/** Path with the scan root stripped and leading separators removed. */
function relUnderRoot(path: string, root: string): string {
  const rel = path.startsWith(root) ? path.slice(root.length) : path;
  return rel.replace(/^[\\/]+/, "");
}

/** Grouping key: the project = first folder under the scan root.
 *  e.g. <root>/prun/src-tauri/target  →  "prun" (not "src-tauri"). */
function projectKeyOf(path: string, root: string): string {
  const rel = relUnderRoot(path, root);
  return rel.split(/[\\/]/)[0] || rel || path;
}

/** The artifact's location within its project, e.g. "src-tauri/target". */
function subPathOf(path: string, root: string): string {
  const parts = relUnderRoot(path, root).split(/[\\/]/);
  return parts.slice(1).join("/") || parts[0] || path;
}

/** Distinct categories present in a set of locations, biggest-footprint first. */
function distinctCategories(locs: Location[]): CategoryId[] {
  const size = new Map<CategoryId, number>();
  for (const l of locs) size.set(l.category, (size.get(l.category) ?? 0) + l.size);
  return [...size].sort((a, b) => b[1] - a[1]).map(([c]) => c);
}

/** Grouping key: the project folder for a normal scan, or the cache name in the
 *  system-caches view (cache paths are absolute, so the project segment is the
 *  meaningful label there). */
function groupKey(loc: Location, root: string): string {
  if (state.mode === "caches") return loc.project || loc.category;
  return projectKeyOf(loc.path, root);
}

/** Roll the (already filtered) locations up into project groups, biggest first. */
function groupByProject(locations: Location[], root: string): ProjectGroup[] {
  const groups = new Map<string, Location[]>();
  for (const loc of locations) {
    const key = groupKey(loc, root);
    let bucket = groups.get(key);
    if (!bucket) groups.set(key, (bucket = []));
    bucket.push(loc);
  }
  return [...groups]
    .map(([name, locs]) => ({
      name,
      locations: locs.sort((a, b) => b.size - a.size),
      size: locs.reduce((s, l) => s + l.size, 0),
    }))
    .sort((a, b) => b.size - a.size);
}

/* ── progress strip ─────────────────────────────────────────────── *
 * Shows only the directory being scanned, with a classic blue bar under it:
 * an indeterminate marquee while discovering (total unknown), then a
 * determinate fill while sizing. No per-file paths or counts. */
function showScanbar(rootLabel: string) {
  scanbar.hidden = false;
  scanRoot.textContent = rootLabel;
  scanPct.textContent = "";
  scanbar.classList.add("scanbar--indeterminate");
  scanFill.style.width = ""; // clear inline width so the CSS marquee applies
}
function hideScanbar() {
  scanbar.hidden = true;
  scanbar.classList.remove("scanbar--indeterminate");
  scanFill.style.width = "0%";
  scanPct.textContent = "";
}
function scanDiscovering() {
  scanbar.classList.add("scanbar--indeterminate");
  scanFill.style.width = "";
  scanPct.textContent = "";
}
function scanSizing(frac: number) {
  scanbar.classList.remove("scanbar--indeterminate");
  const pct = Math.min(100, Math.max(0, frac * 100));
  scanFill.style.width = `${pct}%`;
  scanPct.textContent = `${Math.round(pct)}%`;
}

/* ── clean progress ─────────────────────────────────────────────── *
 * Reuses the scan strip as a determinate "Cleaning…" bar: the total is known up
 * front, so it starts determinate and advances as each path is removed. */
function showCleanbar() {
  scanbar.hidden = false;
  scanbar.classList.remove("scanbar--indeterminate");
  scanRoot.textContent = "Cleaning…";
  scanFill.style.width = "0%";
  scanPct.textContent = "0%";
}
/** Last two segments of a path, e.g. "space-sim/target" — the meaningful tail. */
function shortPath(path: string): string {
  const parts = path.split(/[\\/]+/).filter(Boolean);
  return parts.slice(-2).join("/") || path;
}
function cleanProgress(path: string, done: number, total: number) {
  scanRoot.textContent = `Cleaning… ${shortPath(path)}`;
  scanSizing(total === 0 ? 1 : done / total);
}

/* Coalesce bursts of streamed `located` events into one repaint per frame. */
let rafPending = false;
function scheduleRender() {
  if (rafPending) return;
  rafPending = true;
  requestAnimationFrame(() => {
    rafPending = false;
    if (state.result)
      state.result.categories = rollupCategories(state.result.locations);
    render();
  });
}

/* ───────────────────────── Render ────────────────────────────── */
function render() {
  const res = state.result;
  if (!res) return;

  // categories
  catsList.innerHTML = "";
  for (const cat of res.categories) {
    const li = document.createElement("li");
    li.className = "cat";
    li.innerHTML = `
      <input class="cb" type="checkbox" ${state.catsOn.size === 0 || state.catsOn.has(cat.id) ? "checked" : ""}>
      <span class="dot" style="background:${categoryColor(cat.id)}"></span>
      <span class="cat__name">${esc(cat.label)}</span>
      <span class="cat__size">${fmtSize(cat.size)}</span>`;
    const cb = li.querySelector<HTMLInputElement>(".cb")!;
    cb.addEventListener("change", () => {
      // empty set == "all on"; first manual toggle materialises the set
      if (state.catsOn.size === 0)
        res.categories.forEach((c) => state.catsOn.add(c.id));
      cb.checked ? state.catsOn.add(cat.id) : state.catsOn.delete(cat.id);
      if (state.catsOn.size === res.categories.length) state.catsOn.clear();
      reconcileSelection();
      render();
    });
    catsList.appendChild(li);
  }

  // locations — grouped by project (top-level folder under the scan root)
  const groups = groupByProject(visibleLocations(), res.root);
  locsList.innerHTML = "";
  for (const g of groups) locsList.appendChild(renderGroup(g, res.root));

  updateFooter();
}

/** A project header (arrow + tri-state checkbox + name + dots + count + size)
 *  with its child location rows; expand/collapse and selection update in place
 *  so the scroll position is never lost. */
function renderGroup(g: ProjectGroup, root: string): HTMLLIElement {
  const li = document.createElement("li");
  li.className = "group";
  const expanded = state.expanded.has(g.name);
  const sel = g.locations.filter((l) => state.selected.has(l.path)).length;
  const allSel = sel === g.locations.length;
  const dots = distinctCategories(g.locations)
    .map((c) => `<span class="dot" style="background:${categoryColor(c)}"></span>`)
    .join("");

  li.innerHTML = `
    <div class="group__head">
      <button class="group__arrow${expanded ? " is-open" : ""}" aria-label="Expand project" aria-expanded="${expanded}">
        <svg viewBox="0 0 24 24" width="14" height="14"><path d="M9 6l6 6-6 6"/></svg>
      </button>
      <input class="cb" type="checkbox" ${allSel ? "checked" : ""}>
      <span class="group__name">${esc(g.name)}</span>
      <span class="group__dots">${dots}</span>
      <span class="group__count">${g.locations.length}</span>
      <span class="group__size">${fmtSize(g.size)}</span>
    </div>
    <ul class="group__children"${expanded ? "" : " hidden"}></ul>`;

  const head = li.querySelector<HTMLDivElement>(".group__head")!;
  const arrow = li.querySelector<HTMLButtonElement>(".group__arrow")!;
  const groupCb = li.querySelector<HTMLInputElement>(".cb")!;
  const childrenUl = li.querySelector<HTMLUListElement>(".group__children")!;
  groupCb.indeterminate = sel > 0 && !allSel;

  for (const loc of g.locations)
    childrenUl.appendChild(renderChild(loc, g, groupCb, root));

  const toggleExpand = () => {
    const open = childrenUl.hasAttribute("hidden");
    childrenUl.toggleAttribute("hidden", !open);
    arrow.classList.toggle("is-open", open);
    arrow.setAttribute("aria-expanded", String(open));
    open ? state.expanded.add(g.name) : state.expanded.delete(g.name);
  };
  arrow.addEventListener("click", (e) => {
    e.stopPropagation();
    toggleExpand();
  });
  head.addEventListener("click", (e) => {
    const t = e.target as HTMLElement;
    if (t !== groupCb && !arrow.contains(t)) toggleExpand();
  });

  groupCb.addEventListener("change", () => {
    const on = groupCb.checked;
    for (const loc of g.locations)
      on ? state.selected.add(loc.path) : state.selected.delete(loc.path);
    childrenUl.querySelectorAll<HTMLInputElement>(".cb").forEach((cb) => (cb.checked = on));
    groupCb.indeterminate = false;
    updateFooter();
  });

  return li;
}

function renderChild(
  loc: Location,
  g: ProjectGroup,
  groupCb: HTMLInputElement,
  root: string
): HTMLLIElement {
  const li = document.createElement("li");
  const failure = state.failed.get(loc.path);
  li.className = failure ? "loc loc--child loc--failed" : "loc loc--child";
  if (failure) li.title = `Couldn't remove — ${failure}`;
  const sub = subPathOf(loc.path, root);
  const cut = sub.lastIndexOf("/");
  const prefix = cut >= 0 ? sub.slice(0, cut + 1) : "";
  const leaf = cut >= 0 ? sub.slice(cut + 1) : sub;
  li.innerHTML = `
    <input class="cb" type="checkbox" ${state.selected.has(loc.path) ? "checked" : ""}>
    <span class="dot" style="background:${categoryColor(loc.category)}"></span>
    <span class="loc__path"><span class="loc__sub">${esc(prefix)}</span><span class="loc__leaf">${esc(leaf)}</span></span>
    <span class="loc__size">${fmtSize(loc.size)}</span>`;
  const cb = li.querySelector<HTMLInputElement>(".cb")!;
  const sync = () => {
    cb.checked ? state.selected.add(loc.path) : state.selected.delete(loc.path);
    refreshGroupCheckbox(groupCb, g);
    updateFooter();
  };
  cb.addEventListener("change", sync);
  li.addEventListener("click", (e) => {
    if (e.target !== cb) {
      cb.checked = !cb.checked;
      sync();
    }
  });
  return li;
}

/** Reflect child selection back onto the project checkbox (checked / mixed / off). */
function refreshGroupCheckbox(groupCb: HTMLInputElement, g: ProjectGroup) {
  const sel = g.locations.filter((l) => state.selected.has(l.path)).length;
  groupCb.checked = sel === g.locations.length;
  groupCb.indeterminate = sel > 0 && sel < g.locations.length;
}

function reconcileSelection() {
  const visible = new Set(visibleLocations().map((l) => l.path));
  for (const p of [...state.selected]) if (!visible.has(p)) state.selected.delete(p);
}

function updateFooter() {
  const res = state.result;
  if (!res) return;
  const chosen = res.locations.filter((l) => state.selected.has(l.path));
  const total = chosen.reduce((s, l) => s + l.size, 0);
  selCount.textContent = String(chosen.length);
  selSize.textContent = fmtSize(total);
  // stays disabled mid-clean even though rows (and selection) shrink live
  cleanBtn.disabled = state.cleaning || chosen.length === 0;
}

/* ───────────────────────── Actions ───────────────────────────── */
async function doScan() {
  if (state.scanning || state.cleaning) return; // ignore overlapping scans/cleans
  const opts: ScanOptions = {
    root: rootInput.value.trim() || "~/Projects",
    minAgeDays: state.filters.age ? state.ageDays : null,
    skipGitTracked: state.filters.git,
    respectPrunignore: state.filters.prunignore,
  };

  // Reset to an empty live result the stream will fill in.
  state.scanning = true;
  state.mode = "scan";
  rescanBtn.disabled = true;
  cachesBtn.disabled = true;
  state.result = { root: opts.root, categories: [], locations: [] };
  state.selected.clear();
  state.catsOn.clear();
  state.expanded.clear();
  state.failed.clear();
  let maxDone = 0;

  showScanbar(opts.root);
  render();

  try {
    await runScan(opts, {
      onDiscovering() {
        scanDiscovering();
      },
      onDiscovered(total) {
        maxDone = 0;
        scanSizing(total === 0 ? 1 : 0);
      },
      onLocated(location, done, total) {
        maxDone = Math.max(maxDone, done); // parallel events can arrive out of order
        state.result!.locations.push(location);
        scanSizing(maxDone / total);
        scheduleRender();
      },
      onDone(root, categories) {
        state.result!.root = root;
        state.result!.categories = categories;
      },
    });

    // select everything reclaimable by default — matches the screenshot
    state.selected = new Set(visibleLocations().map((l) => l.path));
    hideScanbar();
    render();
    toast(`Found ${state.result.locations.length} locations`);
  } catch (err) {
    hideScanbar();
    render();
    toast(`Scan failed: ${err}`);
  } finally {
    state.scanning = false;
    rescanBtn.disabled = false;
    cachesBtn.disabled = false;
  }
}

/** Scan the per-user system caches. A separate view: never auto-selected, since
 *  these are shared across projects and slow to rebuild. */
async function doScanCaches() {
  if (state.scanning || state.cleaning) return;
  state.scanning = true;
  state.mode = "caches";
  rescanBtn.disabled = true;
  cachesBtn.disabled = true;
  state.result = { root: "System caches", categories: [], locations: [] };
  state.selected.clear();
  state.catsOn.clear();
  state.expanded.clear();
  state.failed.clear();
  let maxDone = 0;

  showScanbar("System caches");
  render();

  try {
    await runScanCaches({
      onDiscovering() {
        scanDiscovering();
      },
      onDiscovered(total) {
        maxDone = 0;
        scanSizing(total === 0 ? 1 : 0);
      },
      onLocated(location, done, total) {
        maxDone = Math.max(maxDone, done);
        state.result!.locations.push(location);
        scanSizing(maxDone / total);
        scheduleRender();
      },
      onDone(root, categories) {
        state.result!.root = root;
        state.result!.categories = categories;
      },
    });

    hideScanbar();
    render();
    toast(`Found ${state.result.locations.length} system caches`);
  } catch (err) {
    hideScanbar();
    render();
    toast(`Cache scan failed: ${err}`);
  } finally {
    state.scanning = false;
    rescanBtn.disabled = false;
    cachesBtn.disabled = false;
  }
}

/** Drop one location from the list + selection as its deletion confirms (and
 *  clear any stale failure for it, e.g. on a successful retry). */
function removeLocation(path: string) {
  if (!state.result) return;
  state.result.locations = state.result.locations.filter((l) => l.path !== path);
  state.selected.delete(path);
  state.failed.delete(path);
}

/** Delete the selected locations, streaming progress. Each row disappears the
 *  moment its deletion confirms; paths that fail (e.g. a file inside is in use)
 *  stay listed and selected — marked — so they can be retried immediately. */
async function doClean() {
  if (state.cleaning) return; // ignore overlapping cleans
  const res = state.result;
  if (!res) return;
  // largest-first: the biggest reclaims (and most visible progress) land first
  const chosen = res.locations
    .filter((l) => state.selected.has(l.path))
    .sort((a, b) => b.size - a.size);
  if (!chosen.length) return;
  const paths = chosen.map((l) => l.path);
  const toTrash = trashCb.checked;
  const verb = toTrash ? "moved to Trash" : "deleted";

  state.cleaning = true;
  state.failed = new Map();
  cleanBtn.disabled = true;
  rescanBtn.disabled = true;
  cachesBtn.disabled = true;
  showCleanbar();

  try {
    await runClean(paths, toTrash, {
      onRemoving(path, done, total) {
        cleanProgress(path, done, total);
      },
      onRemoved(path, done, total) {
        removeLocation(path);
        cleanProgress(path, done, total);
        scheduleRender(); // shrink the list live (coalesced to one repaint/frame)
      },
      onFailed(path, error, done, total) {
        state.failed.set(path, error); // keep it listed + selected for retry
        cleanProgress(path, done, total);
        scheduleRender();
      },
      onDone() {},
    });

    hideScanbar();
    res.categories = rollupCategories(res.locations); // drop emptied categories
    render();
    const removed = paths.length - state.failed.size;
    toast(
      state.failed.size === 0
        ? `${removed} location${removed === 1 ? "" : "s"} ${verb}`
        : `${removed} ${verb} · ${state.failed.size} couldn't be removed (in use?)`
    );
  } catch (err) {
    hideScanbar();
    render();
    toast(`Clean failed: ${err}`);
  } finally {
    state.cleaning = false;
    rescanBtn.disabled = false;
    cachesBtn.disabled = false;
    updateFooter(); // re-enables Clean if any (failed) rows are still selected
  }
}

/* ───────────────────────── Navigation ────────────────────────── */
/** Switch the top-level screen from the left nav rail. */
function setView(view: "clean" | "rules") {
  state.view = view;
  viewClean.hidden = view !== "clean";
  viewRules.hidden = view !== "rules";
  document.querySelectorAll<HTMLButtonElement>(".nav__item").forEach((b) =>
    b.classList.toggle("is-active", b.dataset.view === view)
  );
  if (view === "rules") enterRulesView();
}

/* ───────────────────────── Toast ─────────────────────────────── */
let toastTimer: number | undefined;
export function toast(msg: string) {
  let el = document.querySelector<HTMLDivElement>(".toast");
  if (!el) {
    el = document.createElement("div");
    el.className = "toast";
    document.body.appendChild(el);
  }
  el.textContent = msg;
  el.classList.add("show");
  clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => el!.classList.remove("show"), 1800);
}

/* ───────────────────────── Wiring ────────────────────────────── */
function wire() {
  // window controls — only buttons that declare a window action (not the
  // settings gear / close button, which share the .wbtn style but aren't controls)
  document.querySelectorAll<HTMLButtonElement>(".wbtn[data-win]").forEach((b) =>
    b.addEventListener("click", () => windowAction(b.dataset.win as any))
  );

  // rescan + system caches + folder picker
  $("#rescan").addEventListener("click", doScan);
  $("#caches").addEventListener("click", doScanCaches);
  rootInput.addEventListener("keydown", (e) => {
    if ((e as KeyboardEvent).key === "Enter") doScan();
  });
  $(".field__icon").addEventListener("click", async () => {
    const dir = await pickFolder();
    if (dir) {
      rootInput.value = dir;
      doScan();
    }
  });

  // filter pills (scoped to the Clean view — the rules editor's tabs are .pill too)
  document.querySelectorAll<HTMLButtonElement>(".filters .pill").forEach((pill) => {
    pill.addEventListener("click", (e) => {
      if ((e.target as HTMLElement).classList.contains("pill__num")) return;
      const key = pill.dataset.filter as keyof typeof state.filters;
      state.filters[key] = !state.filters[key];
      pill.setAttribute("aria-pressed", String(state.filters[key]));
      reconcileSelection();
      render();
    });
  });
  ageInput.addEventListener("input", () => {
    const v = parseInt(ageInput.value, 10);
    state.ageDays = Number.isFinite(v) && v > 0 ? v : 14;
    if (state.filters.age) {
      reconcileSelection();
      render();
    }
  });

  cleanBtn.addEventListener("click", doClean);

  // top-level nav (left rail): Clean / Rules
  document.querySelectorAll<HTMLButtonElement>(".nav__item").forEach((b) =>
    b.addEventListener("click", () => setView(b.dataset.view as "clean" | "rules"))
  );
}

/* ───────────────────────── Boot ──────────────────────────────── */
wire();
doScan();

/* ───────────────────────── Sample data ───────────────────────── *
 * Mirrors the reference screenshot. Used only for browser preview;
 * the Tauri build replaces this with a real disk scan.              */
const GB = 1e9;
const SAMPLE: ScanResult = {
  root: "~/Projects",
  categories: [
    { id: "node", label: "Node.js", size: 2.2 * GB },
    { id: "rust", label: "Rust", size: 14 * GB },
    { id: "jvm", label: "JVM", size: 2.7 * GB },
    { id: "python", label: "Python", size: 2.4 * GB },
    { id: "php", label: "PHP", size: 0.5 * GB },
  ],
  locations: [
    loc("space-sim", "/target", "rust", 6.6),
    loc("dockoptim", "/target", "rust", 4.1),
    loc("wold", "/target", "rust", 3.0),
    loc("mam-rag", "/.venv", "python", 2.3),
    loc("FarmersDelightReforged", "/.gradle", "jvm", 1.8),
    loc("HookCatch", "/node_modules", "node", 1.1),
    loc("FarmersDelightReforged", "/build", "jvm", 0.9),
    loc("laravel-butler", "/node_modules", "node", 0.7),
    loc("laravel-butler", "/vendor", "php", 0.5),
  ],
};
function loc(
  project: string,
  artifact: string,
  category: Location["category"],
  gb: number
): Location {
  return {
    path: `~/Projects/${project}${artifact}`,
    project,
    artifact,
    category,
    size: gb * GB,
    age_secs: 20 * 86400,
    git_ignored: true,
  };
}
