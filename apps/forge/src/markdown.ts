// README rendering. Raw HTML in Markdown is shown as text, links keep only safe schemes,
// and relative links point at the forge's own file and directory pages.
import { Marked, type Tokens } from "marked";
import { escapeHtml } from "./highlight";

const SAFE = /^(https?:|mailto:|#)/i;

/** Renders `source`, resolving relative links against directory `dir` with `link`. */
export function renderMarkdown(source: string, link: (path: string) => string, dir = ""): string {
  const resolve = (href: string) => {
    if (SAFE.test(href)) return href;
    if (/^[a-z][a-z0-9+.-]*:/i.test(href) || href.startsWith("//")) return null;
    const [path] = href.split(/[?#]/);
    const parts = (path.startsWith("/") ? [] : dir.split("/").filter(Boolean)).concat(path.split("/"));
    const out: string[] = [];
    for (const part of parts) {
      if (part === "..") out.pop();
      else if (part && part !== ".") out.push(part);
    }
    return link(out.join("/"));
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
