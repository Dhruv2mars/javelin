// `bun run deploy`: builds the forge and deploys it to production on Vercel (project
// `javelin-codes`, Hobby plan) with the Vercel CLI. Run it from a clean `main` checkout, logged
// in to the `dhruv2mars` Vercel account. Vercel builds nothing itself: `vercel build` runs here
// with `vercel.json`, and only the prebuilt output is uploaded.
import { resolve } from "node:path";

const env = {
  ...process.env,
  // The project and its team. These are identifiers, not secrets; access comes from the login.
  VERCEL_ORG_ID: "team_ocQ5Nvaz3YnuTsKLunP9v0dU",
  VERCEL_PROJECT_ID: "prj_B6q3FY1ByLv4LWQ5MCNhFd9dj2ef",
};
const cwd = resolve(import.meta.dir, "..");

for (const args of [
  ["pull", "--yes", "--environment=production"],
  ["build", "--prod"],
  ["deploy", "--prebuilt", "--prod"],
]) {
  const code = await Bun.spawn(["vercel", ...args], { cwd, env, stdio: ["inherit", "inherit", "inherit"] }).exited;
  if (code !== 0) process.exit(code);
}
