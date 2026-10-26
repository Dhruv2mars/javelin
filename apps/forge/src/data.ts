// Page data for the forge's read-only pages. Each loader reads the node directly over
// protocol v2 and returns plain data; rendering lives in `app.ts`.
import {
  type FileEntry,
  type Hex,
  Kind,
  NodeClient,
  ProtocolError,
  type Publication,
  type Resolved,
  type TreeItem,
  entryBlob,
  listDirectory,
  lookup,
} from "../../../packages/protocol/src";
import { type Hunk, diffLines } from "./diff";
import { relativeTargets } from "./markdown";

export { NodeClient, ProtocolError };
export type { Publication, Resolved, TreeItem };

/** Text files larger than this are shown as too large rather than rendered. */
export const MAX_TEXT = 1 << 20;
const README = /^readme(\.(md|markdown|txt))?$/i;

export type Contents =
  | { kind: "text"; text: string; size: number }
  | { kind: "binary"; size: number }
  | { kind: "large"; size: number }
  | { kind: "symlink"; target: string };

/** A file's bytes as display contents. Binary means a NUL byte or invalid UTF-8. */
export function contents(entry: FileEntry, bytes: Uint8Array): Contents {
  if (entry.kind === "symlink") return { kind: "symlink", target: new TextDecoder().decode(bytes) };
  if (bytes.length > MAX_TEXT) return { kind: "large", size: bytes.length };
  if (bytes.subarray(0, 8000).includes(0)) return { kind: "binary", size: bytes.length };
  try {
    return { kind: "text", text: new TextDecoder("utf-8", { fatal: true }).decode(bytes), size: bytes.length };
  } catch {
    return { kind: "binary", size: bytes.length };
  }
}

/** Directories first, then files, each in name order. */
function sorted(items: TreeItem[]): TreeItem[] {
  return [...items].sort((a, b) => (a.type === b.type ? 0 : a.type === "dir" ? -1 : 1));
}

export type DirectoryPage = {
  version: Resolved;
  head: number;
  publication: Publication | null;
  path: string;
  entries: TreeItem[];
  /** `links` gives the entry kind of each relative link target in the README that exists. */
  readme: { name: string; text: string; links: Record<string, "dir" | "file"> } | null;
};

/** Most README links whose targets are looked up. */
const MAX_README_LINKS = 64;

/**
 * Version `version` (default the head), refused unless its ID hashes from its parent's ID,
 * its root and its changeset, the same check the history and version pages make. Code pages
 * show files from that root, so a substituted tree is refused rather than shown.
 */
export async function verifiedRoot(client: NodeClient, version: number | null) {
  const head = await client.resolve();
  const number = version ?? head.number;
  const parent = number > 0 ? await client.resolve(number - 1) : null;
  const { resolved, linked } = await client.verifiedVersion(number, parent);
  if (!linked) {
    throw new ProtocolError(
      "chain_mismatch",
      `World ${number}'s ID does not hash from its parent, root and changeset`,
    );
  }
  return { at: resolved, head: Math.max(head.number, resolved.number) };
}

/** Repository home or a directory: its entries at a version (default the head) and its README. */
export async function loadDirectory(
  client: NodeClient,
  version: number | null = null,
  path = "",
): Promise<DirectoryPage> {
  const { at, head } = await verifiedRoot(client, version);
  const found = await lookup(client, at.tree, path);
  if (found?.type !== "dir") throw new ProtocolError("not_found", `no directory ${path || "/"} in World ${at.number}`);
  const entries = sorted(await listDirectory(client, found.id));
  const readmeItem = entries.find((e) => e.type === "file" && README.test(e.name));
  let readme: DirectoryPage["readme"] = null;
  if (readmeItem?.type === "file") {
    const shown = contents(readmeItem.entry, await client.blob(entryBlob(readmeItem.entry)));
    if (shown.kind === "text") {
      const targets = relativeTargets(shown.text, path).slice(0, MAX_README_LINKS);
      const kinds = await Promise.all(targets.map((t) => lookup(client, at.tree, t).catch(() => null)));
      const links: Record<string, "dir" | "file"> = {};
      targets.forEach((t, i) => {
        if (kinds[i]) links[t] = kinds[i]!.type;
      });
      readme = { name: readmeItem.name, text: shown.text, links };
    }
  }
  return {
    version: at,
    head,
    publication: await client.attribution(at.number),
    path,
    entries,
    readme,
  };
}

export type FilePage = {
  version: Resolved;
  head: number;
  path: string;
  entry: FileEntry;
  blob: Hex;
  contents: Contents;
};

/** One file at a version. */
export async function loadFile(client: NodeClient, version: number, path: string): Promise<FilePage> {
  const { at, head } = await verifiedRoot(client, version);
  const found = await lookup(client, at.tree, path);
  if (found?.type !== "file") throw new ProtocolError("not_found", `no file ${path} in World ${at.number}`);
  const blob = entryBlob(found.entry);
  const shown = contents(found.entry, await client.blob(blob));
  return { version: at, head, path, entry: found.entry, blob, contents: shown };
}

