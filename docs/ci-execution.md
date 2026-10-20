# CI execution design (U15)

Status: proposed design, not accepted. Written 2026-10-20 on `docs/ci-execution`, stacked on
`docs/online-design` (PR #58). Docs only. Nothing here is implemented or run.
It resolves the TASKS.md item "Resolve CI execution design and verify the accepted workflow
syntax" only as a design. The syntax part is not verified (see §1).

Source tags follow `docs/online-design.md`: `C§` = `docs/build-contract.md`, `G§` =
`docs/git3-direction.md`, `O§` = `docs/online-design.md`. Decision D4 is `O§7`.

## 0. Summary

- **Default (D4 kept):** no Javelin-run runners. External CI (GitHub Actions or any CI) runs
  jobs and posts completed results to the node with a scoped token. The node stores them with the
  reporter's identity. No check gates Publish unless the repository policy names it.
- **Subject:** a check result is about a tree hash. Revisions and Layer checkpoints point at trees.
- **Trust:** the node authenticates the reporter's token and binds it to a check-name pattern.
  No per-result signatures in the first slice.
- **Fetch:** a runner gets the revision by root tree hash with existing `Get`/`Objects` messages,
  verifying each object. No Git, no attach, no packfile.
- **Outage:** checks are not queued. Reporters retry with the same operation ID. A queued Publish
  re-checks required checks at replay.
- **First slice:** `CheckReport` ingestion plus a required-check gate at Publish (U17, proposed,
  §11). Its tests are in §11.

## 1. Accepted workflow syntax

The contract's entire statement, `C§Protocol and first provider`, last paragraph:

> CI workflow syntax compatibility is accepted. CI execution remains to be designed
> and verified. The broad forge wishlist is later scope, not a reason to delay proving
> the local and hosted development workflows.

**Gap and assumption.** The contract accepts "CI workflow syntax compatibility" without naming a
dialect or listing constructs. No other doc in the repo names one. I searched `docs/`, `TASKS.md`,
`AGENTS.md` and the history of `docs/build-contract.md` (introduced in `0366a1b`, unchanged since).
The only related wording is in `TASKS.md`, which pairs "CI" with "the accepted workflow syntax".

**Working reading (unconfirmed, for the hub):** GitHub Actions workflow YAML, because the repo
already uses `.github/workflows/*.yml` and "workflow" is GitHub's term. The validator spec (§10)
targets this reading. If the hub means another dialect, §10 must be rewritten.

**What GitHub's docs say** (fetched 2026-10-20 from
`docs.github.com/en/actions/using-workflows/workflow-syntax-for-github-actions`):

- Top-level keys: `name`, `run-name`, `on`, `permissions`, `env`, `defaults`, `concurrency`,
  `cache-mode`, `jobs`.
- Job keys seen: `name`, `permissions`, `concurrency`, `cache-mode`, `needs`, `runs-on`,
  `outputs`, `secrets`, `env`, `defaults`. Not seen in the fetched text: `strategy`, `if`,
  `steps`, `timeout-minutes`, `continue-on-error`, `container`, `services`. They are in GitHub's
  syntax and are assumed, not confirmed here.
- Step keys: `id`, `if`, `name`, `uses`, `run`, `working-directory`, `shell`, `with`, `env`,
  `continue-on-error`, `timeout-minutes` (max 360), `background`, `wait`, `wait-all`, `cancel`,
  `parallel`. The last five are listed by the page; their semantics are not checked.
- `workflow_dispatch` accepts `inputs`. It "only receives events when the workflow file is on the
  default branch."
- Pinning: GitHub "strongly recommend[s]" a full commit SHA. It does not require one. Javelin's
  policy (§10) requires one.

## 2. Conflicts with the contract or D4

**No conflict with D4.** D4's default fits the contract: `C§Version control` makes "tests, builds,
reviews, and security checks ... optional repository policy, not mandatory default Publish
behavior", and `C§Hosting ownership` puts "checks, policy" on the node.

Flags (the hub decides; none contradicts D4):

1. **Dialect unnamed** (§1). The contract accepts syntax compatibility without naming it.
2. **Workflow files cannot run from a Javelin tree directly under external CI.** GitHub reads
   workflows from its own repository's default branch. A Javelin revision's
   `.github/workflows/*.yml` therefore does not run there. The file is either copied into the
   external CI repository, or a later Javelin-hosted runner reads it from the revision. Either
   way the syntax has to be parsed the same way.
