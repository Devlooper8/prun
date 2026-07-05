/* ───────────────────── Rules editor (in-app screen) ─────────────────────
 * An embedded master–detail view (not a dialog): a compact, searchable,
 * ecosystem-grouped list of entries on the left; the selected entry's form on
 * the right. Loads the active ruleset (override if present, else built-in
 * defaults), edits it in an in-memory model, and saves the COMPLETE ruleset
 * back to the override. The matcher reloads per scan, so edits apply next scan.
 *
 * Re-render discipline: text/boolean edits mutate the model and never re-render
 * (the caret is never lost); a field that affects a list row updates just that
 * row in place; only add/delete/section-change rebuild the list. ----------- */
import { toast } from "./main";
import {
  type RuleFile,
  type RuleDef,
  type JunkDef,
  type CacheDef,
  type RuleDefaults,
  categoryColor,
  categoryLabel,
  KNOWN_ECOSYSTEMS,
} from "./types";
import { IS_TAURI, loadRules, saveRules, resetRules, openRulesFile, rulesStatus } from "./backend";
import { defaultDefaults, sampleRuleFile } from "./sample-data";
import { $ } from "./dom";

const listEl = $<HTMLDivElement>("#re-list");
const detailEl = $<HTMLDivElement>("#re-detail");
const statusEl = $<HTMLDivElement>("#re-status");
const unsavedEl = $<HTMLSpanElement>("#re-unsaved");
const errorEl = $<HTMLDivElement>("#re-error");
const saveBtn = $<HTMLButtonElement>("#re-save");
const splitEl = $<HTMLDivElement>(".reditor__split");

type Section = "rule" | "junk" | "global_cache" | "defaults";
type Entry = RuleDef | JunkDef | CacheDef;

const ed = {
  model: null as RuleFile | null,
  section: "rule" as Section,
  selected: null as Entry | null,
  search: "",
  collapsed: new Set<string>(),
  dirty: false,
  saving: false,
  loaded: false,
  wired: false,
};

/* ── small DOM helpers ─────────────────────────────────────────── */
function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  cls?: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text != null) n.textContent = text;
  return n;
}

function field(label: string, control: HTMLElement): HTMLDivElement {
  const f = el("div", "re-field");
  f.appendChild(el("label", "re-label", label));
  f.appendChild(control);
  return f;
}

function textInput(
  value: string,
  placeholder: string,
  oninput: (v: string) => void,
): HTMLInputElement {
  const i = el("input", "re-input");
  i.type = "text";
  i.value = value;
  i.placeholder = placeholder;
  i.spellcheck = false;
  i.addEventListener("input", () => oninput(i.value));
  return i;
}

function checkbox(
  checked: boolean,
  label: string,
  onchange: (v: boolean) => void,
): HTMLLabelElement {
  const wrap = el("label", "re-check");
  const cb = el("input", "cb");
  cb.type = "checkbox";
  cb.checked = checked;
  cb.addEventListener("change", () => onchange(cb.checked));
  wrap.appendChild(cb);
  wrap.appendChild(el("span", undefined, label));
  return wrap;
}

function noteArea(value: string | null, onchange: (v: string | null) => void): HTMLTextAreaElement {
  const t = el("textarea", "re-input re-note");
  t.value = value ?? "";
  t.rows = 2;
  t.placeholder = "optional note";
  t.addEventListener("input", () => onchange(t.value.trim() === "" ? null : t.value));
  return t;
}

function ensureEcoDatalist() {
  if (document.getElementById("re-eco-list")) return;
  const dl = el("datalist");
  dl.id = "re-eco-list";
  for (const id of KNOWN_ECOSYSTEMS) {
    const o = el("option");
    o.value = id;
    o.textContent = categoryLabel(id);
    dl.appendChild(o);
  }
  document.body.appendChild(dl);
}

