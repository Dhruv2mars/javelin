// Content addressing and the derived object formats a reader needs: object IDs, version
// IDs, tree chunks (`src/tree.rs`) and canonical changesets (`SparseChanges`).
import { type Hex, Kind, ProtocolError, fromHex, toHex } from "./codec";

const OBJECT_DOMAIN = new TextEncoder().encode("javelin-object-v1\0");
const VERSION_DOMAIN = new TextEncoder().encode("javelin-version-v2\0");

async function sha256(...parts: Uint8Array[]): Promise<Hex> {
  const size = parts.reduce((n, p) => n + p.length, 0);
  const all = new Uint8Array(size);
  let at = 0;
  for (const part of parts) {
    all.set(part, at);
    at += part.length;
  }
  return toHex(new Uint8Array(await crypto.subtle.digest("SHA-256", all)));
}

/** The ID of `bytes` as an object of `kind`: SHA-256 over a domain, the kind byte and the bytes. */
export function objectId(kind: Kind, bytes: Uint8Array): Promise<Hex> {
  return sha256(OBJECT_DOMAIN, Uint8Array.of(kind), bytes);
}

/** A World version's ID: `H(domain || parent || root || canonical changeset)`, zero parent for World 0. */
export function versionId(parent: Hex | null, tree: Hex, changeset: Uint8Array): Promise<Hex> {
  return sha256(VERSION_DOMAIN, parent ? fromHex(parent) : new Uint8Array(32), fromHex(tree), changeset);
}

export type FileEntry =
  | { kind: "regular"; content: Hex; executable: boolean }
  | { kind: "symlink"; target: Hex };

export type TreeItem =
  | { name: string; type: "file"; entry: FileEntry }
  | { name: string; type: "dir"; id: Hex };

/** One decoded tree chunk. Above level 0, `children` lists child chunks by their last name. */
export type Chunk =
  | { level: 0; items: TreeItem[] }
  | { level: number; children: { last: string; id: Hex }[] };

const strict = new TextDecoder("utf-8", { fatal: true });

/** Decodes a tree chunk: `level u8`, then items of `tag u8, name u16, name, id[32]`, names ascending. */
export function decodeChunk(bytes: Uint8Array): Chunk {
  const bad = (detail: string) => new ProtocolError("malformed", `tree chunk: ${detail}`);
  if (bytes.length === 0) throw bad("empty");
  const level = bytes[0];
  let at = 1;
  const items: TreeItem[] = [];
  const children: { last: string; id: Hex }[] = [];
  let previous: string | null = null;
  while (at < bytes.length) {
    if (at + 3 > bytes.length) throw bad("truncated item");
    const tag = bytes[at];
    const len = (bytes[at + 1] << 8) | bytes[at + 2];
    at += 3;
    if (at + len + 32 > bytes.length) throw bad("truncated item");
    let name: string;
    try {
      name = strict.decode(bytes.subarray(at, at + len));
    } catch {
      throw bad("name is not UTF-8");
    }
    const id = toHex(bytes.subarray(at + len, at + len + 32));
    at += len + 32;
    if (previous !== null && !(compareBytes(previous, name) < 0)) throw bad("names not ascending");
    previous = name;
    if (level > 0) {
      if (tag !== 0) throw bad("child item with a leaf tag");
      children.push({ last: name, id });
      continue;
    }
    switch (tag) {
      case 1:
      case 2:
        items.push({ name, type: "file", entry: { kind: "regular", content: id, executable: tag === 2 } });
        break;
      case 3:
        items.push({ name, type: "file", entry: { kind: "symlink", target: id } });
        break;
      case 4:
        items.push({ name, type: "dir", id });
        break;
      default:
        throw bad("unknown item tag");
    }
  }
  return level === 0 ? { level: 0, items } : { level, children };
}

const encoder = new TextEncoder();

/** Byte-wise comparison of UTF-8 names, the order Rust's `str` uses. */
export function compareBytes(a: string, b: string): number {
  const x = encoder.encode(a);
  const y = encoder.encode(b);
  for (let i = 0; i < Math.min(x.length, y.length); i++) if (x[i] !== y[i]) return x[i] - y[i];
  return x.length - y.length;
}

/** A changeset: each changed path with its new entry, or null for a deletion. */
export type Changeset = Map<string, FileEntry | null>;

/** Parses canonical changeset JSON. Paths come back in the node's order. */
export function parseChangeset(bytes: Uint8Array): Changeset {
  let value: unknown;
  try {
    value = JSON.parse(strict.decode(bytes));
  } catch {
    throw new ProtocolError("malformed", "changeset is not JSON");
  }
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new ProtocolError("malformed", "changeset is not an object");
  }
  const out: Changeset = new Map();
  for (const [path, entry] of Object.entries(value as Record<string, unknown>)) {
    out.set(path, entry === null ? null : fileEntry(entry));
  }
  return out;
}

function fileEntry(value: unknown): FileEntry {
  const e = value as Record<string, unknown>;
  const hex = (v: unknown) => typeof v === "string" && /^[0-9a-f]{64}$/.test(v);
  if (e?.kind === "regular" && hex(e.content) && typeof e.executable === "boolean") {
    return { kind: "regular", content: e.content as Hex, executable: e.executable };
  }
  if (e?.kind === "symlink" && hex(e.target)) return { kind: "symlink", target: e.target as Hex };
  throw new ProtocolError("malformed", "changeset entry is not a file entry");
}

/** The blob a file entry names. */
export function entryBlob(entry: FileEntry): Hex {
  return entry.kind === "regular" ? entry.content : entry.target;
}