3. **`actions/checkout` cannot work for a Javelin repository.** A job that uses it fetches a Git
   repository, which a Javelin revision is not. `C§Version control` and `AGENTS.md` forbid Javelin
   runtime and user workflows from invoking Git. Examples use `javelin revision fetch` (§6).
   Test `09` checks this.
4. **Event delivery for external runners is undesigned.** `O§7 D27` defers notifications: "Clients
   poll `LayerHead` and `ReadRequest`." No design covers how an external CI learns that a revision
   needs checking. Until one exists, an external runner must poll or be triggered by the owner.
   This is a gap, not a contradiction.
5. **Provider compute for Javelin-hosted runners is unconfirmed** (`O§7 D4`, §5). Not a conflict,
   but it blocks that option.

## 3. How checks attach to a revision

A check result is keyed by `(repository, subject_tree, check_name)`. Subject is a tree root hash
(`G§2`: "one root hash identifies a version's full content").

- A Publish Request revision `n` has `checkpoint`, and the checkpoint names a tree. Checks for `n`
  are checks for that tree. A reporter sends the tree hash, and the node also records the checkpoint
  when it is supplied.
- A direct `PublishLayer` uses the Layer's `expected` checkpoint, and its tree is the subject.
- Two revisions with the same tree share results. That is correct, because the content is identical.
- Results do not attach to a Layer id, a request id or a base version. A result for tree `T` remains
  valid for any request or Layer whose source tree is `T`. This matches `O§7 D10`: approvals survive
  non-intersecting World advances, because they bind content, not base.

**Immutability.** Once a result is stored it is never edited (`C§Contributions and reviews`:
"Each submitted revision is immutable"). A re-run is a new result with a new id. Policy reads the
latest completed result per `(subject_tree, check_name, reporter)`.

## 4. Check-result ingestion

**Wire.** Proposed tag `0x60` `CheckReport`, reply `0x61` `CheckOutcome`. Neither tag appears in
`O§3` or PR #52's list. Confirm against `codec.rs` before U17.

```
CheckReport {
  operation: OperationId,        // idempotency key; reporter retries reuse it
  repository: RepositoryId,
  subject_tree: TreeId,          // required
  subject_checkpoint: Option<CheckpointId>,  // if given, node verifies its tree == subject_tree
  name: String,                  // e.g. "ci/unit"; must match the token's pattern
  conclusion: Success | Failure | Neutral | Cancelled | TimedOut | Skipped,
  details_url: Option<String>,   // <= 2 KiB
  summary: Option<String>,       // <= 64 KiB
  started_at, completed_at: Timestamp
}
CheckOutcome = Accepted { id, seq } | Refused { code }
```

- Slice 1 accepts only completed results. The `conclusion` set is the GitHub check-run set
  ("Allowed conclusion values" fetched 2026-10-20 from `docs.github.com/en/rest/checks/runs`). `stale`
  is excluded because only GitHub may set it. `ActionRequired` is excluded in slice 1.
- Replay of the same `operation` returns the original `Accepted` with the original `id`
  (`O§3` replay rule).
- The node checks the subject: unknown tree is refused (`missing_object`, since the tree is absent);
  a checkpoint whose tree differs is refused `tree_mismatch`.

**Error codes (new):** `tree_mismatch`, `check_name_not_allowed`, `check_name_invalid`
(`name` must be `^[a-z0-9_.-]+(/[a-z0-9_.()-]+)*$`). Existing: `unauthorized`, `forbidden`,
`operation_id_reused`, `stale_permissions`.

### 4.1 Scopes and reporter identity

- **Scope:** `checks:report:<pattern>`, a token-scope string. The pattern is a glob over check names,
  e.g. `checks:report:ci/*`. Repository policy grants patterns to a reporter. The token carries the
  pattern, and the node enforces it (`check_name_not_allowed`).
- **Reserved names:** `approval/*` and `publish/*` are never reportable by external tokens. A token
  with `checks:report:*` cannot match them: the reserved names are excluded before patterns are
  applied.
- **Reporter identity:** the token's device or service identity (`O§7 D26` is the agent-identity
  default; a CI reporter is a service identity, decided in U7). It is stored with each result.
