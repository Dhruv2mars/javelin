// The forge in the browser: route, load page data from the node, render. No framework;
// every interpolated value goes through `esc`.
import {
  type Contents,
  type DirectoryPage,
  type FileChange,
  type FilePage,
  type HistoryPage,
  NodeClient,
  ProtocolError,
  type Publication,
  type VersionPage,
  loadDirectory,
  loadFile,
  loadHistory,
  loadVersion,
} from "./data";
import type { Hunk } from "./diff";
import { escapeHtml as esc, highlightLines, languageOf } from "./highlight";
import { landingPage, openTarget } from "./landing";
import { renderMarkdown } from "./markdown";
import { type Config, type Route, type Target, findTarget, href, parseRoute, readConfig } from "./routes";

const main = document.querySelector<HTMLElement>("#main")!;
const nodeLabel = document.querySelector<HTMLElement>("#node")!;
let config: Config = { repositories: {} };
let rendering = 0;

const short = (id: string) => id.slice(0, 12);
const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

function bytes(n: number) {
  if (n < 1024) return plural(n, "byte");
  if (n < 1024 ** 2) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 ** 2).toFixed(1)} MB`;
}

function storage(): Storage | null {
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

/** Who made a version, as a sentence. */
function attribution(number: number, p: Publication | null): string {
  if (!p) return number === 0 ? "The empty World every repository starts from" : "Advanced directly, without a Layer";
  const layer = `Layer <span class="layer">${esc(p.layer)}</span>`;
  if (p.attribution.kind === "human") return `Published by a person from ${layer}`;
  const trace = p.attribution.context
    ? `with trace <code>${esc(short(p.attribution.context))}</code>`
    : "without a trace";
  return `Published by an agent from ${layer}, ${trace}`;
}

function actor(p: Publication | null) {
  const kind = p ? p.attribution.kind : "none";
  const label = { human: "Person", agent: "Agent", none: "No Layer" }[kind];
  return `<span class="actor actor-${kind}" title="${label}" aria-label="${label}"></span>`;
}

function masthead(
  r: { owner: string; repo: string },
  number: number,
  head: number,
  tab: "code" | "history" | "version",
  detail: string,
) {
  const tabs = [
    ["code", "Code", href.tree(r.owner, r.repo, number)],
    ["history", "History", href.history(r.owner, r.repo)],
  ]
    .map(
      ([key, label, link]) =>
        `<a href="${link}"${key === tab ? ' aria-current="page"' : ""}>${label}</a>`,
    )
    .join("");
  const state = number === head ? "head" : `head is ${head}`;
  return `
    <header class="masthead">
      <p class="repo-name"><a href="${href.repo(r.owner, r.repo)}">${esc(r.owner)}<span class="slash">/</span>${esc(r.repo)}</a></p>
      <div class="world">
        <a class="numeral" href="${href.version(r.owner, r.repo, number)}" aria-label="World ${number}">${number}</a>
        <div class="world-text">
          <p class="world-title">World ${number} <span class="state">${state}</span></p>
          <p class="world-detail">${detail}</p>
        </div>
      </div>
      <nav class="tabs" aria-label="Repository">${tabs}</nav>
    </header>`;
}

function crumbs(r: { owner: string; repo: string }, version: number, path: string) {
  const parts = path.split("/").filter(Boolean);
  const links = [`<a href="${href.tree(r.owner, r.repo, version)}">${esc(r.repo)}</a>`];
  parts.forEach((part, i) => {
    const sub = parts.slice(0, i + 1).join("/");
    const last = i === parts.length - 1;
    links.push(
      last
        ? `<span aria-current="page">${esc(part)}</span>`
        : `<a href="${href.tree(r.owner, r.repo, version, sub)}">${esc(part)}</a>`,
    );
  });
  return `<nav class="crumbs" aria-label="Path">${links.join('<span class="sep">/</span>')}</nav>`;
}

function directoryPage(r: { owner: string; repo: string }, d: DirectoryPage) {
  const v = d.version.number;
  const parent = d.path.split("/").slice(0, -1).join("/");
  const up = d.path
    ? `<li class="entry entry-up"><a href="${href.tree(r.owner, r.repo, v, parent)}"><span class="glyph" aria-hidden="true">..</span>Parent directory</a></li>`
    : "";
  const rows = d.entries
    .map((e) => {
      const path = d.path ? `${d.path}/${e.name}` : e.name;
      const link = e.type === "dir" ? href.tree(r.owner, r.repo, v, path) : href.file(r.owner, r.repo, v, path);
      const kind = e.type === "dir" ? "dir" : e.entry.kind === "symlink" ? "link" : e.entry.executable ? "exec" : "file";
      return `<li class="entry entry-${kind}"><a href="${link}"><span class="glyph" aria-hidden="true"></span>${esc(e.name)}${e.type === "dir" ? "/" : ""}</a></li>`;
    })
    .join("");
  const empty = d.entries.length === 0 ? `<p class="empty">World ${v} has no files yet. Publish a Layer to add some.</p>` : "";
  const readme = d.readme
    ? `<article class="readme"><h2 class="readme-name">${esc(d.readme.name)}</h2><div class="prose">${renderMarkdown(
        d.readme.text,
        // A link routes by the kind of entry it names; a missing target opens the file page,
        // which says it is missing.
        (p) => (d.readme!.links[p] === "dir" ? href.tree(r.owner, r.repo, v, p) : href.file(r.owner, r.repo, v, p)),
        d.path,
      )}</div></article>`
    : "";
  const delayed = d.delayed
    ? `<p class="notice delayed">The public view of this repository is delayed. It shows the repository as it was a while ago; members see newer versions.</p>`
    : "";
  return `
    ${masthead(r, v, d.head, "code", `${attribution(v, d.publication)}. Tree <code>${short(d.version.tree)}</code>`)}
    ${delayed}
    <div class="code-layout">
      <section class="files" aria-label="Files">
        ${d.path ? crumbs(r, v, d.path) : ""}
        <ul class="entries">${up}${rows}</ul>
        ${empty}
      </section>
      ${readme}
    </div>`;
}

function fileBody(f: FilePage) {
  const c: Contents = f.contents;
  if (c.kind === "binary") return `<p class="notice">Binary file, ${bytes(c.size)}. It isn't shown here.</p>`;
  if (c.kind === "large") return `<p class="notice">This file is ${bytes(c.size)}, larger than the 1 MB the forge shows.</p>`;
  if (c.kind === "symlink") return `<p class="notice">Symbolic link to <code>${esc(c.target)}</code>.</p>`;
  if (c.text === "") return `<p class="notice">Empty file.</p>`;
  const rows = highlightLines(c.text, f.path)
    .map((html, i) => `<tr id="L${i + 1}"><td class="ln"><a href="#L${i + 1}">${i + 1}</a></td><td class="code">${html || " "}</td></tr>`)
    .join("");
  return `<div class="source"><table><tbody>${rows}</tbody></table></div>`;
}

