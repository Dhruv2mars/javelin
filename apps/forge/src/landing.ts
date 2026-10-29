// The page at `/`: what Javelin is, how to install it, and a form that opens a repository
// on a node by URL. The World line in the hero is the forge's spine, drawn once.
import { escapeHtml as esc } from "./highlight";
import { type Config, type Target, validTarget } from "./routes";

const SOURCE = "https://entire.com/et/dhruv2mars/javelin";

/** A forge name (`owner/repo`) for a target opened without one: the node's host and the ID. */
export function defaultName(t: Target): string {
  const clean = (s: string) => s.replace(/[^A-Za-z0-9._-]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 100) || "node";
  return `${clean(new URL(t.node).host)}/${clean(t.repository)}`;
}

/** Reads the open form. A name, when given, must be `owner/repo` in forge characters. */
export function openTarget(
  node: string,
  repository: string,
  name: string,
): { ok: true; url: string } | { ok: false; error: string } {
  const t = { node: node.trim().replace(/\/+$/, ""), repository: repository.trim() };
  if (!/^https?:\/\/[^/\s]+$/.test(t.node)) {
    return { ok: false, error: "Enter the node's address with its scheme and no path, like https://node.example.com." };
  }
  if (!validTarget(t)) {
    return { ok: false, error: "A repository ID is 1 to 128 letters, digits, dots, colons, dashes or underscores." };
  }
  try {
    new URL(t.node);
  } catch {
    return { ok: false, error: "That node address isn't a valid URL. Check the host and port." };
  }
  const key = name.trim() || defaultName(t);
  // `.` and `..` would be collapsed out of the page's path.
  if (!/^[A-Za-z0-9._-]{1,100}\/[A-Za-z0-9._-]{1,100}$/.test(key) || key.split("/").some((p) => /^\.\.?$/.test(p))) {
    return { ok: false, error: "Name the repository as owner/repo, using letters, digits, dots, dashes or underscores." };
  }
  const q = new URLSearchParams({ node: t.node, repository: t.repository });
  return { ok: true, url: `/${key}?${q}` };
}

/** Repositories opened earlier in this browser, from the `forge:owner/repo` keys `findTarget` writes. */
export function rememberedRepositories(storage: Storage | null): [string, Target][] {
  const out: [string, Target][] = [];
  let length = 0;
  try {
    length = storage?.length ?? 0;
  } catch {
    // unreadable storage lists nothing
  }
  for (let i = 0; i < length; i++) {
    try {
      const key = storage!.key(i);
      if (!key?.startsWith("forge:")) continue;
      const target = JSON.parse(storage!.getItem(key) ?? "null");
      if (validTarget(target)) out.push([key.slice("forge:".length), target]);
    } catch {
      // a corrupt entry is skipped
    }
  }
  return out.sort(([a], [b]) => a.localeCompare(b));
}

// Versions 7 to 13 on the line, 116 apart; the public frontier sits between 10 and 11.
const X = (n: number) => 64 + (n - 7) * 116;

function worldLine() {
  const versions = [7, 8, 9, 10, 11, 12]
    .map(
      (n) =>
        `<g class="wl-version"><circle cx="${X(n)}" cy="132" r="6"/><text x="${X(n)}" y="104">${n}</text></g>`,
    )
    .join("");
  const checkpoints = [0.3, 0.52, 0.74]
    .map((t) => `<circle class="wl-checkpoint" cx="${X(11) + 40 + t * (X(13) - X(11) - 80)}" cy="214" r="4.5"/>`)
    .join("");
  const frontier = (X(10) + X(11)) / 2;
  return `
    <figure class="world-line-figure">
      <div class="wl-scroll">
        <svg class="wl" viewBox="0 0 900 290" role="img" aria-labelledby="wl-title wl-desc">
          <title id="wl-title">A World and a Layer</title>
          <desc id="wl-desc">World versions 7 to 12 lie on one line. A Layer starts from World 11, takes three checkpoints, and publishes as World 13. Public readers see up to World 10; members see the head.</desc>
          <path class="wl-past" d="M0 132 H${X(7)}"/>
          <path class="wl-world" d="M${X(7)} 132 H${X(12)}"/>
          <path class="wl-next" pathLength="1" d="M${X(12)} 132 H${X(13)}"/>
          <line class="wl-frontier" x1="${frontier}" y1="28" x2="${frontier}" y2="250"/>
          <text class="wl-note wl-end" x="${frontier - 14}" y="40">Public readers see up to here</text>
          <text class="wl-note" x="${frontier + 14}" y="40">Members see the head</text>
          ${versions}
          <path class="wl-layer" pathLength="1" d="M${X(11)} 132 C${X(11)} 196, ${X(11) + 20} 214, ${X(11) + 60} 214 H${X(13) - 60} C${X(13) - 20} 214, ${X(13)} 196, ${X(13)} 138"/>
          ${checkpoints}
          <text class="wl-layer-label" x="${X(12)}" y="250" text-anchor="middle">Layer fix-parser, an agent's own directory</text>
          <g class="wl-head"><circle cx="${X(13)}" cy="132" r="9"/><text x="${X(13)}" y="104">13</text></g>
          <text class="wl-note wl-publish" x="${X(13) + 18}" y="176">Publish</text>
        </svg>
      </div>
    </figure>`;
}