export type HistoryRow = {
  number: number;
  id: Hex;
  tree: Hex;
  changed: number;
  publication: Publication | null;
  /** The ID hashes from its parent's ID, its root and its changeset. */
  linked: boolean;
};

export type HistoryPage = { head: number; rows: HistoryRow[]; next: number | null };

/** World versions newest first, `limit` per page, starting at `from` (default the head). */
export async function loadHistory(client: NodeClient, from: number | null = null, limit = 25): Promise<HistoryPage> {
  const head = await client.resolve();
  const top = Math.min(from ?? head.number, head.number);
  const bottom = Math.max(0, top - limit + 1);
  const numbers = Array.from({ length: top - bottom + 1 }, (_, i) => top - i);
  const parents = new Map<number, Resolved>();
  await Promise.all(
    [...numbers, bottom - 1].filter((n) => n >= 0).map(async (n) => parents.set(n, await client.resolve(n))),
  );
  const rows = await Promise.all(
    numbers.map(async (number): Promise<HistoryRow> => {
      const [{ resolved, changeset, linked }, publication] = await Promise.all([
        client.verifiedVersion(number, parents.get(number - 1) ?? null),
        client.attribution(number),
      ]);
      return { number, id: resolved.id, tree: resolved.tree, changed: changeset.size, publication, linked };
    }),
  );
  return { head: head.number, rows, next: bottom > 0 ? bottom - 1 : null };
}

export type Status = "added" | "deleted" | "modified" | "mode";

export type FileChange = {
  path: string;
  status: Status;
  before: FileEntry | null;
  after: FileEntry | null;
  /** Line hunks when both sides are text; otherwise `note` says why there is no diff. */
  hunks: Hunk[] | null;
  note: string | null;
  added: number;
  removed: number;
};

export type VersionPage = {
  version: Resolved;
  parent: Resolved | null;
  head: number;
  publication: Publication | null;
  linked: boolean;
  changes: FileChange[];
};

/** The changeset of `version` as a diff against its parent. */
export async function loadVersion(client: NodeClient, version: number): Promise<VersionPage> {
  const head = await client.resolve();
  const parent = version > 0 ? await client.resolve(version - 1) : null;
  const [{ resolved, changeset, linked }, publication] = await Promise.all([
    client.verifiedVersion(version, parent),
    client.attribution(version),
  ]);
  const paths = [...changeset.keys()];
  const befores = await Promise.all(
    paths.map(async (path) => {
      if (!parent) return null;
      const found = await lookup(client, parent.tree, path);
      return found?.type === "file" ? found.entry : null;
    }),
  );
  const blobs = new Set<Hex>();
  paths.forEach((path, i) => {
    const after = changeset.get(path);
    if (after) blobs.add(entryBlob(after));
    if (befores[i]) blobs.add(entryBlob(befores[i]!));
  });
  const bytes = blobs.size ? await client.objects(Kind.Blob, [...blobs]) : new Map<Hex, Uint8Array>();
  const changes = paths.map((path, i) => change(path, befores[i], changeset.get(path) ?? null, bytes));
  return { version: resolved, parent, head: head.number, publication, linked, changes };
}

function change(
  path: string,
  before: FileEntry | null,
  after: FileEntry | null,
  bytes: Map<Hex, Uint8Array>,
): FileChange {
  const status: Status = !before ? "added" : !after ? "deleted" : sameContent(before, after) ? "mode" : "modified";
  const side = (entry: FileEntry | null) => (entry ? contents(entry, bytes.get(entryBlob(entry))!) : null);
  const [old, now] = [side(before), side(after)];
  const text = (c: Contents | null) => (c === null ? "" : c.kind === "text" ? c.text : c.kind === "symlink" ? c.target : null);
  const [a, b] = [text(old), text(now)];
  let note: string | null = null;
  let hunks: Hunk[] | null = null;
  if (status === "mode") note = modeNote(before!, after!);
  else if (a === null || b === null) note = [old, now].some((c) => c?.kind === "large") ? "File too large to diff" : "Binary file changed";
  else {
    hunks = diffLines(a, b);
    if (!hunks) note = "Diff too large to show";
  }
  const count = (kind: "add" | "del") =>
    (hunks ?? []).reduce((n, h) => n + h.lines.filter((l) => l.kind === kind).length, 0);
  return { path, status, before, after, hunks, note, added: count("add"), removed: count("del") };
}

function sameContent(a: FileEntry, b: FileEntry) {
  return a.kind === b.kind && entryBlob(a) === entryBlob(b);
}

function modeNote(before: FileEntry, after: FileEntry) {
  const mode = (e: FileEntry) => (e.kind === "symlink" ? "symlink" : e.executable ? "executable" : "not executable");
  return `Mode changed from ${mode(before)} to ${mode(after)}`;
}