- **Required checks name a reporter** (§7). A result from another identity does not satisfy the
  requirement, even if the name matches. This blocks any member from forging a green `ci/unit`.

### 4.2 Signatures (decision)

**Default: no per-result signatures in slice 1.** The node is the only verifier. Trust rests on the
node authenticating the token, binding the token to a name pattern, and recording the identity.
Signing adds value only when a result must be verifiable outside the node or relayed by a party the
node does not trust. Neither happens in slice 1. Revisit with Javelin-hosted runners or third-party
attestations. The GitHub model also uses an authenticated App identity, not per-result signatures
(`docs.github.com/en/rest/checks/runs`: write access "only available to GitHub Apps").

## 5. Runner options

| Option | What it is | Status |
| --- | --- | --- |
| None | No checks. Publish is governed by permissions and VCS correctness only. | Always available. Default for repositories with no policy. |
| External runners (default) | GitHub Actions or any CI runs jobs, fetches by tree hash (§6), posts results (§4). | This design. Needs U7 tokens and U17 ingestion. |
| Javelin-hosted runners | Runners on the first provider, reading workflow files from the revision. | Later. Not designed. Compute availability is unconfirmed: Cloudflare Containers search excerpts (fetched 2026-10-20 via WebSearch, not primary docs) describe a beta with per-instance limits and a February 2026 limits increase; a preview docs mirror gives custom limits of 1–4 vCPU, 12 GiB memory, 20 GB disk. None is verified against `developers.cloudflare.com/containers`. Do not depend on this. |

Hosted runners would also need the revision fetch of §6 and a run-time credential. Neither is
designed beyond that.

## 6. How a runner fetches a revision without Git

Runners get a revision by root tree hash. They do not attach (no rolling base, no full local history).

**Path.** Existing messages suffice:

1. `Get` the root tree object by id (`0x12`) → `Objects` (`0x20`).
2. For each tree, `Want` the child tree and blob ids the runner lacks (`0x10`); `Missing` (`0x11`)
   reports what the node lacks. Fetch by `Get`/`Objects`.
3. Verify each object's id before writing it. Object id is SHA-256 over kind and bytes (`O§3`).
   A mismatch aborts with `id_mismatch`.
4. Materialize into an empty directory. Files and directories are written from the verified trees.

This follows `G§2` and `C§Protocol`: "trees first, then the blobs a client wants, with no packfiles."

**Auth.** Anonymous read is allowed for public repositories (`O§7 D24`). Private repositories need a
read token. Runner tokens use the access TTL of `O§7 D6` (1 hour); a longer job refreshes through the
control plane.

**CLI (planned).** `javelin revision fetch --repo <r> --tree <hash> --into <dir>`. It runs the path
above and nothing else. It never invokes Git (`C§Verification gates` checks this with a failing `git`
on PATH).

**Why not attach.** Attach (`O§4.1`) copies the full rolling base (`O§7 D19`). That is correct for a
developer's machine. It costs too much for a single CI job.

## 7. Required-check policy at Publish

- **Policy (node-owned, `C§Hosting ownership` "policy").** `required_checks: [{ name, reporter }]`,
  `reporter` being an identity or `any-allowed`. **Default: empty** (`O§7 D4`: no check gates Publish
  by default).
- **Boundary check (`O§4.8`, extended).** For each required check, the node looks up the latest
  completed result for the subject tree with that name from an allowed reporter.
  - No result → refuse `checks_missing`.
  - Latest result not `success` → refuse `checks_failed`.
  - `Neutral` and `Skipped` do not satisfy a requirement in slice 1.
  The check runs at the same boundary as `not_enough_approvals`, so an approval does not stand in for
  a check.
- **Subject.** The source tree of the revision or Layer checkpoint (§3). Consequence, flagged: a
  non-intersecting World advance can compose to a tree that differs from the tested source. Tests
  cover the source tree only. Gating the composed tree needs a second result per base. That is out of
  slice 1 (open, §9).
- **Policy snapshot.** The policy in force at publish time, like approvals (`O§4.8`). A policy change
  is an applied-permission change and follows epoch rules (`stale_permissions`).
- **Override.** None in slice 1. The owner changes the policy instead. An override would be new product
  behavior, and the contract does not name one for checks (it names one only for agent traces).
