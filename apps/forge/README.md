# Forge (read-only pages)

The first pages of javelin.codes, live at https://javelin-codes.vercel.app until the domain is
connected. The browser reads a repository's node directly over
protocol v2 (`POST {node}/v2/r/{repository}`), with no Javelin-run server in between
(`docs/build-contract.md`, Hosting ownership). Public repositories need no account (D24).

| Page | URL |
| --- | --- |
| Landing: what Javelin is, install status, open a repository by node URL and ID | `/` |
| Repository home: files at the head, README rendered | `/OWNER/REPO` |
| Directory at a version | `/OWNER/REPO/tree/N/PATH` |
| File at a version (highlighted text, notice for binary) | `/OWNER/REPO/file/N/PATH` |
| History, newest first: number, ID, attribution, changed paths | `/OWNER/REPO/history` |
| Version: its changeset as a diff against the parent | `/OWNER/REPO/version/N` |

The forge checks every object it reads against its ID, and each version's ID against its
parent's ID, root and changeset, so it trusts the node for availability, not content.

## Which node

There is no control-plane lookup yet. The forge finds a repository's node from, in order:

1. `?node=http://127.0.0.1:7878&repository=ID` on the URL (remembered in this browser),
2. `forge.config.json` beside the assets:
   `{"repositories": {"owner/repo": {"node": "http://127.0.0.1:7878", "repository": "ID"}}}`.

The self-hosted node allows any origin on read messages (`Resolve`, `Get`, `Changeset`,
`Attribution`, `ReadDisclosure`) and refuses every other message that carries an `Origin` header.

The forge reads signed out, so the node shows it a repository at its public frontier
(`docs/direction.md`, Superposition): versions held by the disclosure delay or an embargo
do not exist for it. When the repository has a delay, its home page says the public view is
delayed, never by how much any one change waits.

## Commands

    bun install
    bun run dev          # builds javelin + javelin-node, seeds a demo repo, starts the node
                         # and the forge; open http://127.0.0.1:5178/demo/lantern
    bun test             # codec, and page data against a real `javelin-node serve`
    bun run typecheck
    bun run build        # static assets in dist/

`bun run dev` takes `FORGE_PORT`, `NODE_PORT`, and `FORGE_REPO` (serve an existing repository
directory instead of the demo). Tests and `dev` build the binaries with cargo unless
`JAVELIN_BIN_DIR` names a directory that holds them. The node refuses anonymous callers unless
the repository is public, so a directory with no `permissions.json` gets a public one, which
the forge reads without a token.

## Deploy

Vercel serves the static build from the `javelin-codes` project on the Hobby plan. Nothing
deploys on push. From a clean `main` checkout, logged in to the `dhruv2mars` Vercel account,
run:

    bun run deploy

`scripts/deploy.ts` runs `vercel pull`, `vercel build --prod` and `vercel deploy --prebuilt
--prod`, so the build runs locally and only its output is uploaded. `vercel.json` holds the
settings: bun install and build, `dist/` as output, index.html for every page route so direct
loads and refreshes work, and the security headers. The CSP lets the page fetch any `https:`
node, plus `http://localhost` and `http://127.0.0.1` nodes for local work.

## Layout

- `src/data.ts`: page data loaders, no DOM. `src/app.ts`: routing and rendering.
- `src/landing.ts` and `src/landing.css`: the page at `/` and its open-a-repository form.
- `src/diff.ts` (Myers line diff), `src/highlight.ts`, `src/markdown.ts` (raw HTML escaped,
  unsafe link schemes dropped), `src/routes.ts`.
- `scripts/node.ts`: seeds a repository with the `javelin` CLI and runs `javelin-node serve`.
- The codec is `packages/protocol/`, shared with other TypeScript code.

`marked` is the only runtime dependency.
