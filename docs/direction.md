# Javelin direction

Status: accepted by the user on 2026-10-26 in the hub thread. This amends
`build-contract.md`, `online-design.md` and `git3-direction.md`. Where they conflict with
this document, this document wins.

## What Javelin is

Git is second-generation version control. It came after centralized systems such as CVS
and Subversion, and it was built for people who work offline and each hold the full
history. Javelin is the third generation, built from scratch for agentic development.

Javelin has two products:

- The `javelin` CLI replaces Git.
- javelin.codes, the forge, replaces github.com.

Migrating from Git is out of scope for now. There is no import, export or way to run Git
and Javelin side by side, and nothing in the design should be shaped by one.

## Forge and node

javelin.codes is the forge. It is everything a person sees and does: accounts,
organizations, members, finding repositories, browsing code and history, Publish Requests
and review, settings, and connecting a node. Javelin runs the forge. Older documents call
it the central control plane. Nobody self-hosts it.

A node is where one repository lives and where its compute runs. It holds the World log,
objects, Layers, review state, checks and applied permissions. People never visit a node.
The CLI and the forge's pages in the browser talk to it directly.

Javelin runs no managed hosting. The repository owner brings the infrastructure, in one of
two ways:

- Connect a provider account from the forge. Cloudflare is the first provider. The forge
  provisions the node into the owner's account through the provider's OAuth consent.
- Run `javelin-node` on a machine the owner controls, and register its URL with the forge.

Javelin never pays for, holds or reads a repository's storage or compute.

### What a node needs

The node's design comes from these needs, not from one provider. A provider is acceptable
if an adapter can meet them.

1. One ordering point per repository. Every World advance goes through it, one at a time.
2. Durable storage for the log and for content-addressed objects. An acknowledged write
   survives a restart.
3. Every object is checked against its hash before a reference to it commits.
4. An idle repository uses no compute.
5. A public HTTPS endpoint that the CLI and browsers reach directly.
6. Bounded compute for compose, Stack and conflict checks.
7. Heavier compute (checks, headless Layers, agent runs) goes to a separate runner the
   owner connects. It is designed together with CI, later.
8. A normal repository fits the provider's free tier. The node keeps a budget per provider
   limit, and refuses a write that would cross it with `quota_exceeded` instead of causing
   a charge. Only the owner can raise the budget.

Cloudflare meets needs 1 to 6 with a Worker and one SQLite-backed Durable Object per
repository. R2 may hold large objects, inside its free tier and under the budget. The
self-hosted `javelin-node` meets them on one machine, with local fsync and no replica.

The World's durability is the provider's: Cloudflare replicates Durable Object storage,
and a self-hosted node has the durability of its one disk. This replaces the contract's
"quorum-replicated log" wording.

The node core is one Rust implementation. Each provider is an adapter behind its storage
and coordination traits. The TypeScript Worker is a stopgap until the core builds for wasm.

## Publishing

Direct Publish and Publish Requests are both first-class. A repository setting picks the
default (D12: `direct` for private, `requests` for public). A member allowed to Publish
can still open a Publish Request for any change. Publishing as soon as required checks
pass is direct Publish with required checks, and it waits for the CI design.

## Superposition: who sees what, and when

The problem. In open source a security fix is public the moment it lands, before users
have updated, and an agent can turn a public fix into an exploit in minutes. Some files
also belong in the repository without being public: environment and config files, internal
notes, unreleased work.

The model. A repository has one World and one history. Every reader sees the same
versions with the same IDs, and their audience decides what they can open. Nothing is
rewritten when content becomes public.

There are three controls.

1. **Path audiences.** A repository policy maps path patterns to audiences: `public`,
   `members`, `maintainers` or a named group. A reader outside the audience never receives
   that content. For example, `.env*` and `config/prod/**` are readable by `maintainers`
   while `src/**` is public.
2. **Disclosure delay.** A repository setting. Public readers see the World as it was a
   fixed time ago, for example 7 days. Members see the head. Each version becomes public
   on its own once the delay passes. The delay is the same for every version, so a pause
   in public updates says nothing about any one change.
3. **Embargo.** A Publish can carry an embargo with a release condition: a time, or a
   maintainer's release action. Public readers stay below an embargoed version until it is
   released, and then every waiting version becomes public at once. Use it when a fix
   needs longer than the delay.

The public frontier is the newest version that is older than the delay and has no
unreleased embargo at or below it. Public readers attach, fetch, browse and fork at the
frontier. They do not see version numbers above it. The forge shows the same frontier to
anyone outside the repository.

### Rules this sets now

These change the storage format, so they are fixed before any real repository is stored.

- A tree entry for content outside the public audience holds a salted commitment,
  `H(salt, content id)`, and the audience, instead of the plain content id. Only the
  audience receives the salt and the content id. A reader without access can still verify
  the tree root and the version chain, but cannot confirm a guess of the hidden content.
- Changesets use the same form. A changed hidden path appears in the changeset with its
  commitment and audience, never its plain content id, and the version ID chain hashes
  that committed form. Changes inside a hidden directory appear as one entry for that
  directory, so file names inside it stay hidden too. A reader can then verify every
  version it can see without learning anything that would confirm a guess.
- Tree roots and version IDs are the same for every reader. Disclosing a path serves its
  salt and content. It never changes an ID.
- The policy lives in the World as a file that every reader can see, so it is versioned
  and published like code. Only Maintain can change it, and the node checks that at
  Publish. A policy change applies from the version that makes it. Content that was
  already public stays public.
- A writer outside an audience cannot change its paths. Their Layer does not contain
  those paths, and a changeset never treats an unseen path as deleted. Compose leaves
  unseen paths unchanged.
- A conflict with a change the author cannot see is reported without its content or path
  ("conflicts with a change that is not public yet").
- Agent traces use the same audiences. D25's restricted trace is a trace whose audience is
  `maintainers`.
- A fork receives only what the forker can read. Hidden entries keep their commitments, so
  the fork's version IDs still match the source.

Hidden content is still plaintext on the owner's node, which the owner controls. Secret
values (tokens, keys) later get an entry kind encrypted to the audience members' device
keys, so the node and the provider store only ciphertext. That is not in the first slice.

## Layers follow their owner (replaces D21)

A Layer belongs to its owner, not to a machine. Checkpoints upload as they are captured,
so the node already has the Layer.

- One device writes to a Layer at a time. The node holds a writer lease per Layer.
- `javelin layer take <layer>` on another device of the same owner moves the lease. The new
  device materializes the Layer from the node's head and continues.
- The old device's checkpoints that were not uploaded yet are refused with `layer_stale`
  and stay in its outbox. `javelin layer clone` turns them into a new Layer. Nothing is
  lost, and no write is forced.
- The owner's agents and subagents use the owning device's lease (D26).

## CI

Open. The user is still designing it. Until then D4 stands: external runners post check
results, and no check gates Publish by default. U17 (check ingestion) and U18 (workflow
validator) stay as they are and get no new work. GitHub Actions YAML (CI1) is not a
commitment. The Dagger and Bend discussion is input for that design:
`t3-thread://v1/mcp:59dd2dca-4d85-4237-9c45-fdf093042136`.

## Sources

- The user's answers on 2026-10-26 in the hub thread,
  `t3-thread://v1/76efa593-7e25-4ae8-ad18-70811fa8df90`.
- Catch-up on what changed after Scion:
  `t3-thread://v1/mcp:cd92572d-7125-49de-9439-f60d644e2e20`.
