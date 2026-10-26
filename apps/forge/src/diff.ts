// Line diff (Myers, O((N+M)·D)) grouped into hunks with three lines of context. Common
// prefix and suffix are trimmed first, an empty side needs no search, and the search stops
// at a work bound, so a large rewrite yields "too large" instead of exhausting memory.

/** `eof` marks a last line that has no final newline. */
export type Line = { kind: "ctx" | "add" | "del"; text: string; old: number | null; new: number | null; eof?: boolean };
export type Hunk = { oldStart: number; newStart: number; lines: Line[] };

const CONTEXT = 3;
/** Most edits the search explores: its trace holds about D² numbers. */
export const MAX_EDITS = 4000;
/** Most diagonal steps the search may take, about (N+M)·D. */
export const MAX_WORK = 50_000_000;

/** A text's lines without their newlines, and whether its last line lacks one. */
export type Split = { lines: string[]; noEol: boolean };

export function splitLines(text: string): Split {
  if (text === "") return { lines: [], noEol: false };
  const lines = text.split("\n");
  const noEol = lines[lines.length - 1] !== "";
  if (!noEol) lines.pop();
  return { lines, noEol };
}

/**
 * One side of a diff. Two lines are equal when their text is equal and either both or
 * neither is a last line without a newline, so adding or removing the final newline is a
 * change. The flag travels beside the text, never inside it.
 */
type Side = { lines: string[]; noEolAt: number };

const side = (s: Split): Side => ({ lines: s.lines, noEolAt: s.noEol ? s.lines.length - 1 : -1 });

function line(kind: Line["kind"], text: string, old: number | null, now: number | null, eof: boolean): Line {
  return { kind, text, old, new: now, ...(eof ? { eof } : {}) };
}

/** The shortest edit script from `a` to `b`, or null when it exceeds the search bounds. */
export function diffScript(before: Split, after: Split): Line[] | null {
  const a = side(before);
  const b = side(after);
  const same = (i: number, j: number) => a.lines[i] === b.lines[j] && (i === a.noEolAt) === (j === b.noEolAt);
  const ctx = (i: number, j: number) => line("ctx", a.lines[i], i + 1, j + 1, i === a.noEolAt);
  let start = 0;
  while (start < a.lines.length && start < b.lines.length && same(start, start)) start++;
  let endA = a.lines.length;
  let endB = b.lines.length;
  while (endA > start && endB > start && same(endA - 1, endB - 1)) {
    endA--;
    endB--;
  }
  const head = Array.from({ length: start }, (_, i) => ctx(i, i));
  const tail = Array.from({ length: a.lines.length - endA }, (_, i) => ctx(endA + i, endB + i));
  const middle = search(a, b, same, start, endA, endB);
  return middle && [...head, ...middle, ...tail];
}

/** Myers over `a[base..endA]` and `b[base..endB]`, the differing middle. */
function search(
  a: Side,
  b: Side,
  same: (i: number, j: number) => boolean,
  base: number,
  endA: number,
  endB: number,
): Line[] | null {
  const n = endA - base;
  const m = endB - base;
  const add = (j: number) => line("add", b.lines[base + j], null, base + j + 1, base + j === b.noEolAt);
  const del = (i: number) => line("del", a.lines[base + i], base + i + 1, null, base + i === a.noEolAt);
  if (n === 0) return Array.from({ length: m }, (_, j) => add(j));
  if (m === 0) return Array.from({ length: n }, (_, i) => del(i));
  const offset = n + m + 1;
  const v = new Int32Array(2 * offset + 1);
  // trace[d] holds v[-d..d] as it was before step d, the only range step d reads.
  const trace: Int32Array[] = [];
  let work = 0;
  let done = false;
  for (let d = 0; d <= n + m && !done; d++) {
    if (d > MAX_EDITS) return null;
    trace.push(v.slice(offset - d, offset + d + 1));
    for (let k = -d; k <= d; k += 2) {
      let x = k === -d || (k !== d && v[offset + k - 1] < v[offset + k + 1]) ? v[offset + k + 1] : v[offset + k - 1] + 1;
      let y = x - k;
      while (x < n && y < m && same(base + x, base + y)) {
        x++;
        y++;
        work++;
      }
      if (++work > MAX_WORK) return null;
      v[offset + k] = x;
      if (x >= n && y >= m) {
        done = true;
        break;
      }
    }
  }
  const out: Line[] = [];
  let x = n;
  let y = m;
  for (let d = trace.length - 1; d >= 0; d--) {
    const at = (k: number) => trace[d][k + d];
    const k = x - y;
    let prevX = 0;
    let prevY = 0;
    let down = false;
    if (d > 0) {
      down = k === -d || (k !== d && at(k - 1) < at(k + 1));
      const prevK = down ? k + 1 : k - 1;
      prevX = at(prevK);
      prevY = prevX - prevK;
    }
    while (x > prevX && y > prevY) {
      const i = base + x - 1;
      out.push(line("ctx", a.lines[i], i + 1, base + y, i === a.noEolAt));
      x--;
      y--;
    }
    if (d > 0) out.push(down ? add(prevY) : del(prevX));
    x = prevX;
    y = prevY;
  }
  return out.reverse();
}

/** Changed regions of `a` → `b`, each with up to three lines of context, or null when the
 *  diff is too large to compute. */
export function diffLines(a: string, b: string): Hunk[] | null {
  const script = diffScript(splitLines(a), splitLines(b));
  if (!script) return null;
  const ranges: [number, number][] = [];
  script.forEach((l, i) => {
    if (l.kind === "ctx") return;
    const [start, end] = [Math.max(0, i - CONTEXT), Math.min(script.length - 1, i + CONTEXT)];
    const last = ranges[ranges.length - 1];
    if (last && start <= last[1] + 1) last[1] = end;
    else ranges.push([start, end]);
  });
  return ranges.map(([start, end]) => {
    const lines = script.slice(start, end + 1);
    const firstOld = lines.find((l) => l.old !== null)?.old ?? before(script, start, "old");
    const firstNew = lines.find((l) => l.new !== null)?.new ?? before(script, start, "new");
    return { oldStart: firstOld, newStart: firstNew, lines };
  });
}

/** The line number before index `i` on one side, for a hunk with no lines on that side. */
function before(script: Line[], i: number, side: "old" | "new") {
  for (let j = i - 1; j >= 0; j--) if (script[j][side] !== null) return script[j][side]!;
  return 0;
}
