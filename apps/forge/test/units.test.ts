// Pure units: the diff's bounds and final newlines, routes, README links, bounded replies.
import { expect, test } from "bun:test";
import { readBounded } from "../../../packages/protocol/src";
import { MAX_EDITS, diffLines } from "../src/diff";
import { relativeTargets, renderMarkdown, resolveRelative } from "../src/markdown";
import { findTarget, parseRoute } from "../src/routes";

const lines = (n: number, f: (i: number) => string) => Array.from({ length: n }, (_, i) => `${f(i)}\n`).join("");

test("a large added file diffs without a search, and a large rewrite is too large", () => {
  const big = lines(100_000, (i) => `line ${i}`);
  const added = diffLines("", big)!;
  expect(added).toHaveLength(1);
  expect(added[0].lines).toHaveLength(100_000);
  expect(added[0].lines.every((l) => l.kind === "add")).toBe(true);
  expect(diffLines(big, "")![0].lines.every((l) => l.kind === "del")).toBe(true);

  // One edit in a 100,000-line file: the common prefix and suffix are trimmed first.
  const edited = big.replace("line 50000\n", "line fifty thousand\n");
  const hunks = diffLines(big, edited)!;
  expect(hunks).toHaveLength(1);
  expect(hunks[0].oldStart).toBe(49_998);
  expect(hunks[0].lines.map((l) => l.kind).join(" ")).toBe("ctx ctx ctx del add ctx ctx ctx");

  // Every line differs: more edits than the search allows.
  const rewritten = lines(100_000, (i) => `other ${i}`);
  const started = performance.now();
  expect(diffLines(big, rewritten)).toBeNull();
  expect(performance.now() - started).toBeLessThan(5_000);
  // Just under the edit bound still diffs.
  const n = MAX_EDITS / 2 - 1;
  expect(diffLines(lines(n, (i) => `a${i}`), lines(n, (i) => `b${i}`))).not.toBeNull();
});

test("adding or removing the final newline is a change", () => {
  const added = diffLines("a\nb", "a\nb\n")!;
  expect(added).toHaveLength(1);
  expect(added[0].lines.map((l) => [l.kind, l.text, l.eof ?? false])).toEqual([
    ["ctx", "a", false],
    ["del", "b", true],
    ["add", "b", false],
  ]);
  expect(diffLines("a\n", "a")![0].lines.map((l) => [l.kind, l.eof ?? false])).toEqual([
    ["del", false],
    ["add", true],
  ]);
  expect(diffLines("a\nb", "a\nb")).toEqual([]);
});

test("malformed percent escapes are a missing page", () => {
  expect(parseRoute("/owner/repo/file/1/%ZZ")).toEqual({ page: "missing", path: "/owner/repo/file/1/%ZZ" });
  expect(parseRoute("/owner/repo/file/1/a%20b.md")).toMatchObject({ page: "file", path: "a b.md" });
});

test("storage that throws never stops the page, and a chosen node stays in memory", () => {
  const broken = {
    getItem() {
      throw new Error("SecurityError");
    },
    setItem() {
      throw new Error("QuotaExceededError");
    },
  } as unknown as Storage;
  const empty = { repositories: {} };
  const chosen = { node: "http://127.0.0.1:7878", repository: "repo_a" };
  expect(findTarget("o/r", "?node=http://127.0.0.1:7878&repository=repo_a", empty, broken)).toEqual(chosen);
  expect(findTarget("o/r", "", empty, broken)).toEqual(chosen);
  expect(findTarget("o/other", "", empty, broken)).toBeNull();
});

test("README links decode once and name their targets", () => {
  expect(resolveRelative("my%20file.md", "docs")).toBe("docs/my file.md");
  expect(resolveRelative("../src/", "docs")).toBe("src");
  expect(resolveRelative("https://example.com")).toBeNull();
  expect(resolveRelative("javascript:alert(1)")).toBeNull();
  const source = "[a](my%20file.md) and [b](src/) and ![c](img.png) and [d](https://x.test)";
  expect(relativeTargets(source)).toEqual(["my file.md", "src", "img.png"]);
  const html = renderMarkdown(source, (p) => `/x/${p.split("/").map(encodeURIComponent).join("/")}`);
  expect(html).toContain('href="/x/my%20file.md"');
  expect(html).not.toContain("%2520");
});

test("a reply past the frame limit is refused while it streams", async () => {
  const declared = new Response(new Uint8Array(10), { headers: { "content-length": "10" } });
  await expect(readBounded(declared, 4)).rejects.toMatchObject({ code: "limit_exceeded" });
  let pulled = 0;
  const endless = new ReadableStream<Uint8Array>({
    pull(controller) {
      pulled++;
      controller.enqueue(new Uint8Array(1024));
    },
  });
  await expect(readBounded(new Response(endless), 64 * 1024)).rejects.toMatchObject({ code: "limit_exceeded" });
  expect(pulled).toBeLessThan(100);
  expect(await readBounded(new Response(Uint8Array.of(1, 2, 3)), 3)).toEqual(Uint8Array.of(1, 2, 3));
});
