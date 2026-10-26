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

/** Text files hold no NUL (a NUL makes a file binary), so it marks a missing final newline. */
const NO_EOL = "\0";

/** Lines without their newlines. A last line without a final newline keeps a marker, so
 *  adding or removing that newline is a change. */
export function splitLines(text: string): string[] {
  if (text === "") return [];
  const lines = text.split("\n");
  if (lines[lines.length - 1] === "") lines.pop();
  else lines[lines.length - 1] += NO_EOL;
  return lines;
}

function line(kind: Line["kind"], raw: string, old: number | null, now: number | null): Line {
  const eof = raw.endsWith(NO_EOL);
  return { kind, text: eof ? raw.slice(0, -1) : raw, old, new: now, ...(eof ? { eof } : {}) };
}

/** The shortest edit script from `a` to `b`, or null when it exceeds the search bounds. */
export function diffScript(a: string[], b: string[]): Line[] | null {
  let start = 0;
  while (start < a.length && start < b.length && a[start] === b[start]) start++;
  let endA = a.length;
  let endB = b.length;
  while (endA > start && endB > start && a[endA - 1] === b[endB - 1]) {
    endA--;
    endB--;
  }
  const head = a.slice(0, start).map((t, i) => line("ctx", t, i + 1, i + 1));
  const tail = a.slice(endA).map((t, i) => line("ctx", t, endA + i + 1, endB + i + 1));
  const middle = search(a.slice(start, endA), b.slice(start, endB), start);
  return middle && [...head, ...middle, ...tail];
}

/** Myers over the differing middle. `base` is the number of lines before it on both sides. */
function search(a: string[], b: string[], base: number): Line[] | null {
  const n = a.length;
  const m = b.length;
  if (n === 0) return b.map((t, j) => line("add", t, null, base + j + 1));
  if (m === 0) return a.map((t, i) => line("del", t, base + i + 1, null));
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
      while (x < n && y < m && a[x] === b[y]) {
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
      out.push(line("ctx", a[x - 1], base + x, base + y));
      x--;
      y--;
    }
    if (d > 0) {
      if (down) out.push(line("add", b[prevY], null, base + prevY + 1));
      else out.push(line("del", a[prevX], base + prevX + 1, null));
    }
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