- **Publish paths.** `PublishLayer` (direct) and `PublishRequest` both run the check.

## 8. Outage behavior

Matches `O§5` and `O§7`:

| Event | Behavior |
| --- | --- |
| Node down, reporter posts `CheckReport` | Fails client-side as `node_unavailable`. Not queued (D7 list excludes checks). The reporter retries with the same `operation`. Replay returns the original result (§4), so no duplicate. |
| Control plane down | Posting works while the token is valid (`C§Hosting ownership`). Refresh fails. |
| Token expires mid-job, control plane down | Post refused `unauthorized` after expiry. The result is missing, so a required check refuses Publish with `checks_missing`. The owner reruns once the token is refreshed. |
| `PublishLayer` queued during a node outage | Queued (D7). At replay, required checks are re-evaluated. Missing ones refuse with `checks_missing`. That is one durable outcome. The owner reruns Publish after the check completes. |
| Lost reply on `CheckReport` | Same `operation` on retry returns the original `Accepted`. |
| Provider outage | Checks cannot be posted. Publish queues. Local work continues (`C§Hosting ownership`). |

Checks are not added to the D7 queue list. The reporter already retries with the same `operation`, so a
node-side queue would add state without adding safety. Adding checks to the queue would change D7, so
the hub decides (CI4).

## 9. Open decisions (defaults above)

- **CI1. Dialect.** GitHub Actions YAML (§1). Hub to confirm.
- **CI2. Composed-tree gating.** Default: source tree only (§7). Alternative: second result per base.
- **CI3. Override for required checks.** Default: none (§7).
- **CI4. Checks in the D7 queue.** Default: not queued (§8).
- **CI5. Per-result signatures.** Default: none (§4.2).
- **CI6. Event delivery to external CI.** Default: none designed (§2 flag 4). Needs a decision before
  external runners can be triggered by a Javelin event.
- **CI7. Javelin-hosted runners.** Default: not designed (§5).

## 10. Validator spec (workflow syntax)

A later PR builds it, as a bun script under `tools/ci-validate/` (JS tooling uses bun, `CLAUDE.md`).
It parses with `Bun.YAML.parse` (bun 1.3.13 has it; checked locally 2026-10-20). Duplicate keys are a
parse error.

**Interface.** `bun tools/ci-validate/validate.ts <file.yml>...`. Output: one JSON line per finding,
`{ "file", "path", "code", "message" }`. Exit `0` valid, `1` invalid, `2` usage or unreadable input.

**Limits.** 256 KiB per file, 256 jobs, 64 steps per job (`too_large`). These are Javelin's choices.

**Rules.** Each rule has one code. Each code gets a fixture.

| Code | Rule |
| --- | --- |
| `yaml_error` | YAML does not parse, or has duplicate keys. |
| `unknown_key` | Key not in GitHub's syntax as fetched (§1). |
| `unsupported_key` | Key GitHub documents but Javelin slice 1 does not accept: top-level `cache-mode`; job `cache-mode`, `secrets`, `container`, `services`; reusable-workflow `uses`/`with` at job level; step `background`, `wait`, `wait-all`, `cancel`, `parallel`. |
| `missing_required` | No `on`, no `jobs`, or a job without `runs-on` (unless `uses`), or a job without `steps`. |
| `bad_job_id` | Job id not `^[A-Za-z_][A-Za-z0-9_-]*$`. |
| `bad_needs` | `needs` names a missing job, or the graph has a cycle. |
| `bad_step` | Step has both or neither of `run` and `uses`. |
| `bad_value` | `timeout-minutes` not a positive integer ≤ 360. |
| `bad_expression` | Unbalanced `${{` / `}}`. |
| `git_checkout` | `uses` starts with `actions/checkout`. Any ref. |
| `git_invocation` | A `run` line invokes `git` as a command (`(^|[;&|(])\s*git(\s|$)`). |
| `unpinned_action` | Third-party `uses: owner/repo@ref` where `ref` is not a 40-hex SHA. Javelin policy; GitHub only recommends it (§1). |
| `unsupported_uses` | `docker://` action, or `uses` that escapes the revision (`../`). |

**Fixtures.** Files `01`–`07` valid; `08` expects `unknown_key` at `jobs.test.runson`; `09` expects
`git_checkout` at `jobs.test.steps[0].uses`. Plus one minimal fixture per other code.

