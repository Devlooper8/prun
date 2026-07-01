import { describe, it, expect } from "vitest";
import { fmtSize, fmtAge, cleanSummary, esc, shortPath, truncate } from "./format";

describe("fmtSize", () => {
  it("scales bytes through B / KB / MB / GB", () => {
    expect(fmtSize(512)).toBe("512 B");
    expect(fmtSize(1500)).toBe("2 KB");
    expect(fmtSize(1_500_000)).toBe("2 MB");
    expect(fmtSize(6.6e9)).toBe("6.6 GB");
  });
});

describe("fmtAge", () => {
  it("scales seconds through today / days / months / years", () => {
    expect(fmtAge(300)).toBe("today");
    expect(fmtAge(5 * 86400)).toBe("5d ago");
    expect(fmtAge(60 * 86400)).toBe("2mo ago");
    expect(fmtAge(400 * 86400)).toBe("1y ago");
  });
});

describe("cleanSummary", () => {
  it("permanent delete claims the space as reclaimed", () => {
    expect(cleanSummary(4.2e9, 12, false, 0)).toBe("Reclaimed 4.2 GB · 12 artifacts deleted");
  });
  it("trash is honest that disk isn't freed until the bin is emptied", () => {
    expect(cleanSummary(4.2e9, 12, true, 0)).toBe(
      "Moved 4.2 GB to Trash · 12 artifacts — empty Trash to reclaim",
    );
  });
  it("singularizes one artifact and appends a failure note", () => {
    expect(cleanSummary(1e6, 1, false, 2)).toBe(
      "Reclaimed 1 MB · 1 artifact deleted · 2 couldn't be removed (in use?)",
    );
  });
  it("reports nothing removed when every path failed", () => {
    expect(cleanSummary(0, 0, true, 3)).toBe("Nothing removed · 3 couldn't be removed (in use?)");
  });
});

describe("esc", () => {
  it("neutralizes the HTML metacharacters that enable XSS", () => {
    expect(esc("<img src=x onerror=alert(1)>")).toBe("&lt;img src=x onerror=alert(1)&gt;");
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

describe("truncate", () => {
  it("leaves short strings alone and caps long ones with an ellipsis", () => {
    expect(truncate("short", 10)).toBe("short");
    expect(truncate("exactly-10", 10)).toBe("exactly-10");
    expect(truncate("a-much-longer-string", 10)).toBe("a-much-lo…");
    expect(truncate("a-much-longer-string", 10).length).toBe(10);
  });
});
