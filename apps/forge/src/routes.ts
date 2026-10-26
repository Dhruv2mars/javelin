// URL shapes and node lookup. Until the control plane answers "where does owner/repo live",
// the node comes from `?node=URL&repository=ID` (remembered per repository in this browser)
// or from `forge.config.json` beside the static assets.

export type Route =
  | { page: "index" }
  | { page: "tree"; owner: string; repo: string; version: number | null; path: string }
  | { page: "file"; owner: string; repo: string; version: number; path: string }
  | { page: "history"; owner: string; repo: string; from: number | null }
  | { page: "version"; owner: string; repo: string; version: number }
  | { page: "missing"; path: string };

const NAME = /^[A-Za-z0-9._-]{1,100}$/;
const NUMBER = /^(0|[1-9]\d{0,15})$/;

export function parseRoute(pathname: string, search = ""): Route {
  const parts = pathname.split("/").filter(Boolean).map(decodeURIComponent);
  if (parts.length === 0) return { page: "index" };
  const [owner, repo, view, at, ...rest] = parts;
  const missing: Route = { page: "missing", path: pathname };
  if (!owner || !repo || !NAME.test(owner) || !NAME.test(repo)) return missing;
  const path = rest.join("/");
  if (view === undefined) return { page: "tree", owner, repo, version: null, path: "" };
  if (view === "history" && at === undefined) {
    const from = new URLSearchParams(search).get("from");
    return { page: "history", owner, repo, from: from && NUMBER.test(from) ? Number(from) : null };
  }
  if (at === undefined || !NUMBER.test(at)) return missing;
  const version = Number(at);
  if (view === "tree") return { page: "tree", owner, repo, version, path };
  if (view === "file" && path) return { page: "file", owner, repo, version, path };
  if (view === "version" && rest.length === 0) return { page: "version", owner, repo, version };
  return missing;
}

const encodePath = (path: string) => path.split("/").map(encodeURIComponent).join("/");

export const href = {
  repo: (o: string, r: string) => `/${o}/${r}`,
  tree: (o: string, r: string, v: number, path = "") => `/${o}/${r}/tree/${v}${path ? `/${encodePath(path)}` : ""}`,
  file: (o: string, r: string, v: number, path: string) => `/${o}/${r}/file/${v}/${encodePath(path)}`,
  history: (o: string, r: string, from: number | null = null) => `/${o}/${r}/history${from === null ? "" : `?from=${from}`}`,
  version: (o: string, r: string, v: number) => `/${o}/${r}/version/${v}`,
};

export type Target = { node: string; repository: string };
export type Config = { repositories: Record<string, Target> };

/** Validates a parsed `forge.config.json`. Entries that are not a node URL and an ID are dropped. */
export function readConfig(value: unknown): Config {
  const out: Config = { repositories: {} };
  const repos = (value as { repositories?: unknown })?.repositories;
  if (!repos || typeof repos !== "object") return out;
  for (const [key, target] of Object.entries(repos as Record<string, unknown>)) {
    const t = target as Partial<Target>;
    if (/^[^/]+\/[^/]+$/.test(key) && validTarget(t)) out.repositories[key] = { node: t.node!, repository: t.repository! };
  }
  return out;
}

export function validTarget(t: Partial<Target> | null | undefined): t is Target {
  return (
    typeof t?.node === "string" &&
    /^https?:\/\/[^/\s]+\/?$/.test(t.node) &&
    typeof t.repository === "string" &&
    /^[A-Za-z0-9._:-]{1,128}$/.test(t.repository)
  );
}

/** The node for `owner/repo`: query parameters first, then the config, then remembered ones. */
export function findTarget(
  key: string,
  search: string,
  config: Config,
  remembered: Storage | null,
): Target | null {
  const q = new URLSearchParams(search);
  const fromQuery = { node: q.get("node") ?? undefined, repository: q.get("repository") ?? undefined };
  if (validTarget(fromQuery)) {
    remembered?.setItem(`forge:${key}`, JSON.stringify(fromQuery));
    return fromQuery;
  }
  if (config.repositories[key]) return config.repositories[key];
  try {
    const saved = JSON.parse(remembered?.getItem(`forge:${key}`) ?? "null");
    if (validTarget(saved)) return saved;
  } catch {
    // ignore a corrupt remembered value
  }
  return null;
}