/** Chips + add-input for a string array; re-renders only its own container. */
function stringList(items: string[], placeholder: string, onChange: () => void): HTMLDivElement {
  const wrap = el("div", "re-chips");
  const paint = () => {
    wrap.innerHTML = "";
    items.forEach((item, idx) => {
      const chip = el("span", "re-chip");
      chip.appendChild(el("span", "re-chip__text", item));
      const x = el("button", "re-chip__x");
      x.type = "button";
      x.textContent = "×";
      x.title = `Remove ${item}`;
      x.addEventListener("click", () => {
        items.splice(idx, 1);
        onChange();
        paint();
      });
      chip.appendChild(x);
      wrap.appendChild(chip);
    });
    const add = el("input", "re-input re-chip-add");
    add.type = "text";
    add.placeholder = placeholder;
    add.spellcheck = false;
    const commit = () => {
      const v = add.value.trim();
      if (v && !items.includes(v)) {
        items.push(v);
        onChange();
        paint();
        wrap.querySelector<HTMLInputElement>(".re-chip-add")?.focus();
      }
    };
    add.addEventListener("keydown", (e) => {
      if ((e as KeyboardEvent).key === "Enter") {
        e.preventDefault();
        commit();
      }
    });
    add.addEventListener("blur", commit);
    wrap.appendChild(add);
  };
  paint();
  return wrap;
}

/* ── list pane ─────────────────────────────────────────────────── */
function matchEntry(e: Entry, q: string): boolean {
  return (
    (e.id || "").toLowerCase().includes(q) ||
    (e.name || "").toLowerCase().includes(q) ||
    (e.ecosystem || "").toLowerCase().includes(q)
  );
}

function renderList() {
  listEl.innerHTML = "";
  const toolbar = el("div", "re-toolbar");
  const search = el("input", "re-input re-search");
  search.type = "search";
  search.placeholder = "Search…";
  search.value = ed.search;
  search.spellcheck = false;
  search.addEventListener("input", () => {
    ed.search = search.value;
    renderGroups();
  });
  const add = el("button", "btn btn--ghost re-addbtn");
  add.type = "button";
  add.textContent = "＋ Add";
  add.addEventListener("click", onAdd);
  toolbar.appendChild(search);
  toolbar.appendChild(add);
  listEl.appendChild(toolbar);
  listEl.appendChild(el("div", "re-list__scroll"));
  renderGroups();
}

function renderGroups() {
  const scroll = listEl.querySelector<HTMLElement>(".re-list__scroll");
  if (!scroll) return;
  scroll.innerHTML = "";
  const q = ed.search.trim().toLowerCase();
  // `ed.section` is never "defaults" here (that section has no list pane)
  const entries = SECTIONS[ed.section as ListSection].list(ed.model!);
  const filtered = entries.filter((e) => !q || matchEntry(e, q));

  if (filtered.length === 0) {
    scroll.appendChild(el("div", "re-empty", q ? "No matches." : "Nothing here yet — ＋ Add one."));
    return;
  }

  const groups = new Map<string, typeof filtered>();
  for (const e of filtered) {
    const key = e.ecosystem || "(unsorted)";
    const bucket = groups.get(key);
    if (bucket) bucket.push(e);
    else groups.set(key, [e]);
  }

  for (const key of [...groups.keys()].sort()) {
    const items = groups.get(key)!;
    const gkey = `${ed.section}:${key}`;
    const collapsed = ed.collapsed.has(gkey);

    const header = el("button", "re-group");
    header.type = "button";
    header.appendChild(el("span", "re-group__caret" + (collapsed ? "" : " is-open"), "▸"));
    if (key !== "(unsorted)") {
      const dot = el("span", "dot");
      dot.style.background = categoryColor(key);
      header.appendChild(dot);
    }
    header.appendChild(
      el("span", "re-group__name", key === "(unsorted)" ? "(unsorted)" : categoryLabel(key)),
    );
    header.appendChild(el("span", "re-group__count", String(items.length)));
    header.addEventListener("click", () => {
      if (ed.collapsed.has(gkey)) ed.collapsed.delete(gkey);
      else ed.collapsed.add(gkey);
      renderGroups();
    });
    scroll.appendChild(header);

    if (!collapsed) for (const e of items) scroll.appendChild(renderRow(e));
  }
}

