// The forge reads a delayed repository signed out, so a real `javelin-node serve` shows it the
// public frontier as the head, and the repository home says the view is delayed. The node's
// disclosure state is written to its `node.db` before it starts, standing in for a maintainer's
// `javelin delay` and a week of elapsed time.
import { Database } from "bun:sqlite";
import { afterAll, beforeAll, expect, test } from "bun:test";
import { join } from "node:path";
import { NodeClient, type ProtocolError } from "../../../packages/protocol/src";
import { loadDirectory, loadHistory } from "../src/data";
import { type RunningNode, binaries, scratch, seedDemo, startNode } from "../scripts/node";

const DAY = 86_400;
let node: RunningNode;
let client: NodeClient;

beforeAll(async () => {
  const bins = await binaries();
  const dir = await scratch();
  await seedDemo(bins.javelin, dir);
  // A week's delay; Worlds 0 to 2 were published a month ago, Worlds 3 and 4 just now.
  const now = Math.floor(Date.now() / 1000);
  const db = new Database(join(dir, "node.db"));
  db.run("CREATE TABLE IF NOT EXISTS disclosure_delay (one INTEGER PRIMARY KEY CHECK(one=0), delay INTEGER NOT NULL)");
  db.run("CREATE TABLE IF NOT EXISTS version_times (version INTEGER PRIMARY KEY, at INTEGER NOT NULL)");
  db.run("INSERT INTO disclosure_delay(one,delay) VALUES(0,?)", [7 * DAY]);
  for (const [version, at] of [
    [0, now - 30 * DAY],
    [1, now - 30 * DAY],
    [2, now - 30 * DAY],
    [3, now],
    [4, now],
  ]) {
    db.run("INSERT INTO version_times(version,at) VALUES(?,?)", [version, at]);
  }
  db.close();
  node = await startNode(bins.node, dir);
  client = new NodeClient(node.url, node.repository);
}, 600_000);

afterAll(async () => {
  await node?.stop();
});

test("the repository home shows the public frontier and says the view is delayed", async () => {
  const home = await loadDirectory(client);
  expect(home.version.number).toBe(2);
  expect(home.head).toBe(2);
  expect(home.delayed).toBe(true);
  // World 4's icon is not there yet.
  expect(home.entries.map((e) => e.name)).not.toContain("assets");
});

test("history ends at the frontier and nothing above it is readable", async () => {
  const history = await loadHistory(client);
  expect(history.rows.map((r) => r.number)).toEqual([2, 1, 0]);
  const refusal = await client.resolve(3).then(
    () => "resolved",
    (error) => (error as ProtocolError).code,
  );
  expect(refusal).toBe("missing_version");
});

test("an outsider is told the view is delayed, not by how much", async () => {
  expect(await client.disclosure()).toEqual({ delayed: true, delay: null, frontier: 2, embargoes: [] });
});
