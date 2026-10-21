import { expect, test } from "bun:test";
import { readdirSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { validateText, type Finding } from "./validate";

const EXAMPLES = resolve(import.meta.dir, "../../docs/examples/ci");
const VALIDATOR = join(import.meta.dir, "validate.ts");
const VALID = `on: push
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - run: echo hi
`;

async function runCli(args: string[]) {
  const proc = Bun.spawn([process.execPath, VALIDATOR, ...args], { stdout: "pipe", stderr: "pipe" });
  const [stdout, exitCode] = await Promise.all([new Response(proc.stdout).text(), proc.exited]);
  const findings = stdout.split("\n").filter(Boolean).map((line): Finding => JSON.parse(line));
  return { exitCode, findings };
}

// README rows: | `NN-name.yml` | purpose | valid | or | invalid: `code` at `path` |
function readmeExpectations() {
  const rows = readFileSync(join(EXAMPLES, "README.md"), "utf8").split("\n").filter((l) => l.startsWith("| `"));
  return rows.map((row) => {
    const [file, , expected] = row.split("|").slice(1, -1).map((c) => c.trim());
    const name = file.replace(/`/g, "");
    if (expected === "valid") return { name, valid: true as const };
    const m = /invalid: `(\w+)`(?: at `([^`]+)`)?/.exec(expected);
    if (!m) throw new Error(`unparseable README expectation for ${name}: ${expected}`);
    return { name, valid: false as const, code: m[1], path: m[2] };
  });
}

test("validator_fixtures_match_readme", async () => {
  const rows = readmeExpectations();
  const fixtures = readdirSync(EXAMPLES).filter((f) => /^\d\d-.*\.yml$/.test(f)).sort();
  expect(rows.map((r) => r.name)).toEqual(fixtures);

  const { exitCode, findings } = await runCli(fixtures.map((f) => join(EXAMPLES, f)));
  expect(exitCode).toBe(1);
  for (const row of rows) {
    const mine = findings.filter((f) => f.file === join(EXAMPLES, row.name));
    if (row.valid) {
      expect({ file: row.name, findings: mine }).toEqual({ file: row.name, findings: [] });
    } else {
      const got = mine.map((f) => ({ code: f.code, path: f.path }));
      if (row.path === undefined) expect(got.map((f) => f.code)).toContain(row.code);
      else expect({ file: row.name, got }).toMatchObject({ got: expect.arrayContaining([{ code: row.code, path: row.path }]) });
    }
  }
});

test("valid_examples_exit_zero_with_no_output", async () => {
  const valid = readmeExpectations().filter((r) => r.valid).map((r) => join(EXAMPLES, r.name));
  const { exitCode, findings } = await runCli(valid);
  expect(exitCode).toBe(0);
  expect(findings).toEqual([]);
});

test("usage_and_unreadable_input_exit_two", async () => {
  expect((await runCli([])).exitCode).toBe(2);
  expect((await runCli([join(EXAMPLES, "missing.yml")])).exitCode).toBe(2);
});

const bigComment = VALID + "#" + "x".repeat(256 * 1024);
const manyJobs = "on: push\njobs:\n" + Array.from({ length: 257 }, (_, i) => `  j${i}:\n    runs-on: x\n    steps:\n      - run: echo\n`).join("");
const manySteps = "on: push\njobs:\n  test:\n    runs-on: x\n    steps:\n" + "      - run: echo\n".repeat(65);

const cases: [string, string, string][] = [
  ["yaml_error", "", "on: push\non: pull_request\njobs: {}\n"],
  ["too_large", "", bigComment],
  ["too_large", "jobs", manyJobs],
  ["too_large", "jobs.test.steps", manySteps],
  ["missing_required", "on", "jobs:\n  test:\n    runs-on: x\n    steps:\n      - run: echo\n"],
  ["missing_required", "jobs.test", "on: push\njobs:\n  test:\n    steps:\n      - run: echo\n"],
  ["unknown_key", "jobs.test.runson", VALID.replace("runs-on", "runson")],
  ["unsupported_key", "cache-mode", VALID + "cache-mode: x\n"],
  ["unsupported_key", "jobs.test.container", VALID.replace("runs-on", "container: x\n    runs-on")],
  ["bad_job_id", "jobs.1test", VALID.replace("test:", "1test:")],
  ["bad_needs", "jobs.test.needs", VALID.replace("runs-on", "needs: missing\n    runs-on")],
  [
    "bad_needs",
    "jobs.b.needs",
    "on: push\njobs:\n  a:\n    needs: b\n    runs-on: x\n    steps:\n      - run: echo\n  b:\n    needs: a\n    runs-on: x\n    steps:\n      - run: echo\n",
  ],
  ["bad_step", "jobs.test.steps[0]", VALID.replace("- run: echo hi", "- run: echo hi\n        uses: ./x")],
  ["bad_value", "jobs.test.timeout-minutes", VALID.replace("runs-on", "timeout-minutes: 361\n    runs-on")],
  ["bad_value", "jobs.test.steps[0].timeout-minutes", VALID.replace("- run: echo hi", "- run: echo hi\n        timeout-minutes: 0")],
  ["bad_expression", "jobs.test.steps[0].run", VALID.replace("echo hi", "echo ${{ inputs.x")],
  ["git_checkout", "jobs.test.steps[0].uses", VALID.replace("- run: echo hi", "- uses: actions/checkout@v4")],
  ["git_invocation", "jobs.test.steps[0].run", VALID.replace("echo hi", "cargo test && git status")],
  ["unpinned_action", "jobs.test.steps[0].uses", VALID.replace("- run: echo hi", "- uses: owner/repo@v1")],
  ["unsupported_uses", "jobs.test.steps[0].uses", VALID.replace("- run: echo hi", "- uses: docker://alpine:3")],
  ["unsupported_uses", "jobs.test.steps[0].uses", VALID.replace("- run: echo hi", "- uses: ../escape")],
];

for (const [code, path, text] of cases) {
  test(`rejects ${code} at ${path || "<root>"}`, () => {
    const found = validateText("case.yml", text);
    expect(found.map((f) => ({ code: f.code, path: f.path }))).toContainEqual({ code, path });
  });
}

test("accepts a minimal workflow", () => {
  expect(validateText("case.yml", VALID)).toEqual([]);
});

test("accepts a pinned third-party action and a local action", () => {
  const text = VALID.replace("- run: echo hi", "- uses: ./.javelin/actions/setup\n      - uses: owner/repo@" + "a".repeat(40));
  expect(validateText("case.yml", text)).toEqual([]);
});
