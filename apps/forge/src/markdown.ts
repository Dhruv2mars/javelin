// README rendering. Raw HTML in Markdown is shown as text, links keep only safe schemes,
// and relative links point at the forge's own file and directory pages.
import { Marked, type Token, type Tokens, lexer } from "marked";
import { escapeHtml } from "./highlight";

const SAFE = /^(https?:|mailto:|#)/i;

/**
 * The repository path a relative link names from directory `dir`, decoded once, or null for
 * an absolute, unsafe or fragment-only link. `..` never climbs above the root.
 */
export function resolveRelative(href: string, dir = ""): string | null {
  if (SAFE.test(href) || /^[a-z][a-z0-9+.-]*:/i.test(href) || href.startsWith("//")) return null;
  const [path] = href.split(/[?#]/);
  const parts = (path.startsWith("/") ? [] : dir.split("/").filter(Boolean)).concat(path.split("/"));
  const out: string[] = [];
  for (const raw of parts) {
    let part = raw;
    try {
      part = decodeURIComponent(raw);
    } catch {
      // keep a segment with a malformed escape as written
    }
    if (part === "..") out.pop();
    else if (part && part !== ".") out.push(part);
  }
  return out.join("/");
}

/** Every relative link and image target in `source`, resolved from `dir`, without repeats. */
export function relativeTargets(source: string, dir = ""): string[] {
  const found = new Set<string>();
  const walk = (tokens: Token[]) => {
    for (const token of tokens) {
      if (token.type === "link" || token.type === "image") {
        const path = resolveRelative((token as Tokens.Link).href, dir);
        if (path !== null) found.add(path);
      }
      const nested = token as { tokens?: Token[]; items?: Token[] };
      if (nested.tokens) walk(nested.tokens);
      if (nested.items) walk(nested.items);
    }
  };
  walk(lexer(source, { gfm: true }));
  return [...found];
}

/** Renders `source`, sending each relative link target (resolved from `dir`) through `link`. */
export function renderMarkdown(source: string, link: (path: string) => string, dir = ""): string {
  const resolve = (href: string) => {
    if (SAFE.test(href)) return href;
    const path = resolveRelative(href, dir);
    return path === null ? null : link(path);
  };
  const marked = new Marked({
    gfm: true,
    renderer: {
      html: ({ text }: Tokens.HTML | Tokens.Tag) => escapeHtml(text),
      link({ href, title, tokens }: Tokens.Link) {
        const inner = this.parser.parseInline(tokens);
        const target = resolve(href);
        if (target === null) return inner;
        const t = title ? ` title="${escapeHtml(title)}"` : "";
        return `<a href="${escapeHtml(target)}"${t}>${inner}</a>`;
      },
      // Images would need a blob URL per object; show the alt text as a link to the file.
      image({ href, text }: Tokens.Image) {
        const target = resolve(href);
        const alt = escapeHtml(text || href);
        return target === null ? alt : `<a class="md-image" href="${escapeHtml(target)}">${alt}</a>`;
      },
    },
  });
  return marked.parse(source, { async: false }) as string;
}
