// `bun run dev`: builds the binaries, seeds a demo repository, starts `javelin-node serve` on
// loopback, builds the forge and serves it. Open the printed URL. Ctrl-C stops both.
//
//   FORGE_PORT   forge port (default 5178)
//   NODE_PORT    node port (default: a free one)
//   FORGE_REPO   serve an existing repository directory instead of seeding one
import { build } from "./build";
import { DEMO_REPOSITORY, binaries, scratch, seedDemo, startNode } from "./node";
import { serveForge } from "./serve";

const bins = await binaries();
let dir = process.env.FORGE_REPO;
if (!dir) {
  dir = await scratch();
  await seedDemo(bins.javelin, dir);
}
const node = await startNode(bins.node, dir, DEMO_REPOSITORY, Number(process.env.NODE_PORT) || undefined);
await build();
const forge = serveForge(Number(process.env.FORGE_PORT) || 5178, {
  repositories: { "demo/lantern": { node: node.url, repository: node.repository } },
});
console.log(`node   ${node.url}/v2/r/${node.repository}  (data in ${dir})`);
console.log(`forge  http://127.0.0.1:${forge.port}/demo/lantern`);

const stop = async () => {
  forge.stop(true);
  await node.stop();
  process.exit(0);
};
process.on("SIGINT", stop);
process.on("SIGTERM", stop);