function filePage(r: { owner: string; repo: string }, f: FilePage) {
  const v = f.version.number;
  const c = f.contents;
  const facts = [
    c.kind === "text" ? languageOf(f.path).name : c.kind === "symlink" ? "Symbolic link" : "Binary",
    "size" in c ? bytes(c.size) : null,
    c.kind === "text" ? plural(highlightLines(c.text, f.path).length, "line") : null,
    f.entry.kind === "regular" && f.entry.executable ? "Executable" : null,
  ].filter(Boolean);
  return `
    ${masthead(r, v, f.head, "code", `Reading <code>${esc(f.path)}</code> as of this version`)}
    <section class="file" aria-label="${esc(f.path)}">
      ${crumbs(r, v, f.path)}
      <div class="file-head">
        <ul class="facts">${facts.map((x) => `<li>${esc(String(x))}</li>`).join("")}</ul>
        <span class="blob">Blob <code>${short(f.blob)}</code></span>
      </div>
      ${fileBody(f)}
    </section>`;
}

function historyPage(r: { owner: string; repo: string }, h: HistoryPage) {
  const rows = h.rows
    .map(
      (row) => `
      <li class="version-row">
        <a class="line-number" href="${href.version(r.owner, r.repo, row.number)}">${row.number}</a>
        ${actor(row.publication)}
        <div class="version-body">
          <p class="version-who"><a href="${href.version(r.owner, r.repo, row.number)}">${attribution(row.number, row.publication)}</a></p>
          <p class="version-facts">
            <span>${plural(row.changed, "path")} changed</span>
            <span>ID <code>${short(row.id)}</code></span>
            ${row.linked ? `<span class="linked">${row.number === 0 ? "ID matches the genesis ID" : `ID checks out against World ${row.number - 1}`}</span>` : `<span class="unlinked">ID does not match its parent, root and changeset</span>`}
          </p>
        </div>
      </li>`,
    )
    .join("");
  const top = h.rows[0]?.number ?? h.head;
  const older = h.next === null ? "" : `<a class="older" href="${href.history(r.owner, r.repo, h.next)}">Show World ${h.next} and older</a>`;
  return `
    ${masthead(r, top, h.head, "history", `${plural(h.head + 1, "version")}, newest first`)}
    <ol class="world-line" aria-label="World versions">${rows}</ol>
    ${older}`;
}

