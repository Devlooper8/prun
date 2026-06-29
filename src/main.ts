import "./styles.css";
import { type ScanResult, type ScanOptions, type Location, categoryColor } from "./types";
import { fmtSize, esc, shortPath, truncate } from "./format";
import {
  type ProjectGroup,
  subPathOf,
  distinctCategories,
  rollupCategories,
  groupByProject,
  filterLocations,
} from "./grouping";
import {
  type ScanHandlers,
  runScan,
  runScanCaches,
  runClean,
  cancelScan,
  pickFolder,
  windowAction,
  openLogsDir,
} from "./backend";
import { enterRulesView } from "./rules-editor";

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
const scanCancel = $<HTMLButtonElement>("#scan-cancel");
const viewClean = $<HTMLElement>("#view-clean");
const viewRules = $<HTMLElement>("#view-rules");

/* ───────────────────────── Helpers ───────────────────────────── */
/** Run `fn` only after `ms` of silence — keeps rapid input (typing in the age
 *  field) from re-filtering and re-rendering the whole list per keystroke. */
function debounce(fn: () => void, ms: number): () => void {
  let timer: number | undefined;
  return () => {
    clearTimeout(timer);
    timer = window.setTimeout(fn, ms);
  };
}

/** Toast text after a scan: the count, plus an unreadable-items note when the
 *  backend couldn't fully read some paths (so a wrong total isn't shown as final).
 *  The first concrete example is appended so "unreadable" is actionable; the full
 *  sample list goes to the console and the backend log. */
function scanSummary(
  count: number,
  errors: number,
  noun = "location",
  errorSamples: string[] = [],
): string {
  const base = `Found ${count} ${noun}${count === 1 ? "" : "s"}`;
  if (errors === 0) return base;
  const example = errorSamples.length > 0 ? ` — e.g. ${truncate(errorSamples[0], 80)}` : "";
  return `${base} · ${errors} item${errors === 1 ? "" : "s"} unreadable${example}`;
}

/** Locations passing the current category/age/git filters (biggest first). Thin
 *  wrapper that feeds the global filter state into the pure `filterLocations`. */
function visibleLocations(): Location[] {
  if (!state.result) return [];
  return filterLocations(state.result.locations, {
    catsOn: state.catsOn,
    ageFilter: state.filters.age,
    ageDays: state.ageDays,
    gitFilter: state.filters.git,
  });
}

/* ── progress strip ─────────────────────────────────────────────── *
 * Shows only the directory being scanned, with a classic blue bar under it:
 * an indeterminate marquee while discovering (total unknown), then a
 * determinate fill while sizing. No per-file paths or counts. */