function openForm() {
  return `
    <form class="open-form" id="open-repository" novalidate>
      <label for="open-node">Node address</label>
      <input id="open-node" name="node" type="url" inputmode="url" autocomplete="url" spellcheck="false" placeholder="https://node.example.com" required />
      <label for="open-id">Repository ID</label>
      <input id="open-id" name="repository" type="text" autocomplete="off" spellcheck="false" placeholder="repo_lantern" required />
      <label for="open-name">Show it as <span class="optional">(optional)</span></label>
      <input id="open-name" name="name" type="text" autocomplete="off" spellcheck="false" placeholder="owner/repo" aria-describedby="open-name-help" />
      <p class="field-help" id="open-name-help">Without a name, the forge uses the node's host and the ID.</p>
      <p class="form-error" id="open-error" role="alert" hidden></p>
      <button type="submit">Open repository</button>
    </form>`;
}

function repositoryList(config: Config, storage: Storage | null) {
  const listed = new Map<string, Target>(Object.entries(config.repositories));
  for (const [key, t] of rememberedRepositories(storage)) if (!listed.has(key)) listed.set(key, t);
  if (listed.size === 0) return "";
  const rows = [...listed]
    .map(
      ([key, t]) =>
        `<li class="entry entry-dir"><a href="/${esc(key)}"><span class="glyph" aria-hidden="true"></span>${esc(key)}</a> <span class="muted">on ${esc(t.node.replace(/^https?:\/\//, ""))}</span></li>`,
    )
    .join("");
  return `<div class="known"><h3>Opened in this browser</h3><ul class="entries">${rows}</ul></div>`;
}

/**
 * Whether a release is published on npm (docs/releases.md). Until it is, the section says
 * installers are coming; the commands would find no package.
 */
export const INSTALLERS_PUBLISHED = true;
const SITE = "https://javelin-codes.vercel.app";

/** The install section: the one-line installers once a release is published. */
export function installSection(published = INSTALLERS_PUBLISHED): string {
  if (!published) {
    return `
      <section class="band" id="install" aria-labelledby="install-title">
        <div class="band-head">
          <h2 id="install-title">Install the CLI</h2>
          <p>Install opens soon.</p>
        </div>
        <div class="band-body prose-block">
          <p>Javelin is pre-release. Release builds with installers for macOS, Linux and Windows are coming, and the commands will be here. Until then you can read <a href="${SOURCE}">the source on Entire</a>.</p>
        </div>
      </section>`;
  }
  return `
      <section class="band" id="install" aria-labelledby="install-title">
        <div class="band-head">
          <h2 id="install-title">Install the CLI</h2>
          <p>One command installs <code>javelin</code> and <code>javelin-node</code>.</p>
        </div>
        <div class="band-body prose-block">
          <p>With npm or bun, on macOS or Linux:</p>
          <pre class="install-command"><code>npm i -g @dhruv2mars/javelin</code></pre>
          <pre class="install-command"><code>bun add -g @dhruv2mars/javelin</code></pre>
          <p>Without either (macOS and Linux, x86_64 or arm64, glibc 2.28 or newer):</p>
          <pre class="install-command"><code>curl -fsSL ${SITE}/install.sh | sh</code></pre>
          <p>The installer fetches the same package from the npm registry, checks it against the registry's SHA-512 before it writes anything, and puts both programs in <code>~/.javelin/bin</code>. It tells you how to add that to your <code>PATH</code> and never edits your shell profile. Then run <code>javelin --version</code> and <code>javelin init</code> in a project.</p>
          <p class="coming">Windows has no release yet. Javelin is pre-release; read <a href="${SOURCE}">the source on Entire</a>.</p>
        </div>
      </section>`;
}