const statusLabel = { added: "Added", deleted: "Deleted", modified: "Modified", mode: "Mode" };

function hunkRows(h: Hunk, whole: boolean) {
  // An added or deleted file is one hunk with nothing to locate.
  const head = whole ? "" : `<tr class="hunk-head"><td colspan="3">From old line ${h.oldStart}, new line ${h.newStart}</td></tr>`;
  return (
    head +
    h.lines
      .map((l) => {
        const sign = { add: "+", del: "−", ctx: "" }[l.kind];
        const eof = l.eof ? `<span class="eof">No newline at end of file</span>` : "";
        return `<tr class="d-${l.kind}"><td class="ln">${l.old ?? ""}</td><td class="ln">${l.new ?? ""}</td><td class="code"><span class="sign" aria-hidden="true">${sign}</span>${esc(l.text) || (eof ? "" : " ")}${eof}</td></tr>`;
      })
      .join("")
  );
}

function changeBlock(r: { owner: string; repo: string }, v: VersionPage, c: FileChange, i: number) {
  const link =
    c.status === "deleted" && v.parent
      ? href.file(r.owner, r.repo, v.parent.number, c.path)
      : href.file(r.owner, r.repo, v.version.number, c.path);
  const body = c.hunks
    ? c.hunks.length
      ? `<div class="source diff"><table><tbody>${c.hunks.map((h) => hunkRows(h, c.status === "added" || c.status === "deleted")).join("")}</tbody></table></div>`
      : `<p class="notice">Same text; only the entry changed.</p>`
    : `<p class="notice">${esc(c.note ?? "")}</p>`;
  return `
    <section class="change" id="change-${i}" aria-label="${esc(c.path)}">
      <header class="change-head">
        <span class="status status-${c.status}">${statusLabel[c.status]}</span>
        <a class="change-path" href="${link}">${esc(c.path)}</a>
        ${c.hunks ? `<span class="counts"><span class="plus">+${c.added}</span> <span class="minus">−${c.removed}</span></span>` : ""}
      </header>
      ${body}
    </section>`;
}

function versionPage(r: { owner: string; repo: string }, v: VersionPage) {
  const n = v.version.number;
  const against = v.parent
    ? `Changes against <a href="${href.version(r.owner, r.repo, v.parent.number)}">World ${v.parent.number}</a>`
    : "World 0 has no parent and no changes";
  const summary = v.changes
    .map(
      (c, i) =>
        `<li><a href="#change-${i}"><span class="status status-${c.status}">${statusLabel[c.status]}</span>${esc(c.path)}</a></li>`,
    )
    .join("");
  return `
    ${masthead(r, n, v.head, "version", `${attribution(n, v.publication)}. ID <code>${short(v.version.id)}</code>`)}
    <section class="version" aria-label="Changes in World ${n}">
      <p class="against">${against}. ${plural(v.changes.length, "path")} changed. <a href="${href.tree(r.owner, r.repo, n)}">Browse files at World ${n}</a></p>
      ${v.linked ? "" : `<p class="notice warn">This version's ID does not hash from its parent, root and changeset. The node may be serving altered history.</p>`}
      ${summary ? `<ul class="summary">${summary}</ul>` : ""}
      ${v.changes.map((c, i) => changeBlock(r, v, c, i)).join("")}
    </section>`;
}

function failure(error: unknown, target: Target | null) {
  const e = error instanceof ProtocolError ? error : new ProtocolError("error", String(error));
  const where = target ? esc(target.node) : "the node";
  const fix: Record<string, string> = {
    node_unavailable: `Can't reach ${where}. Check that the node is running and that the address is right.`,
    not_found: esc(e.detail.charAt(0).toUpperCase() + e.detail.slice(1)) + ".",
    missing_version: "That World version doesn't exist yet.",
    chain_mismatch: `${esc(e.detail)}, so ${where} may be serving altered files. Nothing from it is shown.`,
  };
  const technical = e.code === "node_unavailable" ? `<p class="muted"><code>${esc(e.detail)}</code></p>` : "";
  return `<section class="failure" role="alert"><h1>${fix[e.code] ? "Nothing to show" : "The node refused this read"}</h1><p>${fix[e.code] ?? `${esc(e.code)}: ${esc(e.detail)}`}</p>${technical}</section>`;
}