function renderRow(e: Entry): HTMLElement {
  const row = el("div", "re-row");
  if (e === ed.selected) row.classList.add("is-selected");
  if (e.enabled === false) row.classList.add("is-off");
  const dot = el("span", "dot");
  dot.style.background = categoryColor(e.ecosystem || "");
  row.appendChild(dot);
  row.appendChild(el("span", "re-row__name", e.name || e.id || "(unnamed)"));

  // caches have no honoured `enabled` flag → no toggle (would be a dead control)
  if (ed.section !== "global_cache") {
    const cb = el("input", "cb re-row__cb");
    cb.type = "checkbox";
    cb.checked = e.enabled !== false;
    cb.title = "Enable / disable";
    cb.addEventListener("click", (ev) => ev.stopPropagation());
    cb.addEventListener("change", () => {
      e.enabled = cb.checked;
      row.classList.toggle("is-off", !cb.checked);
      markDirty();
    });
    row.appendChild(cb);
  }

  row.addEventListener("click", () => selectEntry(e));
  return row;
}

function selectEntry(e: Entry) {
  ed.selected = e;
  renderGroups(); // re-apply the .is-selected highlight (search input untouched)
  renderDetail();
}

/** Update the selected row's label/dot/toggle in place (no list rebuild). */
function syncSelectedRow() {
  const e = ed.selected;
  const row = listEl.querySelector<HTMLElement>(".re-row.is-selected");
  if (!e || !row) return;
  const name = row.querySelector<HTMLElement>(".re-row__name");
  if (name) name.textContent = e.name || e.id || "(unnamed)";
  const dot = row.querySelector<HTMLElement>(".dot");
  if (dot) dot.style.background = categoryColor(e.ecosystem || "");
  const cb = row.querySelector<HTMLInputElement>(".re-row__cb");
  if (cb) {
    cb.checked = e.enabled !== false;
    row.classList.toggle("is-off", e.enabled === false);
  }
}

/* ── detail pane ───────────────────────────────────────────────── */
function renderDetail() {
  detailEl.innerHTML = "";
  if (ed.section === "defaults") {
    detailEl.appendChild(renderDefaultsForm(ed.model!.defaults));
    return;
  }
  if (!ed.selected) {
    detailEl.appendChild(
      el("div", "re-placeholder", "Select an entry on the left, or ＋ Add a new one."),
    );
    return;
  }
  if (ed.section === "rule") detailEl.appendChild(renderRuleForm(ed.selected as RuleDef));
  else if (ed.section === "junk") detailEl.appendChild(renderJunkForm(ed.selected as JunkDef));
  else detailEl.appendChild(renderCacheForm(ed.selected as CacheDef));
}

function formHead(titleText: string): { head: HTMLDivElement; title: HTMLSpanElement } {
  const head = el("div", "re-form__head");
  const title = el("span", "re-form__title", titleText);
  head.appendChild(title);
  const del = el("button", "re-card__del");
  del.type = "button";
  del.textContent = "Delete";
  del.addEventListener("click", deleteSelected);
  head.appendChild(del);
  return { head, title };
}

/** The id / name / ecosystem fields every entry form opens with, returned as an
 *  un-appended grid so a caller can add more (the cache form appends `platform`).
 *  `titled` refreshes the form title + list row after an id/name edit. */
function commonHeaderFields(e: Entry, titled: () => void): HTMLDivElement {
  const grid = el("div", "re-grid");
  grid.appendChild(
    field(
      "id",
      textInput(e.id, "unique-id", (v) => {
        e.id = v;
        markDirty();
        titled();
      }),
    ),
  );
  grid.appendChild(
    field(
      "name",
      textInput(e.name, "Display name", (v) => {
        e.name = v;
        markDirty();
        titled();
      }),
    ),
  );
  const eco = textInput(e.ecosystem, "ecosystem (free text)", (v) => {
    e.ecosystem = v;
    markDirty();
    syncSelectedRow();
  });
  eco.setAttribute("list", "re-eco-list"); // suggest the known ecosystems
  grid.appendChild(field("ecosystem", eco));
  return grid;
}