export function landingPage(config: Config, storage: Storage | null) {
  return `
    <div class="landing">
      <section class="hero" aria-labelledby="hero-title">
        <h1 id="hero-title">Version control for code that people and agents write together.</h1>
        <p class="lede">Javelin is two things. The <code>javelin</code> command line replaces Git. This site, the forge, replaces GitHub. Every person and every agent works in a Layer of their own, and finished work lands on the repository in one step.</p>
        ${worldLine()}
      </section>

      <section class="band open-band" id="open" aria-labelledby="open-title">
        <div class="band-head">
          <h2 id="open-title">Open a repository</h2>
          <p>Each repository lives on a node. Your browser reads it from there directly and checks every file and version against its ID. Public repositories need no account.</p>
        </div>
        <div class="band-body">
          ${openForm()}
          ${repositoryList(config, storage)}
        </div>
      </section>

      <section class="band" aria-labelledby="world-title">
        <div class="band-head"><h2 id="world-title">How work moves</h2></div>
        <dl class="band-body terms">
          <dt>The World</dt>
          <dd>A repository's history is one line of numbered versions. A version never changes after it exists, and everyone who can see it sees the same ID.</dd>
          <dt>Layers</dt>
          <dd>Each person or agent works in a Layer: a plain directory cloned from a World version, isolated from every other Layer. Editors, compilers and agents use it like any folder. Javelin records its changes as checkpoints.</dd>
          <dt>Publish</dt>
          <dd>Publish puts a whole Layer onto the World as the next version, all of it or none of it. When someone published first, Javelin composes the two or reports the conflict. By default a private repository publishes directly and a public one goes through a Publish Request.</dd>
        </dl>
      </section>

      <section class="band" aria-labelledby="hosting-title">
        <div class="band-head"><h2 id="hosting-title">Your repository stays on your infrastructure</h2></div>
        <div class="band-body prose-block">
          <p>Javelin runs no hosting. A repository lives on a node that its owner runs: <code>javelin-node</code> on a machine you control, or a node in your own Cloudflare account. Javelin never pays for, holds or reads your storage. The CLI and this site talk to the node directly.</p>
          <p class="coming">Today you can run <code>javelin-node</code> yourself. Connecting a Cloudflare account from the forge is being built.</p>
        </div>
      </section>

      <section class="band" aria-labelledby="audience-title">
        <div class="band-head"><h2 id="audience-title">Who sees what, and when</h2></div>
        <div class="band-body">
          <p class="prose-block">A repository has one history, and every reader sees the same versions with the same IDs. What changes is what each reader can open.</p>
          <dl class="terms">
            <dt>Path audiences</dt>
            <dd>Files can belong to the repository without being public. <code>.env*</code> can be readable by maintainers only while <code>src/**</code> is public.</dd>
            <dt>Disclosure delay</dt>
            <dd>Public readers can see the World as it was a fixed time ago, such as 7 days, while members see the head. A security fix is then not public the moment it lands, while users still run the old code.</dd>
            <dt>Embargo</dt>
            <dd>A Publish can wait for a release time or a maintainer's say-so, then becomes public with everything before it.</dd>
          </dl>
          <p class="coming">The storage format for this is fixed; audiences, the delay and embargoes are being built.</p>
        </div>
      </section>

      ${installSection()}

      <footer class="landing-foot">
        <p>Javelin is open source and pre-release. <a href="${SOURCE}">Source on Entire</a>.</p>
      </footer>
    </div>`;
}
