// Builds the forge to static assets in `dist/`: index.html, app.js and styles.css. Vercel
// serves them (`vercel.json`); any static host with an SPA fallback to index.html can.
import { cp, mkdir, rm, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";

export const dist = resolve(import.meta.dir, "../dist");
const src = resolve(import.meta.dir, "../src");

export async function build(): Promise<void> {
  await rm(dist, { recursive: true, force: true });
  await mkdir(dist, { recursive: true });
  const result = await Bun.build({
    entrypoints: [join(src, "app.ts")],
    outdir: dist,
    target: "browser",
    minify: true,
    sourcemap: "linked",
    naming: "[name].[ext]",
  });
  if (!result.success) throw new AggregateError(result.logs, "forge build failed");
  await cp(join(src, "index.html"), join(dist, "index.html"));
  await cp(join(src, "styles.css"), join(dist, "styles.css"));
  // No repositories are listed by default; the landing page opens them by node URL.
  await writeFile(join(dist, "forge.config.json"), '{"repositories":{}}\n');
}

if (import.meta.main) {
  await build();
  console.log(`built ${dist}`);
}
