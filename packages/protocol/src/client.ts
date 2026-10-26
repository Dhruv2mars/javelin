// A read-only node client: one protocol v2 message per `POST {node}/v2/r/{repository}`.
// Every object is checked against its ID before it is returned, so a reader trusts the
// node for availability, never for content.
import { type Hex, Kind, type Message, type Publication, ProtocolError, decode, encode, limits } from "./codec";
import { type Changeset, type Chunk, decodeChunk, objectId, parseChangeset, versionId } from "./objects";

export type Resolved = { number: number; id: Hex; tree: Hex };

/** Largest changeset a reader will buffer. */
export const MAX_READ_CHANGESET = 64 << 20;

export class NodeClient {
  readonly url: string;

  constructor(
    node: string,
    readonly repository: string,
    // Wrapped, so a browser's `fetch` is never called with this client as its receiver.
    private fetcher: (url: string, init: RequestInit) => Promise<Response> = (url, init) => fetch(url, init),
  ) {
    this.url = `${node.replace(/\/+$/, "")}/v2/r/${encodeURIComponent(repository)}`;
  }

  /** Sends one message. A node `Error` reply throws its code; an unreachable node is `node_unavailable`. */
  async call(message: Message): Promise<Message> {
    let response: Response;
    try {
      response = await this.fetcher(this.url, {
        method: "POST",
        headers: { "content-type": "application/octet-stream" },
        body: encode(message),
      });
    } catch (error) {
      throw new ProtocolError("node_unavailable", `could not reach ${this.url}: ${String(error)}`);
    }
    if (!response.ok) {
      throw new ProtocolError("node_unavailable", `${this.url} answered HTTP ${response.status}`);
    }
    const reply = decode(await readBounded(response, limits.MAX_FRAME));
    if (reply.type === "error") throw new ProtocolError(reply.code, reply.detail);
    return reply;
  }

  /** The head, or World `version`. */
  async resolve(version: number | null = null): Promise<Resolved> {
    const reply = await this.call({ type: "resolve", version });
    if (reply.type !== "resolved" || (version !== null && reply.number !== version)) throw unexpected();
    return { number: reply.number, id: reply.id, tree: reply.tree };
  }

  /** Who published `version`: its Layer and declared attribution, or null. */
  async attribution(version: number): Promise<Publication | null> {
    const reply = await this.call({ type: "attribution", version });
    if (reply.type !== "attributed" || reply.number !== version) throw unexpected();
    return reply.publication;
  }

  /** The canonical changeset bytes `version` applied to its parent, read in parts. */
  async changesetBytes(version: number): Promise<Uint8Array> {
    const parts: Uint8Array[] = [];
    let offset = 0;
    let total: number | null = null;
    for (;;) {
      const reply = await this.call({ type: "changeset", version, offset });
      if (reply.type !== "changesetPart") throw unexpected();
      total ??= reply.total;
      if (reply.total !== total || offset + reply.bytes.length > total || total > MAX_READ_CHANGESET) {
        throw new ProtocolError("malformed", `changeset part of version ${version} disagrees with its length`);
      }
      if (reply.bytes.length === 0 && offset < total) throw new ProtocolError("malformed", "empty changeset part");
      parts.push(reply.bytes);
      offset += reply.bytes.length;
      if (offset === total) break;
    }
    const out = new Uint8Array(offset);
    let at = 0;
    for (const part of parts) {
      out.set(part, at);
      at += part.length;
    }
    return out;
  }

  async changeset(version: number): Promise<Changeset> {
    return parseChangeset(await this.changesetBytes(version));
  }

  /**
   * Version `version` with its changeset, checked against its parent's ID: the node's
   * claimed ID must hash from the parent, the root and the changeset.
   */
  async verifiedVersion(version: number, parent: Resolved | null) {
    const resolved = await this.resolve(version);
    const bytes = await this.changesetBytes(version);
    const linked = (await versionId(parent?.id ?? null, resolved.tree, bytes)) === resolved.id;
    return { resolved, changeset: parseChangeset(bytes), linked };
  }

  /** Objects by ID, each checked against its ID. A reply may answer only a prefix; the rest is re-requested. */
  async objects(kind: Kind, ids: Hex[]): Promise<Map<Hex, Uint8Array>> {
    const unique = [...new Set(ids)];
    const out = new Map<Hex, Uint8Array>();
    let done = 0;
    while (done < unique.length) {
      const request = unique.slice(done, done + limits.MAX_WANT);
      const reply = await this.call({ type: "get", items: request.map((id) => [kind, id]) });
      if (reply.type !== "objects" || reply.objects.length === 0 || reply.objects.length > request.length) {
        throw unexpected();
      }
      for (const [i, object] of reply.objects.entries()) {
        if (object.kind !== kind || object.id !== request[i]) throw unexpected();
        if ((await objectId(kind, object.bytes)) !== object.id) {
          throw new ProtocolError("id_mismatch", `received bytes do not match ${object.id}`);
        }
        out.set(object.id, object.bytes);
        done++;
      }
    }
    return out;
  }

  async blob(id: Hex): Promise<Uint8Array> {
    return (await this.objects(Kind.Blob, [id])).get(id)!;
  }

  async chunk(id: Hex): Promise<Chunk> {
    return decodeChunk((await this.objects(Kind.Tree, [id])).get(id)!);
  }
}

/**
 * The response body, read chunk by chunk. A declared or actual length past `max` cancels
 * the stream and refuses, so a node cannot make a reader buffer without bound.
 */
export async function readBounded(response: Response, max: number): Promise<Uint8Array> {
  const tooLarge = () => new ProtocolError("limit_exceeded", `reply exceeds ${max} bytes`);
  const declared = Number(response.headers.get("content-length"));
  if (declared > max) {
    await response.body?.cancel();
    throw tooLarge();
  }
  if (!response.body) return new Uint8Array(0);
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let size = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    size += value.length;
    if (size > max) {
      await reader.cancel();
      throw tooLarge();
    }
    chunks.push(value);
  }
  const out = new Uint8Array(size);
  let at = 0;
  for (const chunk of chunks) {
    out.set(chunk, at);
    at += chunk.length;
  }
  return out;
}

function unexpected() {
  return new ProtocolError("unexpected_message", "the node replied with an unexpected message");
}
