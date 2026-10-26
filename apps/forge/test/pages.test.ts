// Page data loading against a real `javelin-node serve` on loopback, seeded with the
// `javelin` CLI. No part of the node is mocked.
import { afterAll, beforeAll, expect, test } from "bun:test";
import {
  Kind,
  type Message,
  NodeClient,
  ProtocolError,
  decode,
  encode,
  objectId,
  versionId,
} from "../../../packages/protocol/src";
import { loadDirectory, loadFile, loadHistory, loadVersion } from "../src/data";
import { DEMO, type RunningNode, binaries, freePort, scratch, seedDemo, startNode } from "../scripts/node";

let node: RunningNode;
let client: NodeClient;

beforeAll(async () => {
  const bins = await binaries();
  const dir = await scratch();
  await seedDemo(bins.javelin, dir);
  node = await startNode(bins.node, dir);
  client = new NodeClient(node.url, node.repository);
}, 600_000);

afterAll(async () => {
  await node?.stop();
});

const refusal = (promise: Promise<unknown>) =>
  promise.then(
    () => "resolved",
    (error) => (error as ProtocolError).code,
  );

test("the head and World 0 hash from their parts", async () => {
  const head = await client.resolve();
  expect(head.number).toBe(4);
  const genesis = await client.resolve(0);
  const emptyTree = await objectId(Kind.Tree, Uint8Array.of(0));
  expect(genesis.tree).toBe(emptyTree);
  expect(await versionId(null, emptyTree, new TextEncoder().encode("{}"))).toBe(genesis.id);
});

test("repository home lists the head's files and renders its README", async () => {
  const home = await loadDirectory(client);
  expect(home.version.number).toBe(4);
  expect(home.head).toBe(4);
  expect(home.entries.map((e) => e.name)).toEqual([
    "assets",
    "docs",
    "scripts",
    "src",
    "tests",
    "Cargo.toml",
    "README.md",
  ]);
  expect(home.readme?.name).toBe("README.md");
  expect(home.readme?.text).toContain("[the guide](docs/guide.md)");
  // README links route by the kind of entry they name.
  expect(home.readme?.links).toEqual({ "docs/guide.md": "file", src: "dir", "src/main.rs": "file" });
  expect(home.publication).toEqual({ layer: "icon", attribution: { kind: "human" } });
});

test("a directory reads at an older version", async () => {
  const docs = await loadDirectory(client, 1, "docs");
  expect(docs.version.number).toBe(1);
  expect(docs.entries.map((e) => e.name)).toEqual(["usage.md"]);
  const scripts = await loadDirectory(client, 4, "scripts");
  const build = scripts.entries[0];
  expect(build.type === "file" && build.entry.kind === "regular" && build.entry.executable).toBe(
    process.platform !== "win32",
  );
  expect(await refusal(loadDirectory(client, 4, "nope"))).toBe("not_found");
});

test("file view shows text, and a notice for binary content", async () => {
  const main = await loadFile(client, 4, "src/main.rs");
  expect(main.contents).toEqual({ kind: "text", text: DEMO.main, size: DEMO.main.length });
  expect(main.blob).toBe(await objectId(Kind.Blob, new TextEncoder().encode(DEMO.main)));
  const icon = await loadFile(client, 4, "assets/icon.png");
  expect(icon.contents).toEqual({ kind: "binary", size: DEMO.png.length });
  const old = await loadFile(client, 1, "docs/usage.md");
  expect(old.contents.kind === "text" && old.contents.text).toBe(DEMO.usage);
  expect(await refusal(loadFile(client, 3, "docs/usage.md"))).toBe("not_found");
  expect(await refusal(loadFile(client, 9, "README.md"))).toBe("missing_version");
});

test("history lists every version newest first with attribution and changed paths", async () => {
  const history = await loadHistory(client);
  expect(history.head).toBe(4);
  expect(history.next).toBeNull();
  expect(history.rows.map((r) => [r.number, r.changed, r.linked])).toEqual([
    [4, 1, true],
    [3, 3, true],
    [2, 2, true],
    [1, 6, true],
    [0, 0, true],
  ]);
  const [four, three, two, one, zero] = history.rows.map((r) => r.publication);
  expect(four).toEqual({ layer: "icon", attribution: { kind: "human" } });
  expect(three?.layer).toBe("docs-guide");
  expect(three?.attribution.kind === "agent" && three.attribution.context).toHaveLength(64);
  expect(two).toEqual({ layer: "lists", attribution: { kind: "agent", context: null, allowMissingTrace: true } });
  expect(one).toEqual({ layer: "scaffold", attribution: { kind: "human" } });
  expect(zero).toBeNull();
  for (const row of history.rows) expect(row.id).toBe((await client.resolve(row.number)).id);

  const page = await loadHistory(client, null, 2);
  expect(page.rows.map((r) => r.number)).toEqual([4, 3]);
  expect(page.next).toBe(2);
  expect((await loadHistory(client, page.next, 2)).rows.map((r) => r.number)).toEqual([2, 1]);
});

