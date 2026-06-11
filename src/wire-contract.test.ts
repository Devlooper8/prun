/* ─────────────────── Wire-contract fixtures ───────────────────
 * The Rust DTOs are mirrored BY HAND in types.ts, so a Rust field rename
 * would normally surface as a silent `undefined` at runtime. The fixtures in
 * /fixtures close that gap as a triangle:
 *
 *   Rust tests pin   fixture == serde's actual serialization
 *   this file pins   fixture == what types.ts declares
 *   therefore        Rust wire format == TS types
 *
 * The `expected` literals are TYPED — `npm run build` (tsc) fails if a field
 * here stops matching types.ts; the deep-equals fail if a fixture drifts.
 * When the wire format changes intentionally, update the fixture and both
 * pin tests together. */
import { describe, it, expect } from "vitest";
import type { ScanEvent, CleanEvent } from "./types";
import scanEventsFixture from "../fixtures/scan-events.json";
import cleanEventsFixture from "../fixtures/clean-events.json";

const expectedScanEvents: ScanEvent[] = [
  { kind: "discovering", scanned: 480 },
  { kind: "discovered", total: 2 },
  {
    kind: "located",
    location: {
      path: "/projects/app/target",
      project: "app",
      artifact: "/target",
      category: "rust",
      size: 6_600_000_000,
      age_secs: 1_728_000,
      git_ignored: true,
    },
    done: 1,
    total: 2,
  },
  {
    kind: "done",
    root: "/projects",
    categories: [{ id: "rust", label: "Rust", size: 6_600_000_000 }],
    errors: 1,
    error_samples: ["/projects/app/target/locked.bin: access denied"],
  },
];

const expectedCleanEvents: CleanEvent[] = [
  { kind: "removing", path: "/projects/app/target", done: 0, total: 2 },
  { kind: "removed", path: "/projects/app/target", done: 1, total: 2 },
  { kind: "failed", path: "/projects/web/node_modules", error: "in use", done: 2, total: 2 },
  { kind: "done", removed: 1, failed: 1 },
];

describe("wire contract", () => {
  it("scan-event fixtures match the TS union exactly", () => {
    expect(scanEventsFixture).toEqual(expectedScanEvents);
  });

  it("clean-event fixtures match the TS union exactly", () => {
    expect(cleanEventsFixture).toEqual(expectedCleanEvents);
  });
});
