/* Pure, DOM-free formatting helpers. Kept separate from main.ts so they can be
 * unit-tested without a browser environment. */

/** Human-readable byte size, e.g. 1500000 → "2 MB", 6.6e9 → "6.6 GB". */
export function fmtSize(bytes: number): string {
  if (bytes >= 1e9) return `${(bytes / 1e9).toFixed(1)} GB`;
  if (bytes >= 1e6) return `${(bytes / 1e6).toFixed(0)} MB`;
  if (bytes >= 1e3) return `${(bytes / 1e3).toFixed(0)} KB`;
  return `${bytes} B`;
}

/** Post-clean toast text. Honest about disk: permanent delete truly frees the
 *  space, but "Move to Trash" only relocates it — the bytes still occupy the disk
 *  until the Trash/Recycle Bin is emptied, so don't claim them as reclaimed. */
export function cleanSummary(
  reclaimedBytes: number,
  removed: number,
  toTrash: boolean,
  failed: number,
): string {
  const locs = `${removed} location${removed === 1 ? "" : "s"}`;
  let head: string;
  if (removed === 0) head = "Nothing removed";
  else if (toTrash)
    head = `Moved ${fmtSize(reclaimedBytes)} to Trash · ${locs} — empty Trash to reclaim`;
  else head = `Reclaimed ${fmtSize(reclaimedBytes)} · ${locs} deleted`;
  if (failed > 0) head += ` · ${failed} couldn't be removed (in use?)`;
  return head;
}

const escMap: Record<string, string> = {
  "&": "&amp;",
  "<": "&lt;",
  ">": "&gt;",
  '"': "&quot;",
};

/** Escape a string for safe interpolation into innerHTML. */
export const esc = (s: string) => s.replace(/[&<>"]/g, (c) => escMap[c]);

/** Last two segments of a path, e.g. "space-sim/target" — the meaningful tail. */
export function shortPath(path: string): string {
  const parts = path.split(/[\\/]+/).filter(Boolean);
  return parts.slice(-2).join("/") || path;
}

/** Cap a string at `max` characters, marking the cut with an ellipsis. */
export function truncate(s: string, max: number): string {
  return s.length <= max ? s : `${s.slice(0, Math.max(0, max - 1))}…`;
}
