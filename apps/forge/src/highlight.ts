// A small syntax-aware highlighter: comments, strings, numbers and keywords per language,
// chosen by file name. It returns escaped HTML per line, so a token never spans a line.

type Language = {
  name: string;
  line?: string[];
  block?: [string, string];
  strings: string[];
  keywords: Set<string>;
};

const words = (s: string) => new Set(s.split(/\s+/).filter(Boolean));

const C_LIKE = "if else for while do return break continue switch case default new delete true false null this";
const LANGUAGES: Record<string, Language> = {
  rust: {
    name: "Rust",
    line: ["//"],
    block: ["/*", "*/"],
    strings: ['"'],
    keywords: words(
      "as async await break const continue crate dyn else enum extern false fn for if impl in let loop match mod move mut pub ref return self Self static struct super trait true type unsafe use where while Some None Ok Err",
    ),
  },
  typescript: {
    name: "TypeScript",
    line: ["//"],
    block: ["/*", "*/"],
    strings: ['"', "'", "`"],
    keywords: words(
      `${C_LIKE} async await class const enum export extends from function import in instanceof interface let of private protected public readonly static throw try catch finally type typeof undefined var void yield`,
    ),
  },
  python: {
    name: "Python",
    line: ["#"],
    strings: ['"', "'"],
    keywords: words(
      "and as assert async await break class continue def del elif else except False finally for from global if import in is lambda None nonlocal not or pass raise return True try while with yield self",
    ),
  },
  go: {
    name: "Go",
    line: ["//"],
    block: ["/*", "*/"],
    strings: ['"', "`"],
    keywords: words(
      "break case chan const continue default defer else fallthrough for func go goto if import interface map package range return select struct switch type var nil true false",
    ),
  },
  c: {
    name: "C",
    line: ["//"],
    block: ["/*", "*/"],
    strings: ['"', "'"],
    keywords: words(`${C_LIKE} int char void struct typedef static const unsigned long short float double include define`),
  },
  shell: {
    name: "Shell",
    line: ["#"],
    strings: ['"', "'"],
    keywords: words("if then else elif fi for in do done while case esac function return export local set echo exit"),
  },
  toml: { name: "TOML", line: ["#"], strings: ['"', "'"], keywords: words("true false") },
  yaml: { name: "YAML", line: ["#"], strings: ['"', "'"], keywords: words("true false null yes no") },
  json: { name: "JSON", strings: ['"'], keywords: words("true false null") },
  css: { name: "CSS", block: ["/*", "*/"], strings: ['"', "'"], keywords: words("important media supports from to") },
  markdown: { name: "Markdown", strings: [], keywords: new Set() },
  text: { name: "Plain text", strings: [], keywords: new Set() },
};

const BY_EXTENSION: Record<string, string> = {
  rs: "rust",
  ts: "typescript",
  tsx: "typescript",
  js: "typescript",
  jsx: "typescript",
  mjs: "typescript",
  py: "python",
  go: "go",
  c: "c",
  h: "c",
  cc: "c",
  cpp: "c",
  hpp: "c",
  java: "c",
  sh: "shell",
  bash: "shell",
  zsh: "shell",
  toml: "toml",
  yml: "yaml",
  yaml: "yaml",
  json: "json",
  jsonc: "json",
  css: "css",
  md: "markdown",
  markdown: "markdown",
};

export function languageOf(path: string): Language {
  const name = path.split("/").pop() ?? "";
  if (name === "Dockerfile" || name === "Makefile") return LANGUAGES.shell;
  const ext = name.includes(".") ? name.split(".").pop()!.toLowerCase() : "";
  return LANGUAGES[BY_EXTENSION[ext] ?? "text"];
}

export function escapeHtml(text: string): string {
  return text.replace(/[&<>"']/g, (c) => `&${{ "&": "amp", "<": "lt", ">": "gt", '"': "quot", "'": "#39" }[c]};`);
}

type Token = { cls: string | null; text: string };

/** Tokens for the whole text, in order. */
function tokenize(text: string, lang: Language): Token[] {
  const out: Token[] = [];
  let plain = "";
  const flush = () => {
    if (plain) out.push({ cls: null, text: plain });
    plain = "";
  };
  let i = 0;
  while (i < text.length) {
    const rest = text.startsWith.bind(text);
    const lineMark = lang.line?.find((mark) => rest(mark, i));
    if (lineMark) {
      flush();
      const end = text.indexOf("\n", i);
      const stop = end === -1 ? text.length : end;
      out.push({ cls: "c", text: text.slice(i, stop) });
      i = stop;
      continue;
    }
    if (lang.block && rest(lang.block[0], i)) {
      flush();
      const end = text.indexOf(lang.block[1], i + lang.block[0].length);
      const stop = end === -1 ? text.length : end + lang.block[1].length;
      out.push({ cls: "c", text: text.slice(i, stop) });
      i = stop;
      continue;
    }
    const ch = text[i];
    if (lang.strings.includes(ch)) {
      flush();
      let j = i + 1;
      while (j < text.length && text[j] !== ch && (ch === "`" || text[j] !== "\n")) j += text[j] === "\\" ? 2 : 1;
      out.push({ cls: "s", text: text.slice(i, Math.min(j + 1, text.length)) });
      i = j + 1;
      continue;
    }
    const word = /^[A-Za-z_][A-Za-z0-9_]*/.exec(text.slice(i, i + 64));
    if (word) {
      if (lang.keywords.has(word[0])) {
        flush();
        out.push({ cls: "k", text: word[0] });
      } else plain += word[0];
      i += word[0].length;
      continue;
    }
    const number = /^\d[\d_]*(\.\d+)?/.exec(text.slice(i, i + 64));
    if (number && lang.keywords.size) {
      flush();
      out.push({ cls: "n", text: number[0] });
      i += number[0].length;
      continue;
    }
    plain += ch;
    i++;
  }
  flush();
  return out;
}

/** Escaped, highlighted HTML for each line of `text`. */
export function highlightLines(text: string, path: string): string[] {
  const lines: string[] = [""];
  for (const token of tokenize(text, languageOf(path))) {
    token.text.split("\n").forEach((piece, i) => {
      if (i > 0) lines.push("");
      if (!piece) return;
      const html = escapeHtml(piece);
      lines[lines.length - 1] += token.cls ? `<span class="t-${token.cls}">${html}</span>` : html;
    });
  }
  if (text.endsWith("\n")) lines.pop();
  return lines;
}