async function render() {
  const ticket = ++rendering;
  const route: Route = parseRoute(location.pathname, location.search);
  const show = (html: string, title: string) => {
    if (ticket !== rendering) return;
    main.innerHTML = html;
    document.title = title;
    main.removeAttribute("aria-busy");
  };
  if (route.page === "index") {
    nodeLabel.textContent = "";
    show(landingPage(config, storage()), "Javelin: version control for people and agents");
    // On a narrow screen the World line scrolls; start at the head, where Publish lands.
    const line = main.querySelector<HTMLElement>(".wl-scroll");
    if (line) line.scrollLeft = line.scrollWidth;
    return;
  }
  if (route.page === "missing") {
    return show(`<section class="failure"><h1>No page here</h1><p>Forge pages look like <code>/OWNER/REPO</code>.</p></section>`, "Not found");
  }
  const key = `${route.owner}/${route.repo}`;
  const target = findTarget(key, location.search, config, storage());
  if (!target) {
    nodeLabel.textContent = "";
    return show(
      `<section class="failure"><h1>Which node holds ${esc(key)}?</h1><p>Add <code>?node=URL&amp;repository=ID</code> to the address, or list the repository in <code>forge.config.json</code>.</p></section>`,
      key,
    );
  }
  nodeLabel.textContent = `Read from ${target.node.replace(/^https?:\/\//, "").replace(/\/$/, "")}`;
  main.setAttribute("aria-busy", "true");
  const client = new NodeClient(target.node, target.repository);
  const r = { owner: route.owner, repo: route.repo };
  try {
    switch (route.page) {
      case "tree": {
        const d = await loadDirectory(client, route.version, route.path);
        return show(directoryPage(r, d), route.path ? `${route.path} in ${key}` : key);
      }
      case "file": {
        return show(filePage(r, await loadFile(client, route.version, route.path)), `${route.path} in ${key}`);
      }
      case "history":
        return show(historyPage(r, await loadHistory(client, route.from)), `History of ${key}`);
      case "version":
        return show(versionPage(r, await loadVersion(client, route.version)), `World ${route.version} of ${key}`);
    }
  } catch (error) {
    show(failure(error, target), key);
  }
}

// Same-origin links navigate in place.
document.addEventListener("click", (event) => {
  const a = (event.target as Element).closest?.("a");
  if (!a || event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey) return;
  const url = new URL(a.href, location.href);
  if (url.origin !== location.origin || (url.pathname === location.pathname && url.hash)) return;
  event.preventDefault();
  history.pushState(null, "", url);
  window.scrollTo(0, 0);
  render();
});
window.addEventListener("popstate", render);

// The landing page's form opens a repository by its node address and ID.
document.addEventListener("submit", (event) => {
  const form = event.target as HTMLFormElement;
  if (form.id !== "open-repository") return;
  event.preventDefault();
  const field = (name: string) => String(new FormData(form).get(name) ?? "");
  const opened = openTarget(field("node"), field("repository"), field("name"));
  const error = form.querySelector<HTMLElement>("#open-error")!;
  if (!opened.ok) {
    error.textContent = opened.error;
    error.hidden = false;
    return;
  }
  error.hidden = true;
  history.pushState(null, "", opened.url);
  window.scrollTo(0, 0);
  render();
});

fetch("/forge.config.json")
  .then((r) => (r.ok ? r.json() : null))
  .catch(() => null)
  .then((value) => {
    config = readConfig(value);
    render();
  });