function showScanbar(rootLabel: string) {
  scanbar.hidden = false;
  scanRoot.textContent = rootLabel;
  scanDiscovering(); // indeterminate marquee + cleared pct/fill
  scanCancel.hidden = false; // scans are cancellable
  scanCancel.disabled = false;
  scanCancel.textContent = "Cancel";
}
function hideScanbar() {
  scanbar.hidden = true;
  scanbar.classList.remove("scanbar--indeterminate");
  scanFill.style.width = "0%";
  scanPct.textContent = "";
  scanCancel.hidden = true;
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
  scanCancel.hidden = true; // a clean isn't cancelled from here (it streams per-path)
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
    if (state.result) state.result.categories = rollupCategories(state.result.locations);
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
      if (state.catsOn.size === 0) res.categories.forEach((c) => state.catsOn.add(c.id));
      if (cb.checked) state.catsOn.add(cat.id);
      else state.catsOn.delete(cat.id);
      if (state.catsOn.size === res.categories.length) state.catsOn.clear();
      reconcileSelection();
      render();
    });
    catsList.appendChild(li);
  }

  // locations — grouped by project (top-level folder under the scan root)
  const groups = groupByProject(visibleLocations(), res.root, state.mode);
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

  for (const loc of g.locations) childrenUl.appendChild(renderChild(loc, g, groupCb, root));

  const toggleExpand = () => {
    const open = childrenUl.hasAttribute("hidden");
    childrenUl.toggleAttribute("hidden", !open);
    arrow.classList.toggle("is-open", open);
    arrow.setAttribute("aria-expanded", String(open));
    if (open) state.expanded.add(g.name);
    else state.expanded.delete(g.name);
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
    for (const loc of g.locations) {
      if (on) state.selected.add(loc.path);
      else state.selected.delete(loc.path);
    }
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
  root: string,
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
    if (cb.checked) state.selected.add(loc.path);
    else state.selected.delete(loc.path);
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
/** Shared driver for both scans: reset state, stream into a live result, then
 *  (project scan only) auto-select everything and toast a summary. `runner` opens
 *  the matching backend stream with the shared handler set. */
async function runScanInto(
  cfg: {
    mode: "scan" | "caches";
    rootLabel: string;
    noun: string; // summary toast: "location" | "system cache"
    failVerb: string; // failure toast / warn: "Scan" | "Cache scan"
    autoSelectAll: boolean;
  },
  runner: (handlers: ScanHandlers) => Promise<void>,
) {
  if (state.scanning || state.cleaning) return; // ignore overlapping scans/cleans
  // Reset to an empty live result the stream will fill in.
  state.scanning = true;
  state.mode = cfg.mode;
  rescanBtn.disabled = true;
  cachesBtn.disabled = true;
  state.result = { root: cfg.rootLabel, categories: [], locations: [] };
  state.selected.clear();
  state.catsOn.clear();
  state.expanded.clear();
  state.failed.clear();
  let maxDone = 0;
  let errors = 0;
  let errorSamples: string[] = [];

  showScanbar(cfg.rootLabel);
  render();

  try {
    await runner({
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
      onDone(root, categories, e, samples) {
        state.result!.root = root;
        state.result!.categories = categories;
        errors = e;
        errorSamples = samples;
      },
    });

    if (cfg.autoSelectAll) state.selected = new Set(visibleLocations().map((l) => l.path));
    hideScanbar();
    render();
    if (errorSamples.length > 0)
      console.warn(`unreadable during ${cfg.failVerb.toLowerCase()}:`, errorSamples);
    toast(scanSummary(state.result.locations.length, errors, cfg.noun, errorSamples));
  } catch (err) {
    hideScanbar();
    render();
    toast(`${cfg.failVerb} failed: ${err}`);
  } finally {
    state.scanning = false;
    rescanBtn.disabled = false;
    cachesBtn.disabled = false;
  }
}

function doScan() {
  const opts: ScanOptions = {
    root: rootInput.value.trim() || "~/Projects",
    minAgeDays: state.filters.age ? state.ageDays : null,
    skipGitTracked: state.filters.git,
    respectPrunignore: state.filters.prunignore,
  };
  return runScanInto(
    { mode: "scan", rootLabel: opts.root, noun: "location", failVerb: "Scan", autoSelectAll: true },
    (handlers) => runScan(opts, handlers),
  );
}

/** Scan the per-user system caches. A separate view: never auto-selected, since
 *  these are shared across projects and slow to rebuild. */
function doScanCaches() {
  return runScanInto(
    {
      mode: "caches",
      rootLabel: "System caches",
      noun: "system cache",
      failVerb: "Cache scan",
      autoSelectAll: false,
    },
    (handlers) => runScanCaches(handlers),
  );
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
    });

    hideScanbar();
    res.categories = rollupCategories(res.locations); // drop emptied categories
    render();
    const removed = paths.length - state.failed.size;
    toast(
      state.failed.size === 0
        ? `${removed} location${removed === 1 ? "" : "s"} ${verb}`
        : `${removed} ${verb} · ${state.failed.size} couldn't be removed (in use?)`,
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
  document
    .querySelectorAll<HTMLButtonElement>(".nav__item")
    .forEach((b) => b.classList.toggle("is-active", b.dataset.view === view));
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
  // window controls — only buttons that declare a window action (not buttons
  // that merely share the .wbtn style). The [data-win] selector guarantees the
  // dataset value exists; the HTML is the source of the three action names.
  document
    .querySelectorAll<HTMLButtonElement>(".wbtn[data-win]")
    .forEach((b) =>
      b.addEventListener("click", () =>
        windowAction(b.dataset.win as "minimize" | "maximize" | "close"),
      ),
    );

  // rescan + system caches + folder picker
  $("#rescan").addEventListener("click", doScan);
  $("#caches").addEventListener("click", doScanCaches);

  // cancel the in-flight scan (button lives in the progress strip)
  scanCancel.addEventListener("click", () => {
    scanCancel.disabled = true;
    scanCancel.textContent = "Cancelling…";
    cancelScan();
  });
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
  // Re-filtering is debounced: parsing is instant, but the full reconcile +
  // re-render waits for a typing pause (matters on large result sets).
  const applyAgeFilter = debounce(() => {
    if (state.filters.age) {
      reconcileSelection();
      render();
    }
  }, 200);
  ageInput.addEventListener("input", () => {
    const v = parseInt(ageInput.value, 10);
    state.ageDays = Number.isFinite(v) && v > 0 ? v : 14;
    applyAgeFilter();
  });

  cleanBtn.addEventListener("click", doClean);

  // top-level nav (left rail): Clean / Rules. Scoped to [data-view] — the rail
  // also holds action buttons (Logs) that share the style but switch no view.
  document
    .querySelectorAll<HTMLButtonElement>(".nav__item[data-view]")
    .forEach((b) =>
      b.addEventListener("click", () => setView(b.dataset.view as "clean" | "rules")),
    );

  // open the log / crash-report folder (null = browser preview, no backend)
  $("#open-logs").addEventListener("click", () => {
    openLogsDir()
      .then((dir) => {
        if (dir === null) toast("Logs are available in the desktop app");
      })
      .catch((err) => toast(`Couldn't open logs: ${err}`));
  });
}

/* ───────────────────────── Boot ──────────────────────────────── */
wire();
doScan();