/** The trailing optional-note field every entry form ends with. */
function appendNote(wrap: HTMLElement, e: Entry): void {
  wrap.appendChild(
    field(
      "note",
      noteArea(e.note, (v) => {
        e.note = v;
        markDirty();
      }),
    ),
  );
}

function renderRuleForm(r: RuleDef): HTMLElement {
  const wrap = el("div", "re-form");
  const { head, title } = formHead(r.name || r.id || "(new rule)");
  wrap.appendChild(head);
  const titled = () => {
    title.textContent = r.name || r.id || "(new rule)";
    syncSelectedRow();
  };

  wrap.appendChild(commonHeaderFields(r, titled));

  wrap.appendChild(
    field("markers", stringList(r.markers, "+ marker (Cargo.toml, *.csproj…)", markDirty)),
  );
  wrap.appendChild(
    field(
      "anti-markers",
      stringList(
        r.anti_markers,
        "+ anti-marker (skip dir if present, e.g. CMakeLists.txt)",
        markDirty,
      ),
    ),
  );
  wrap.appendChild(field("dirs", stringList(r.dirs, "+ dir (target, project/target…)", markDirty)));
  wrap.appendChild(field("globs", stringList(r.globs, "+ glob (*.o, __pycache__…)", markDirty)));

  const toggles = el("div", "re-toggles");
  toggles.appendChild(
    checkbox(r.enabled, "Enabled", (v) => {
      r.enabled = v;
      markDirty();
      syncSelectedRow();
    }),
  );
  toggles.appendChild(
    checkbox(r.reclaim_root, "Reclaim root (the marker's own dir is the artifact)", (v) => {
      r.reclaim_root = v;
      markDirty();
    }),
  );
  wrap.appendChild(toggles);

  appendNote(wrap, r);
  return wrap;
}

function renderJunkForm(j: JunkDef): HTMLElement {
  const wrap = el("div", "re-form");
  const { head, title } = formHead(j.name || j.id || "(new junk)");
  wrap.appendChild(head);
  const titled = () => {
    title.textContent = j.name || j.id || "(new junk)";
    syncSelectedRow();
  };

  wrap.appendChild(commonHeaderFields(j, titled));

  wrap.appendChild(field("dirs", stringList(j.dirs, "+ dir (.ccls-cache…)", markDirty)));
  wrap.appendChild(field("globs", stringList(j.globs, "+ glob (.DS_Store, *.swp…)", markDirty)));

  const toggles = el("div", "re-toggles");
  toggles.appendChild(
    checkbox(j.enabled, "Enabled", (v) => {
      j.enabled = v;
      markDirty();
      syncSelectedRow();
    }),
  );
  wrap.appendChild(toggles);

  appendNote(wrap, j);
  return wrap;
}

function renderCacheForm(c: CacheDef): HTMLElement {
  // No `enabled` toggle: scan_caches ignores it (value still round-trips).
  const wrap = el("div", "re-form");
  const { head, title } = formHead(c.name || c.id || "(new cache)");
  wrap.appendChild(head);
  const titled = () => {
    title.textContent = c.name || c.id || "(new cache)";
    syncSelectedRow();
  };

  const grid = commonHeaderFields(c, titled);
  grid.appendChild(
    field(
      "platform",
      textInput(c.platform ?? "", "all (or macos/windows/linux)", (v) => {
        c.platform = v.trim() === "" ? null : v.trim();
        markDirty();
      }),
    ),
  );
  wrap.appendChild(grid);

  wrap.appendChild(
    field("paths", stringList(c.paths, "+ path (~/.cargo/registry/cache…)", markDirty)),
  );
  appendNote(wrap, c);
  return wrap;
}

