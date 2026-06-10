import { describe, it, expect } from "vitest";
import { fmtSize, esc, shortPath } from "./format";

describe("fmtSize", () => {
  it("scales bytes through B / KB / MB / GB", () => {
    expect(fmtSize(512)).toBe("512 B");
    expect(fmtSize(1500)).toBe("2 KB");
    expect(fmtSize(1_500_000)).toBe("2 MB");
    expect(fmtSize(6.6e9)).toBe("6.6 GB");
  });
});

describe("esc", () => {
  it("neutralizes the HTML metacharacters that enable XSS", () => {
    expect(esc("<img src=x onerror=alert(1)>")).toBe(
      "&lt;img src=x onerror=alert(1)&gt;"
    );
    expect(esc('a & b "c"')).toBe("a &amp; b &quot;c&quot;");
  });
  it("leaves plain text untouched", () => {
    expect(esc("Node.js")).toBe("Node.js");
  });
});

describe("shortPath", () => {
  it("keeps the last two segments across separator styles", () => {
    expect(shortPath("/home/me/projects/space-sim/target")).toBe("space-sim/target");
    expect(shortPath("C:\\Users\\me\\app\\node_modules")).toBe("app/node_modules");
  });
});
