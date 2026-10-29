// `bun tools/acceptance/relay.ts https://NODE 127.0.0.1:PORT`: a plain-HTTP relay on loopback to
// a node that cannot be stopped, such as a deployed Worker. Clones attach to the relay's URL;
// killing the relay is the node going down, and starting it again is the node coming back.
// `daily-workflow.sh` takes those as NODE_STOP and NODE_START. It prints `serving` once it
// listens.
const [target, listen] = process.argv.slice(2);
if (!target || !listen) throw new Error("usage: relay.ts https://NODE 127.0.0.1:PORT");
const [hostname, port] = listen.split(":");
Bun.serve({
  hostname,
  port: Number(port),
  async fetch(request) {
    const url = new URL(request.url);
    const headers = new Headers(request.headers);
    headers.delete("host");
    const body = request.method === "GET" || request.method === "HEAD" ? undefined : await request.arrayBuffer();
    const reply = await fetch(target.replace(/\/$/, "") + url.pathname + url.search, {
      method: request.method,
      headers,
      body,
    });
    const out = new Headers(reply.headers);
    out.delete("content-encoding");
    out.delete("content-length");
    return new Response(await reply.arrayBuffer(), { status: reply.status, headers: out });
  },
});
console.error(`relay serving on ${listen} for ${target}`);