function renderDefaultsForm(d: RuleDefaults): HTMLElement {
  const wrap = el("div", "re-form");
  wrap.appendChild(el("div", "re-form__title", "Defaults"));

  const minAge = textInput(String(d.min_age_days), "14", (v) => {
    const n = parseInt(v, 10);
    d.min_age_days = Number.isFinite(n) && n >= 0 ? n : 0;
    markDirty();
  });
  minAge.inputMode = "numeric";
  wrap.appendChild(field("min_age_days", minAge));

  const toggles = el("div", "re-toggles");
  toggles.appendChild(
    checkbox(d.skip_git_tracked, "skip_git_tracked", (v) => {
      d.skip_git_tracked = v;
      markDirty();
    }),
  );
  toggles.appendChild(
    checkbox(d.respect_ignorefile, "respect_ignorefile", (v) => {
      d.respect_ignorefile = v;
      markDirty();
    }),
  );
  toggles.appendChild(
    checkbox(d.move_to_trash, "move_to_trash", (v) => {
      d.move_to_trash = v;
      markDirty();
    }),
  );
  wrap.appendChild(toggles);

  wrap.appendChild(
    field("global_ignore", stringList(d.global_ignore, "+ dir to never enter (.git…)", markDirty)),
  );
  wrap.appendChild(
    el(
      "p",
      "re-hint",
      "min_age_days / skip_git_tracked / respect_ignorefile are driven per-scan by the filter pills; only global_ignore always applies.",
    ),
  );
  return wrap;
}

/* ── add / delete ──────────────────────────────────────────────── */
const blankRule = (): RuleDef => ({
  id: "",
  name: "",
  ecosystem: "",
  markers: [],
  anti_markers: [],
  dirs: [],
  globs: [],
  reclaim_root: false,
  enabled: true,
  note: null,
});
const blankJunk = (): JunkDef => ({
  id: "",
  name: "",
  ecosystem: "junk",
  dirs: [],
  globs: [],
  enabled: true,
  note: null,
});
const blankCache = (): CacheDef => ({
  id: "",
  name: "",
  ecosystem: "",
  paths: [],
  platform: null,
  enabled: false,
  note: null,
});

type ListSection = Exclude<Section, "defaults">;

/** The three editable list sections: where each entry array lives in the model and
 *  how to mint a blank entry. (`defaults` is a singleton form, not a list, so the
 *  three callers below only ever run for a list section.) */
const SECTIONS: Record<ListSection, { list: (m: RuleFile) => Entry[]; blank: () => Entry }> = {
  rule: { list: (m) => m.rule, blank: blankRule },
  junk: { list: (m) => m.junk, blank: blankJunk },
  global_cache: { list: (m) => m.global_cache, blank: blankCache },
};

function onAdd() {
  const sec = SECTIONS[ed.section as ListSection];
  const e = sec.blank();
  sec.list(ed.model!).unshift(e);
  ed.selected = e;
  markDirty();
  renderGroups();
  renderDetail();
  detailEl.querySelector<HTMLInputElement>(".re-input")?.focus();
}

function deleteSelected() {
  const e = ed.selected;
  if (!e) return;
  const list = SECTIONS[ed.section as ListSection].list(ed.model!);
  const i = list.indexOf(e);
  if (i >= 0) list.splice(i, 1);
  ed.selected = null;
  markDirty();
  renderGroups();
  renderDetail();
}

/* ── load / normalize / dirty / status ─────────────────────────── */
function normalize(rf: RuleFile): RuleFile {
  rf.rule = (rf.rule ?? []).map((r) => ({
    ...r,
    markers: r.markers ?? [],
    anti_markers: r.anti_markers ?? [],
    dirs: r.dirs ?? [],
    globs: r.globs ?? [],
    reclaim_root: r.reclaim_root ?? false,
    enabled: r.enabled ?? true,
    note: r.note ?? null,
  }));
  rf.junk = (rf.junk ?? []).map((j) => ({
    ...j,
    dirs: j.dirs ?? [],
    globs: j.globs ?? [],
    enabled: j.enabled ?? true,
    note: j.note ?? null,
  }));
  rf.global_cache = (rf.global_cache ?? []).map((c) => ({
    ...c,
    paths: c.paths ?? [],
    platform: c.platform ?? null,
    enabled: c.enabled ?? false,
    note: c.note ?? null,
  }));
  rf.defaults = rf.defaults ?? defaultDefaults();
  rf.defaults.global_ignore = rf.defaults.global_ignore ?? [];
  rf.schema_version = rf.schema_version ?? 3;
  return rf;
}

