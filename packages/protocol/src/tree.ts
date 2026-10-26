// Root-down tree reads: one directory's entries, and the entry at a path. A directory is a
// prolly tree of chunks; each level's chunks are fetched in one request.
import { type Hex, Kind, ProtocolError } from "./codec";
import type { NodeClient } from "./client";
import { type FileEntry, type TreeItem, decodeChunk } from "./objects";

/** Highest chunk level the native decoder accepts. */
const MAX_LEVEL = 32;

/** Every entry of the directory whose root chunk is `root`, in name order. */
export async function listDirectory(client: NodeClient, root: Hex): Promise<TreeItem[]> {
  let level = [root];
  for (let depth = 0; depth <= MAX_LEVEL; depth++) {
    const fetched = await client.objects(Kind.Tree, level);
    const chunks = level.map((id) => decodeChunk(fetched.get(id)!));
    if (chunks.every((c) => c.level === 0)) return chunks.flatMap((c) => ("items" in c ? c.items : []));
    if (chunks.some((c) => c.level === 0)) throw new ProtocolError("malformed", "tree chunks mix levels");
    level = chunks.flatMap((c) => ("children" in c ? c.children.map((child) => child.id) : []));
  }
  throw new ProtocolError("limit_exceeded", "tree is deeper than its level limit");
}

export type Found = { type: "file"; entry: FileEntry } | { type: "dir"; id: Hex };

/** The entry at `path` under the root `tree`, or null. The empty path is the root directory. */
export async function lookup(client: NodeClient, tree: Hex, path: string): Promise<Found | null> {
  let found: Found = { type: "dir", id: tree };
  for (const name of path.split("/").filter(Boolean)) {
    if (found.type !== "dir") return null;
    const item: TreeItem | undefined = (await listDirectory(client, found.id)).find((i) => i.name === name);
    if (!item) return null;
    found = item.type === "dir" ? { type: "dir", id: item.id } : { type: "file", entry: item.entry };
  }
  return found;
}
