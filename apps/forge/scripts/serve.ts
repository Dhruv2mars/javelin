// Serves the built forge locally: files from `dist/`, `forge.config.json` from memory, and
// index.html for every other path, as the Worker will.
import { join, normalize } from "node:path";
import type { Config } from "../src/routes";
import { dist } from "./build";

export function serveForge(port: number, config: Config) {
  return Bun.serve({
    port,
    hostname: "127.0.0.1",
    async fetch(request) {
      const path = normalize(decodeURIComponent(new URL(request.url).pathname));
      if (path === "/forge.config.json") return Response.json(config);
      if (!path.includes("..") && path !== "/") {
        const file = Bun.file(join(dist, path));
        if (await file.exists()) return new Response(file);
      }
      return new Response(Bun.file(join(dist, "index.html")), { headers: { "content-type": "text/html" } });
    },
  });
}
