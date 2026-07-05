/* Pure location-grouping / filtering / roll-up logic, extracted from main.ts so it
 * carries no DOM or global-state dependency and can be unit-tested directly. Every
 * function here takes its inputs as arguments and returns plain data. */
import { type Location, type Category, type CategoryId, categoryLabel } from "./types";

/** "scan" groups by project folder; "caches" groups by the cache's display name. */
export type ScanMode = "scan" | "caches";

export interface ProjectGroup {
  /** top-level folder under the scan root (or the cache name in caches mode) */
  name: string;
  locations: Location[];
  size: number;
}

/** Path with the scan root stripped and leading separators removed. */
export function relUnderRoot(path: string, root: string): string {
  const rel = path.startsWith(root) ? path.slice(root.length) : path;
  return rel.replace(/^[\\/]+/, "");
}

/** Grouping key: the project = first folder under the scan root.
 *  e.g. <root>/prun/src-tauri/target  →  "prun" (not "src-tauri"). */
export function projectKeyOf(path: string, root: string): string {
  const rel = relUnderRoot(path, root);
  return rel.split(/[\\/]/)[0] || rel || path;
}

/** The artifact's location within its project, e.g. "src-tauri/target". */
export function subPathOf(path: string, root: string): string {
  const parts = relUnderRoot(path, root).split(/[\\/]/);
  return parts.slice(1).join("/") || parts[0] || path;
}

/** Distinct categories present in a set of locations, biggest-footprint first. */
export function distinctCategories(locs: Location[]): CategoryId[] {
  const size = new Map<CategoryId, number>();
  for (const l of locs) size.set(l.category, (size.get(l.category) ?? 0) + l.size);
  return [...size].sort((a, b) => b[1] - a[1]).map(([c]) => c);
}

/** Build the category roll-up from locations — live, as a scan streams. */
export function rollupCategories(locations: Location[]): Category[] {
  const totals = new Map<CategoryId, number>();
  for (const l of locations) totals.set(l.category, (totals.get(l.category) ?? 0) + l.size);
  return [...totals]
    .map(([id, size]) => ({ id, label: categoryLabel(id), size }))
    .sort((a, b) => b.size - a.size);
}

/** Grouping key: the project folder for a normal scan, or the cache name in the
 *  caches view (cache paths are absolute, so the project segment is the meaningful
 *  label there). */
function groupKey(loc: Location, root: string, mode: ScanMode): string {
  if (mode === "caches") return loc.project || loc.category;
  return projectKeyOf(loc.path, root);
}

/** Roll the (already filtered) locations up into project groups, biggest first. */
export function groupByProject(
  locations: Location[],
  root: string,
  mode: ScanMode,
): ProjectGroup[] {
  const groups = new Map<string, Location[]>();
  for (const loc of locations) {
    const key = groupKey(loc, root, mode);
    const bucket = groups.get(key);
    if (bucket) bucket.push(loc);
    else groups.set(key, [loc]);
  }
  return [...groups]
    .map(([name, locs]) => ({
      name,
      locations: locs.sort((a, b) => b.size - a.size),
      size: locs.reduce((s, l) => s + l.size, 0),
    }))
    .sort((a, b) => b.size - a.size);
}

/** The active filter state, passed in so the predicate stays pure. */
export interface LocationFilter {
  /** enabled category ids; an empty set means "all categories on" */
  catsOn: Set<string>;
  ageFilter: boolean;
  ageDays: number;
  gitFilter: boolean;
  /** when set, drop anything smaller than `sizeBytes` (focus on the big wins) */
  sizeFilter?: boolean;
  sizeBytes?: number;
}

/** Apply the category / age / git / size filters and sort biggest-first. */
export function filterLocations(locations: Location[], f: LocationFilter): Location[] {
  return locations
    .filter((loc) => {
      if (f.catsOn.size && !f.catsOn.has(loc.category)) return false;
      if (f.ageFilter && loc.age_secs < f.ageDays * 86400) return false;
      if (f.gitFilter && !loc.git_ignored) return false;
      if (f.sizeFilter && loc.size < (f.sizeBytes ?? 0)) return false;
      return true;
    })
    .sort((a, b) => b.size - a.size);
}