test("version view diffs each changed path against the parent", async () => {
  const three = await loadVersion(client, 3);
  expect(three.parent?.number).toBe(2);
  expect(three.linked).toBe(true);
  const byPath = Object.fromEntries(three.changes.map((c) => [c.path, c]));
  expect(Object.keys(byPath)).toEqual(["README.md", "docs/guide.md", "docs/usage.md"]);
  expect(byPath["README.md"].status).toBe("modified");
  expect(byPath["README.md"].added).toBe(1);
  expect(byPath["README.md"].removed).toBe(1);
  const lines = byPath["README.md"].hunks!.flatMap((h) => h.lines);
  expect(lines.find((l) => l.kind === "add")?.text).toContain("[the guide](docs/guide.md)");
  expect(lines.find((l) => l.kind === "del")?.text).toContain("[the usage guide](docs/usage.md)");
  expect(byPath["docs/guide.md"].status).toBe("added");
  expect(byPath["docs/usage.md"]).toMatchObject({ status: "deleted", removed: 3, added: 0 });

  const two = await loadVersion(client, 2);
  const lib = two.changes.find((c) => c.path === "src/lib.rs")!;
  expect(lib).toMatchObject({ status: "modified", added: 2, removed: 0 });
  expect(lib.hunks).toHaveLength(1);
  expect(lib.hunks![0].lines.filter((l) => l.kind === "ctx").length).toBe(6);

  const four = await loadVersion(client, 4);
  expect(four.changes).toMatchObject([{ path: "assets/icon.png", status: "added", hunks: null, note: "Binary file changed" }]);
  const zero = await loadVersion(client, 0);
  expect(zero.parent).toBeNull();
  expect(zero.changes).toEqual([]);
});

test("objects that do not match their IDs are refused", async () => {
  // The real node's replies, with one byte of every object flipped in transit.
  const tampering = async (url: string, init: RequestInit) => {
    const reply = new Uint8Array(await (await fetch(url, init)).arrayBuffer());
    if (reply[0] === 0x20 && reply.length > 45) reply[reply.length - 1] ^= 0xff;
    return new Response(reply);
  };
  const tampered = new NodeClient(node.url, node.repository, tampering);
  expect(await refusal(loadFile(tampered, 4, "README.md"))).toBe("id_mismatch");
});

test("code pages refuse a tree substituted under a real version ID", async () => {
  const real = await client.resolve(4);
  const other = await client.resolve(1);
  // The real node's replies, with World 4's root swapped for World 1's real root.
  const substituting = async (url: string, init: RequestInit) => {
    const request = decode(new Uint8Array(init.body as Uint8Array<ArrayBuffer>));
    const response = await fetch(url, init);
    if (request.type !== "resolve") return response;
    const reply = decode(new Uint8Array(await response.arrayBuffer())) as Message;
    if (reply.type === "resolved" && reply.number === 4) reply.tree = other.tree;
    return new Response(encode(reply));
  };
  const lying = new NodeClient(node.url, node.repository, substituting);
  expect((await lying.resolve()).id).toBe(real.id);
  expect(await refusal(loadDirectory(lying))).toBe("chain_mismatch");
  expect(await refusal(loadDirectory(lying, 4, "docs"))).toBe("chain_mismatch");
  expect(await refusal(loadFile(lying, 4, "docs/usage.md"))).toBe("chain_mismatch");
  // Other versions are untouched and still load.
  expect((await loadFile(lying, 1, "docs/usage.md")).contents.kind).toBe("text");
});

test("browsers can read the node, and only read", async () => {
  const url = `${node.url}/v2/r/${node.repository}`;
  const preflight = await fetch(url, {
    method: "OPTIONS",
    headers: { origin: "http://127.0.0.1:5178", "access-control-request-method": "POST" },
  });
  expect(preflight.status).toBe(204);
  expect(preflight.headers.get("access-control-allow-origin")).toBe("*");
  expect(preflight.headers.get("access-control-allow-headers")).toBe("content-type");
  const read = await fetch(url, {
    method: "POST",
    headers: { origin: "http://127.0.0.1:5178", "content-type": "application/octet-stream" },
    body: Uint8Array.from([0x40, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
  });
  expect(read.headers.get("access-control-allow-origin")).toBe("*");
});

test("a stopped node is node_unavailable", async () => {
  const nowhere = new NodeClient(`http://127.0.0.1:${await freePort()}`, node.repository);
  expect(await refusal(nowhere.resolve())).toBe("node_unavailable");
});
