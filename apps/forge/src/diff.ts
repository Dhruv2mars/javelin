// Line diff (Myers, O((N+M)·D)) grouped into hunks with three lines of context.

export type Line = { kind: "ctx" | "add" | "del"; text: string; old: number | null; new: number | null };
export type Hunk = { oldStart: number; newStart: number; lines: Line[] };

const CONTEXT = 3;

export function splitLines(text: string): string[] {
  if (text === "") return [];
  const lines = text.split("\n");
  if (lines[lines.length - 1] === "") lines.pop();
  return lines;
}

/** The shortest edit script from `a` to `b`, as one list of lines. */
export function diffScript(a: string[], b: string[]): Line[] {
  const n = a.length;
  const m = b.length;
  const offset = n + m + 1;
  const v = new Int32Array(2 * offset + 1);
  const trace: Int32Array[] = [];
  search: for (let d = 0; d <= n + m; d++) {
    trace.push(v.slice());
    for (let k = -d; k <= d; k += 2) {
      let x = k === -d || (k !== d && v[offset + k - 1] < v[offset + k + 1]) ? v[offset + k + 1] : v[offset + k - 1] + 1;
      let y = x - k;
      while (x < n && y < m && a[x] === b[y]) {
        x++;
        y++;
      }
      v[offset + k] = x;
      if (x >= n && y >= m) break search;
    }
  }
  const out: Line[] = [];
  let x = n;
  let y = m;
  for (let d = trace.length - 1; d >= 0; d--) {
    const vd = trace[d];
    const k = x - y;
    const prevK = k === -d || (k !== d && vd[offset + k - 1] < vd[offset + k + 1]) ? k + 1 : k - 1;
    const prevX = vd[offset + prevK];
    const prevY = prevX - prevK;
    while (x > prevX && y > prevY) {
      out.push({ kind: "ctx", text: a[x - 1], old: x, new: y });
      x--;
      y--;
    }
    if (d > 0) {
      if (x === prevX) out.push({ kind: "add", text: b[prevY], old: null, new: prevY + 1 });
      else out.push({ kind: "del", text: a[prevX], old: prevX + 1, new: null });
    }
    x = prevX;
    y = prevY;
  }
  return out.reverse();
}

/** Changed regions of `a` → `b`, each with up to three lines of context around it. */
export function diffLines(a: string, b: string): Hunk[] {
  const script = diffScript(splitLines(a), splitLines(b));
  const ranges: [number, number][] = [];
  script.forEach((line, i) => {
    if (line.kind === "ctx") return;
    const [start, end] = [Math.max(0, i - CONTEXT), Math.min(script.length - 1, i + CONTEXT)];
    const last = ranges[ranges.length - 1];
    if (last && start <= last[1] + 1) last[1] = end;
    else ranges.push([start, end]);
  });
  return ranges.map(([start, end]) => {
    const lines = script.slice(start, end + 1);
    // A hunk's start is the line before its first line on a side with none (unified diff rule).
    const firstOld = lines.find((l) => l.old !== null)?.old ?? oldBefore(script, start);
    const firstNew = lines.find((l) => l.new !== null)?.new ?? newBefore(script, start);
    return { oldStart: firstOld, newStart: firstNew, lines };
  });
}

function oldBefore(script: Line[], i: number) {
  for (let j = i - 1; j >= 0; j--) if (script[j].old !== null) return script[j].old!;
  return 0;
}

function newBefore(script: Line[], i: number) {
  for (let j = i - 1; j >= 0; j--) if (script[j].new !== null) return script[j].new!;
  return 0;
}
