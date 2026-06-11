/* ───────────────────────── Sample data ─────────────────────────
 * Used only by the browser preview (no Tauri shell): the scan/caches
 * simulators in backend.ts replay it, and the rules editor previews a tiny
 * ruleset. The desktop build never reads any of this.
 * ---------------------------------------------------------------- */
import { type ScanResult, type Location, type RuleFile, type RuleDefaults } from "./types";

const GB = 1e9;

function loc(
  project: string,
  artifact: string,
  category: Location["category"],
  gb: number,
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

/** Mirrors the reference screenshot. */
export const SAMPLE: ScanResult = {
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

/** Fake per-user system caches for the caches-view preview. */
export const SAMPLE_CACHES: Location[] = [
  {
    path: "~/.cargo/registry/cache",
    project: "Cargo registry & git cache",
    artifact: "/cache",
    category: "rust",
    size: 3.4 * GB,
    age_secs: 90 * 86400,
    git_ignored: true,
  },
  {
    path: "~/.gradle/caches",
    project: "Gradle cache",
    artifact: "/caches",
    category: "jvm",
    size: 2.1 * GB,
    age_secs: 45 * 86400,
    git_ignored: true,
  },
  {
    path: "~/.npm/_cacache",
    project: "npm cache",
    artifact: "/_cacache",
    category: "node",
    size: 1.2 * GB,
    age_secs: 60 * 86400,
    git_ignored: true,
  },
];

/** The editor's fallback defaults — also what normalize() fills in when a loaded
 *  ruleset omits the [defaults] table. */
export function defaultDefaults(): RuleDefaults {
  return {
    min_age_days: 14,
    skip_git_tracked: true,
    respect_ignorefile: true,
    move_to_trash: true,
    global_ignore: [".git", ".hg", ".svn", ".jj"],
  };
}

/** A tiny but representative ruleset for the editor's browser preview. */
export function sampleRuleFile(): RuleFile {
  return {
    schema_version: 3,
    defaults: defaultDefaults(),
    rule: [
      {
        id: "rust-cargo",
        name: "Rust (Cargo)",
        ecosystem: "rust",
        markers: ["Cargo.toml"],
        anti_markers: [],
        dirs: ["target"],
        globs: [],
        reclaim_root: false,
        enabled: true,
        note: null,
      },
      {
        id: "node-modules",
        name: "Node.js (dependencies)",
        ecosystem: "node",
        markers: ["package.json"],
        anti_markers: [],
        dirs: ["node_modules"],
        globs: [],
        reclaim_root: false,
        enabled: true,
        note: null,
      },
      {
        id: "vite",
        name: "Vite",
        ecosystem: "node",
        markers: ["vite.config.ts"],
        anti_markers: [],
        dirs: ["dist", ".vite"],
        globs: [],
        reclaim_root: false,
        enabled: true,
        note: null,
      },
      {
        id: "python-venv",
        name: "Python virtualenv",
        ecosystem: "python",
        markers: ["pyvenv.cfg"],
        anti_markers: [],
        dirs: [],
        globs: [],
        reclaim_root: true,
        enabled: true,
        note: "Any dir containing pyvenv.cfg.",
      },
    ],
    junk: [
      {
        id: "os-cruft",
        name: "OS metadata",
        ecosystem: "junk",
        dirs: [],
        globs: [".DS_Store", "Thumbs.db"],
        enabled: true,
        note: null,
      },
    ],
    global_cache: [
      {
        id: "cargo",
        name: "Cargo registry & git cache",
        ecosystem: "rust",
        paths: ["~/.cargo/registry/cache"],
        platform: null,
        enabled: false,
        note: null,
      },
    ],
  };
}