**Not in this spec.** Expression semantics, `if` evaluation, action metadata resolution, and any
runner behavior.

## 11. Smallest first implementation slice (U17, proposed)

Proposed unit, not in the `O§8` table. The hub adds it. Depends on U2 (`PublishLayer`) and U7 (token
scopes). It is not U15's job to build it.

**Owned paths:** `crates/javelin-node/src/checks.rs` (new), `crates/javelin-protocol/src/codec.rs`
(tags `0x60`, `0x61`), `crates/javelin-cli` (`check report` subcommand only), `tests/checks_*.rs`.

**In scope:** `CheckReport` ingestion for completed results; name-pattern scope; reporter identity;
tree binding; required-check gate in `PublishLayer` and `PublishRequest`; `javelin check report`.

**Out of scope:** runners, `revision fetch` (its own unit, after U4 or with it), in-progress updates,
signatures, UI, notifications, override, composed-tree gating.

**Tests (each named as a test):**

1. `accepts_completed_result_for_tree`: stored; `id` returned.
2. `replay_same_operation_returns_original`: lost reply, same `operation`, same `id`, one stored record.
3. `rejects_tree_mismatch`: checkpoint tree ≠ `subject_tree` → `tree_mismatch`.
4. `rejects_unknown_tree`: absent tree → `missing_object`.
5. `token_pattern_limits_names`: `checks:report:ci/*` posting `approval/x` or `publish/x` → `check_name_not_allowed` /
   `forbidden`.
6. `stored_result_is_immutable`: a second report with the same name makes a new `id`; the first record is
   byte-identical.
7. `publish_refuses_missing_required_check`: `checks_missing`.
8. `publish_refuses_failed_required_check`: `checks_failed`.
9. `publish_accepts_success_for_source_tree`: `Published`.
10. `result_for_other_tree_does_not_count`.
11. `nonintersecting_advance_keeps_result_valid`: World moves; source tree still satisfies the requirement (D10).
12. `wrong_reporter_does_not_satisfy_requirement`: same name, different identity → `checks_missing`.
13. `empty_policy_leaves_publish_unchanged`: regression for the default.
14. `reporter_revoked_before_publish_is_refused`: epoch bump → `stale_permissions`.
15. `queued_publish_rechecks_checks_at_replay`: outage replay with missing check → `checks_missing`.
16. `fetch_verifies_each_object_and_rejects_tampering`: (fetch unit) a modified blob → `id_mismatch`, nothing written.
17. `validator_fixtures_match_readme`: (validator PR) `01`–`09` results match `docs/examples/ci/README.md`.

Acceptance for U17: items 1–15 pass under `cargo test --locked --workspace --all-targets --all-features`,
with a failing `git` on PATH. Item 16 belongs to the fetch unit, and item 17 to the validator PR.

## 12. Sources and what is unverified

- **Contract text:** `docs/build-contract.md`, sentence introduced in commit `0366a1b` and unchanged at HEAD. Quoted in §1.
- **Repo search:** `docs/`, `TASKS.md`, `AGENTS.md`, and `git log -S` for "CI workflow syntax compatibility"
  and "workflow syntax" across all branches. No dialect named anywhere.
- **GitHub workflow syntax** (fetched 2026-10-20, two pages of a long document): keys in §1. Not read: the
  rest of the text past the step keys, and the job keys `strategy`, `if`, `steps`, `timeout-minutes`,
  `continue-on-error`, `container`, `services`. **Unverified** beyond the fetched text.
- **GitHub check runs REST** (`docs.github.com/en/rest/checks/runs`, fetched 2026-10-20): App-only write; the
  conclusion and status sets in §4. **Unverified:** whether a completed run can be updated (the page did not
  say).
- **Cloudflare Containers** (WebSearch, 2026-10-20, search excerpts only; not a primary docs page): limits in
  §5. **UNCONFIRMED.**
- **Bun YAML:** `Bun.YAML.parse` exists in bun 1.3.13 (local check). Parsing of the examples passed.
- **Not checked:** GitHub's `workflow_dispatch` input limits; Cloudflare Workers compute for runners; any
  external CI other than GitHub Actions.

## 13. Examples

`docs/examples/ci/` holds nine workflow files and a table of expected validator results (`README.md`).
Files `01`–`07` are valid; `08` and `09` are invalid by design.