async function loadModel() {
  try {
    const model = IS_TAURI ? await loadRules() : sampleRuleFile();
    ed.model = normalize(model);
    ed.loaded = true;
    ed.selected = null;
    setDirty(false);
    errorEl.hidden = true;
  } catch (err) {
    errorEl.textContent = String(err);
    errorEl.hidden = false;
    errorEl.scrollIntoView({ block: "nearest" });
  }
}

function setDirty(v: boolean) {
  ed.dirty = v;
  unsavedEl.hidden = !v;
}
function markDirty() {
  if (!ed.dirty) setDirty(true);
}

async function renderStatus() {
  if (!IS_TAURI) {
    statusEl.textContent =
      "Preview mode — the desktop app saves to your override at %APPDATA%\\prun\\rules.toml";
    return;
  }
  try {
    const s = await rulesStatus();
    const where = `Saving to ${s.override_path}`;
    if (s.error)
      statusEl.textContent = `${where} · ⚠ your override has an error — showing defaults`;
    else if (s.using_override) statusEl.textContent = `${where} · using your override`;
    else statusEl.textContent = `${where} · using built-in defaults (no override yet)`;
  } catch (err) {
    // Leave the last status visible rather than silently blanking it, and say
    // why — a wrong "using defaults" claim would be worse than an error line.
    statusEl.textContent = `⚠ couldn't read the rules status: ${err}`;
  }
}

/* ── section + actions ─────────────────────────────────────────── */
function renderSectionView() {
  document
    .querySelectorAll<HTMLButtonElement>("#view-rules .reditor__tab")
    .forEach((t) => t.classList.toggle("is-active", t.dataset.section === ed.section));
  if (ed.section === "defaults") {
    splitEl.classList.add("reditor__split--single");
    listEl.innerHTML = "";
    renderDetail();
    return;
  }
  splitEl.classList.remove("reditor__split--single");
  renderList();
  renderDetail();
}

async function save() {
  if (!ed.model || ed.saving) return;
  errorEl.hidden = true;
  if (!IS_TAURI) {
    toast("Saving is available in the desktop app");
    return;
  }
  ed.saving = true;
  saveBtn.disabled = true;
  try {
    await saveRules(ed.model);
    setDirty(false);
    renderStatus();
    toast("Saved — applies on your next scan");
  } catch (err) {
    errorEl.textContent = String(err);
    errorEl.hidden = false;
    errorEl.scrollIntoView({ block: "nearest" });
  } finally {
    ed.saving = false;
    saveBtn.disabled = false;
  }
}

async function reset() {
  if (!confirm("Reset to the built-in defaults? This deletes your override file.")) return;
  errorEl.hidden = true;
  if (IS_TAURI) {
    try {
      await resetRules();
    } catch (err) {
      errorEl.textContent = String(err);
      errorEl.hidden = false;
      return;
    }
  }
  await loadModel();
  ed.section = "rule";
  ed.search = "";
  renderStatus();
  renderSectionView();
  toast("Reset to built-in defaults");
}

async function openFile() {
  if (!IS_TAURI) {
    toast("Available in the desktop app");
    return;
  }
  try {
    await openRulesFile();
    toast("Opened the rules file in your editor");
    renderStatus();
  } catch (err) {
    toast(`Couldn't open the file: ${err}`);
  }
}

function wireOnce() {
  if (ed.wired) return;
  ed.wired = true;
  document.querySelectorAll<HTMLButtonElement>("#view-rules .reditor__tab").forEach((t) => {
    t.addEventListener("click", () => {
      ed.section = t.dataset.section as Section;
      ed.selected = null;
      ed.search = "";
      renderSectionView();
    });
  });
  saveBtn.addEventListener("click", save);
  $("#re-reset").addEventListener("click", reset);
  $("#re-openfile").addEventListener("click", openFile);
}

/** Called by the nav when switching to the Rules view. Reloads from disk unless
 *  there are unsaved edits (so switching Clean↔Rules preserves work). */
export async function enterRulesView() {
  ensureEcoDatalist();
  wireOnce();
  if (!ed.loaded || !ed.dirty) await loadModel();
  renderStatus();
  renderSectionView();
}
