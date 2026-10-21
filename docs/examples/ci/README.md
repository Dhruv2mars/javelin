# CI workflow examples

Fixtures for the workflow-syntax validator and the check-report path in `docs/ci-execution.md`.
They use GitHub Actions workflow YAML, the working reading of the contract's "CI workflow syntax
compatibility" (see `docs/ci-execution.md` §1). Nothing here runs yet.

The `javelin revision fetch` and `javelin check report` commands are planned, not implemented.
Each file parses as YAML (checked with `Bun.YAML` in bun 1.3.13; that parser keeps the last of duplicate keys, so the validator uses `yaml` v2 instead, see `docs/ci-execution.md` §10). None has been run through GitHub.

| File | Purpose | Expected validator result |
| --- | --- | --- |
| `01-minimal.yml` | One job, one `run` step, no Javelin commands | valid |
| `02-fetch-by-tree.yml` | Fetch a revision by root tree hash into an empty directory, then test | valid |
| `03-report-check.yml` | Report `ci/unit` with `if: always()`, using the step outcome | valid |
| `04-matrix.yml` | Three-OS matrix; `fail-fast: false` | valid |
| `05-needs-outputs.yml` | `needs`, job `outputs`, conditional `if` | valid |
| `06-defaults-concurrency.yml` | Top-level `permissions`, `env`, `defaults`, `concurrency` | valid |
| `07-pinned-uses.yml` | Local `./` action, third-party action pinned to a full SHA (placeholder) | valid |
| `08-invalid-unknown-key.yml` | Typo `runson` | invalid: `unknown_key` at `jobs.test.runson` |
| `09-invalid-checkout.yml` | `actions/checkout`, which cannot fetch a Javelin revision | invalid: `git_checkout` at `jobs.test.steps[0].uses` |
| `10-invalid-duplicate-key.yml` | Duplicate `runs-on` key; `Bun.YAML` would keep the last value | invalid: `yaml_error` |

Files 08, 09 and 10 are invalid by design. Their first comment line states the expected code.
The validator PR should assert the exact code and path for each.
