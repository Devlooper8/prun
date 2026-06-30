import { describe, it, expect } from "vitest";
import {
  relUnderRoot,
  projectKeyOf,
  subPathOf,
  distinctCategories,
  rollupCategories,
  groupByProject,
  filterLocations,
} from "./grouping";
import type { Location } from "./types";

const loc = (
  path: string,
  category: string,
  size: number,
  extra: Partial<Location> = {},
): Location => ({
  path,
  project: "",
  artifact: "",
  category,
  size,
  age_secs: 0,
  git_ignored: true,
  ...extra,
});

describe("path helpers", () => {
  it("relUnderRoot strips the root and leading separators", () => {
    expect(relUnderRoot("D:\\Projects\\prun\\src-tauri\\target", "D:\\Projects")).toBe(
      "prun\\src-tauri\\target",
    );
    expect(relUnderRoot("/home/me/app/target", "/home/me")).toBe("app/target");
  });

  it("projectKeyOf returns the first folder under the root (not the marker dir)", () => {
    // the documented case: …/prun/src-tauri/target groups under "prun", not "src-tauri"
    expect(projectKeyOf("D:\\Projects\\prun\\src-tauri\\target", "D:\\Projects")).toBe("prun");
    expect(projectKeyOf("/home/me/app/node_modules", "/home/me")).toBe("app");
  });

  it("subPathOf returns the artifact location within its project", () => {
    expect(subPathOf("D:\\Projects\\prun\\src-tauri\\target", "D:\\Projects")).toBe(
      "src-tauri/target",
    );
  });
});

describe("distinctCategories", () => {
  it("orders categories by total footprint, biggest first", () => {
    const locs = [loc("/a", "node", 1), loc("/b", "rust", 10), loc("/c", "node", 5)];
    expect(distinctCategories(locs)).toEqual(["rust", "node"]); // rust 10 > node 6
  });
});

describe("rollupCategories", () => {
  it("sums per category, sorts desc, and resolves labels", () => {
    const locs = [loc("/a", "rust", 3), loc("/b", "node", 5), loc("/c", "rust", 4)];
    const cats = rollupCategories(locs);
    expect(cats.map((c) => c.id)).toEqual(["rust", "node"]); // rust 7 > node 5
    expect(cats[0]).toMatchObject({ id: "rust", label: "Rust", size: 7 });
    expect(cats.find((c) => c.id === "node")?.label).toBe("Node.js");
  });
});

describe("groupByProject", () => {
  const root = "/home/me";
  const locs = [
    loc("/home/me/app/node_modules", "node", 2),
    loc("/home/me/app/dist", "node", 1),
    loc("/home/me/lib/target", "rust", 10),
  ];

  it("groups by first folder under root in scan mode, biggest group first", () => {
    const groups = groupByProject(locs, root, "scan");
    expect(groups.map((g) => g.name)).toEqual(["lib", "app"]); // lib 10 > app 3
    expect(groups[1].size).toBe(3);
    expect(groups[1].locations).toHaveLength(2);
  });

  it("groups by cache name in caches mode", () => {
    const caches = [
      loc("/abs/one", "rust", 5, { project: "Cargo cache" }),
      loc("/abs/two", "node", 2, { project: "npm cache" }),
    ];
    const groups = groupByProject(caches, "", "caches");
    expect(groups.map((g) => g.name)).toEqual(["Cargo cache", "npm cache"]);
  });
});

describe("filterLocations", () => {
  const locs = [
    loc("/a", "rust", 9, { age_secs: 30 * 86400, git_ignored: true }),
    loc("/b", "node", 5, { age_secs: 1 * 86400, git_ignored: true }),
    loc("/c", "rust", 7, { age_secs: 60 * 86400, git_ignored: false }),
  ];

  it("an empty category set means all categories pass", () => {
    expect(
      filterLocations(locs, { catsOn: new Set(), ageFilter: false, ageDays: 14, gitFilter: false }),
    ).toHaveLength(3);
  });

  it("filters by enabled category", () => {
    const out = filterLocations(locs, {
      catsOn: new Set(["rust"]),
      ageFilter: false,
      ageDays: 14,
      gitFilter: false,
    });
    expect(out.map((l) => l.path)).toEqual(["/a", "/c"]); // sorted biggest-first: 9, 7
  });

  it("age filter drops anything younger than the cutoff", () => {
    const out = filterLocations(locs, {
      catsOn: new Set(),
      ageFilter: true,
      ageDays: 14,
      gitFilter: false,
    });
    expect(out.map((l) => l.path)).toEqual(["/a", "/c"]); // /b is 1 day old
  });

  it("git filter keeps only git-ignored paths", () => {
    const out = filterLocations(locs, {
      catsOn: new Set(),
      ageFilter: false,
      ageDays: 14,
      gitFilter: true,
    });
    expect(out.map((l) => l.path)).toEqual(["/a", "/b"]); // /c is tracked
  });

  it("size filter drops anything below the byte threshold", () => {
    const out = filterLocations(locs, {
      catsOn: new Set(),
      ageFilter: false,
      ageDays: 14,
      gitFilter: false,
      sizeFilter: true,
      sizeBytes: 7,
    });
    expect(out.map((l) => l.path)).toEqual(["/a", "/c"]); // /b is 5, below 7
  });
});
