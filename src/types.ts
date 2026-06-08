/** An ecosystem id from the ruleset (e.g. "rust", "node", "cpp", "gamedev", "junk"). */
export type CategoryId = string;

export interface Location {
  /** absolute path on disk */
  path: string;
  /** display project segment, e.g. "space-sim" */
  project: string;
  /** artifact segment, e.g. "/target" or "/node_modules" */
  artifact: string;
  category: CategoryId;
  /** size in bytes */
  size: number;
  /** seconds since the directory was last modified */
  age_secs: number;
  /** true when the path is ignored by git (safe to reclaim) */
  git_ignored: boolean;
}

export interface Category {
  id: CategoryId;
  label: string;
  /** total bytes across all matching locations */
  size: number;
}

export interface ScanResult {
  root: string;
  categories: Category[];
  locations: Location[];
}

/** Status of the user's rules override file, shown in the Settings panel. */
export interface RulesStatus {
  /** absolute path where the override lives (created on demand) */
  override_path: string;
  override_exists: boolean;
  /** true when the override is present and parsed successfully */
  using_override: boolean;
  /** parse/read error message when the override is present but invalid */
  error: string | null;
  rule_count: number;
  junk_count: number;
  cache_count: number;
}

/* ── Rules editor DTOs (mirror the Rust serde structs; snake_case wire) ──
 * Backend skips empty arrays / null notes when serializing, so fields can be
 * absent on load — the editor's normalize() restores them. Keys `rule`/`junk`/
 * `global_cache` match the TOML table names. */
export interface RuleDefaults {
  min_age_days: number;
  skip_git_tracked: boolean;
  respect_ignorefile: boolean;
  move_to_trash: boolean;
  global_ignore: string[];
}
export interface RuleDef {
  id: string;
  name: string;
  ecosystem: string;
  markers: string[];
  /** negative markers: suppress this rule in a dir that contains any of them */
  anti_markers: string[];
  dirs: string[];
  globs: string[];
  reclaim_root: boolean;
  enabled: boolean;
  note: string | null;
}
export interface JunkDef {
  id: string;
  name: string;
  ecosystem: string;
  dirs: string[];
  globs: string[];
  enabled: boolean;
  note: string | null;
}
export interface CacheDef {
  id: string;
  name: string;
  ecosystem: string;
  paths: string[];
  platform: string | null;
  enabled: boolean;
  note: string | null;
}
export interface RuleFile {
  schema_version: number;
  defaults: RuleDefaults;
  rule: RuleDef[];
  junk: JunkDef[];
  global_cache: CacheDef[];
}

export interface ScanOptions {
  root: string;
  /** when set, only include dirs untouched for >= this many days */
  minAgeDays: number | null;
  /** when true, drop anything git tracks (keep only git-ignored dirs) */
  skipGitTracked: boolean;
  /** when true, honour a .prunignore file at the root */
  respectPrunignore: boolean;
}

/**
 * Progress streamed from the backend `scan` command over a Tauri Channel.
 * The `kind` discriminant mirrors the Rust `ScanEvent` enum (serde tag).
 */
export type ScanEvent =
  | { kind: "discovering"; scanned: number }
  | { kind: "discovered"; total: number }
  | { kind: "located"; location: Location; done: number; total: number }
  | { kind: "done"; root: string; categories: Category[] };

/**
 * Progress streamed from the backend `clean` command over a Tauri Channel.
 * Mirrors the Rust `CleanEvent` enum. `done`/`total` drive the progress bar;
 * `done` excludes the in-flight path on `removing`, includes it on
 * `removed`/`failed`.
 */
export type CleanEvent =
  | { kind: "removing"; path: string; done: number; total: number }
  | { kind: "removed"; path: string; done: number; total: number }
  | { kind: "failed"; path: string; error: string; done: number; total: number }
  | { kind: "done"; removed: number; failed: number };

/* The ruleset spans ~26 ecosystems, so colours/labels are resolved by id rather
 * than a fixed map. Known ecosystems get a curated colour; anything else gets a
 * stable colour hashed from its id so it stays distinct and consistent. */
const ECOSYSTEM_COLORS: Record<string, string> = {
  node: "#45c75a",
  rust: "#df6a48",
  jvm: "#9b6cf2",
  python: "#4493f8",
  php: "#21bd8a",
  go: "#00add8",
  cpp: "#6f8fd6",
  dotnet: "#b07cf6",
  ruby: "#e0564b",
  dart: "#3fb7c4",
  swift: "#f0913e",
  beam: "#a64d79",
  haskell: "#8f6fbf",
  crystal: "#cfd2da",
  zig: "#f7a41d",
  nim: "#e3cf3a",
  bazel: "#43a047",
  gamedev: "#e0699f",
  infra: "#7b9acc",
  latex: "#3a9b78",
  nix: "#5277c3",
  data: "#d98c3f",
  docs: "#5fae8e",
  testing: "#cf8b4e",
  junk: "#8a8d96",
  editor: "#9aa0ad",
};

const ECOSYSTEM_LABELS: Record<string, string> = {
  rust: "Rust",
  go: "Go",
  cpp: "C/C++",
  bazel: "Bazel",
  zig: "Zig",
  nim: "Nim",
  swift: "Swift",
  dotnet: ".NET",
  jvm: "JVM",
  node: "Node.js",
  python: "Python",
  php: "PHP",
  ruby: "Ruby",
  dart: "Dart / Flutter",
  beam: "Erlang / Elixir",
  haskell: "Haskell",
  crystal: "Crystal",
  gamedev: "Game engines",
  infra: "Infra / IaC",
  latex: "LaTeX",
  nix: "Nix",
  data: "Data",
  docs: "Docs / SSG",
  testing: "Testing / E2E",
  junk: "OS / junk",
  editor: "Editor caches",
};

/** Known ecosystem ids, for the editor's ecosystem datalist (free text still allowed). */
export const KNOWN_ECOSYSTEMS: string[] = Object.keys(ECOSYSTEM_LABELS);

function hslFromString(s: string): string {
  let h = 0;
  for (let i = 0; i < s.length; i++) h = (h * 31 + s.charCodeAt(i)) >>> 0;
  return `hsl(${h % 360} 52% 60%)`;
}

/** Dot colour for an ecosystem id (curated, with a stable hashed fallback). */
export function categoryColor(id: CategoryId): string {
  return ECOSYSTEM_COLORS[id] ?? hslFromString(id);
}

/** Display label for an ecosystem id (mirrors the Rust `ecosystem_label`). */
export function categoryLabel(id: CategoryId): string {
  if (ECOSYSTEM_LABELS[id]) return ECOSYSTEM_LABELS[id];
  if (!id) return "Other";
  return id
    .split(/[-_]/)
    .filter(Boolean)
    .map((w) => w[0].toUpperCase() + w.slice(1))
    .join(" ");
}
