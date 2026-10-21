#!/usr/bin/env bun
// Validates GitHub Actions workflow YAML against Javelin's CI syntax slice.
// Spec: docs/ci-execution.md §10. Usage: bun tools/ci-validate/validate.ts <file.yml>...
import { readFileSync } from "node:fs";
import { posix } from "node:path";
import { parseDocument } from "yaml";

export type Finding = { file: string; path: string; code: string; message: string };

const MAX_BYTES = 256 * 1024;
const MAX_JOBS = 256;
const MAX_STEPS = 64;

const TOP_KEYS = new Set(["name", "run-name", "on", "permissions", "env", "defaults", "concurrency", "jobs"]);
const TOP_UNSUPPORTED = new Set(["cache-mode"]);
const JOB_KEYS = new Set([
  "name", "permissions", "concurrency", "needs", "runs-on", "outputs", "env", "defaults",
  "strategy", "if", "steps", "timeout-minutes", "continue-on-error",
]);
const JOB_UNSUPPORTED = new Set(["cache-mode", "secrets", "container", "services", "uses", "with"]);
const STEP_KEYS = new Set([
  "id", "if", "name", "uses", "run", "working-directory", "shell", "with", "env",
  "continue-on-error", "timeout-minutes",
]);
const STEP_UNSUPPORTED = new Set(["background", "wait", "wait-all", "cancel", "parallel"]);

const JOB_ID = /^[A-Za-z_][A-Za-z0-9_-]*$/;
const SHA = /^[0-9a-f]{40}$/;
const GIT_CALL = /(^|[;&|(])\s*git(\s|$)/;

type Obj = Record<string, unknown>;

const isObject = (v: unknown): v is Obj => typeof v === "object" && v !== null && !Array.isArray(v);
const joinPath = (base: string, key: string) => (base === "" ? key : `${base}.${key}`);

// `${{` and `}}` must pair up in order; an open expression at the end is unbalanced.
function unbalancedExpression(s: string): boolean {
  let open = false;
  for (let i = 0; i < s.length; ) {
    if (s.startsWith("${{", i)) {
      if (open) return true;
      open = true;
      i += 3;
    } else if (s.startsWith("}}", i)) {
      if (!open) return true;
      open = false;
      i += 2;
    } else i++;
  }
  return open;
}

export function validateText(file: string, text: string): Finding[] {
  const findings: Finding[] = [];
  const add = (path: string, code: string, message: string) => findings.push({ file, path, code, message });

  if (new TextEncoder().encode(text).byteLength > MAX_BYTES) {
    add("", "too_large", `file is larger than ${MAX_BYTES} bytes`);
    return findings;
  }
  const doc = parseDocument(text);
  if (doc.errors.length > 0) {
    add("", "yaml_error", doc.errors[0].message);
    return findings;
  }
  const root: unknown = doc.toJS();
  if (!isObject(root)) {
    add("", "missing_required", "workflow must be a mapping");
    return findings;
  }

  const checkKeys = (obj: Obj, path: string, known: Set<string>, unsupported: Set<string>) => {
    for (const key of Object.keys(obj)) {
      const at = joinPath(path, key);
      if (unsupported.has(key)) add(at, "unsupported_key", `"${key}" is not supported in slice 1`);
      else if (!known.has(key)) add(at, "unknown_key", `"${key}" is not a workflow key`);
    }
  };
  const checkTimeout = (value: unknown, path: string) => {
    if (value === undefined) return;
    if (!Number.isInteger(value) || (value as number) < 1 || (value as number) > 360) {
      add(joinPath(path, "timeout-minutes"), "bad_value", "timeout-minutes must be an integer from 1 to 360");
    }
  };
  const checkExpressions = (value: unknown, path: string) => {
    if (typeof value === "string") {
      if (unbalancedExpression(value)) add(path, "bad_expression", "unbalanced ${{ or }}");
    } else if (Array.isArray(value)) {
      value.forEach((v, i) => checkExpressions(v, `${path}[${i}]`));
    } else if (isObject(value)) {
      for (const [k, v] of Object.entries(value)) checkExpressions(v, joinPath(path, k));
    }
  };

  checkKeys(root, "", TOP_KEYS, TOP_UNSUPPORTED);
  checkExpressions(root, "");
  if (root.on === undefined) add("on", "missing_required", "workflow has no `on`");
  if (root.jobs === undefined) add("jobs", "missing_required", "workflow has no `jobs`");
  if (root.jobs !== undefined && !isObject(root.jobs)) {
    add("jobs", "bad_value", "jobs must be a mapping");
  }
  const jobs: Obj = isObject(root.jobs) ? root.jobs : {};
  const jobIds = Object.keys(jobs);
  if (isObject(root.jobs) && jobIds.length === 0) add("jobs", "missing_required", "jobs is empty");
  if (jobIds.length > MAX_JOBS) add("jobs", "too_large", `more than ${MAX_JOBS} jobs`);

  for (const id of jobIds) {
    const jp = joinPath("jobs", id);
    if (!JOB_ID.test(id)) add(jp, "bad_job_id", `job id "${id}" must match ${JOB_ID}`);
    const job = jobs[id];
    if (!isObject(job)) {
      add(jp, "bad_value", "job must be a mapping");
      continue;
    }
    checkKeys(job, jp, JOB_KEYS, JOB_UNSUPPORTED);
    checkTimeout(job["timeout-minutes"], jp);

    const steps = job.steps;
    if (steps !== undefined && !Array.isArray(steps)) add(joinPath(jp, "steps"), "bad_value", "steps must be a sequence");
    if (job.uses === undefined) {
      if (job["runs-on"] === undefined) add(jp, "missing_required", "job has no `runs-on`");
      if (!Array.isArray(steps) || steps.length === 0) add(jp, "missing_required", "job has no `steps`");
    }
    if (Array.isArray(steps)) {
      if (steps.length > MAX_STEPS) add(joinPath(jp, "steps"), "too_large", `more than ${MAX_STEPS} steps`);
      steps.forEach((step, i) => checkStep(step, `${joinPath(jp, "steps")}[${i}]`));
    }
  }

  // needs: names must exist and the graph must be acyclic.
  const needsOf = (id: string): string[] => {
    const job = jobs[id];
    if (!isObject(job) || job.needs === undefined) return [];
    const list = typeof job.needs === "string" ? [job.needs] : job.needs;
    if (!Array.isArray(list) || !list.every((n) => typeof n === "string")) {
      add(joinPath(joinPath("jobs", id), "needs"), "bad_value", "needs must be a job id or a list of job ids");
      return [];
    }
    for (const dep of list) {
      if (!Object.hasOwn(jobs, dep)) add(joinPath(joinPath("jobs", id), "needs"), "bad_needs", `needs unknown job "${dep}"`);
    }
    return list.filter((dep) => Object.hasOwn(jobs, dep));
  };
  const done = new Set<string>();
  const active = new Set<string>();
  const visit = (id: string) => {
    if (done.has(id)) return;
    active.add(id);
    for (const dep of needsOf(id)) {
      if (active.has(dep)) add(joinPath(joinPath("jobs", id), "needs"), "bad_needs", `needs cycle through "${dep}"`);
      else visit(dep);
    }
    active.delete(id);
    done.add(id);
  };
  for (const id of jobIds) visit(id);

  function checkStep(step: unknown, sp: string) {
    if (!isObject(step)) {
      add(sp, "bad_value", "step must be a mapping");
      return;
    }
    checkKeys(step, sp, STEP_KEYS, STEP_UNSUPPORTED);
    checkTimeout(step["timeout-minutes"], sp);
    const hasRun = step.run !== undefined;
    const hasUses = step.uses !== undefined;
    if (hasRun === hasUses) {
      add(sp, "bad_step", "step needs exactly one of `run` or `uses`");
      return;
    }
    if (hasUses) {
      if (typeof step.uses !== "string") {
        add(joinPath(sp, "uses"), "bad_value", "uses must be a string");
        return;
      }
      checkUses(step.uses, joinPath(sp, "uses"));
    } else if (typeof step.run !== "string") {
      add(joinPath(sp, "run"), "bad_value", "run must be a string");
    } else if (step.run.split("\n").some((line) => GIT_CALL.test(line))) {
      add(joinPath(sp, "run"), "git_invocation", "run invokes git; Javelin workflows must not");
    }
  }

  function checkUses(uses: string, at: string) {
    if (uses.toLowerCase().startsWith("actions/checkout")) {
      add(at, "git_checkout", "actions/checkout fetches Git; use `javelin revision fetch`");
    } else if (uses.startsWith("docker://")) {
      add(at, "unsupported_uses", "docker:// actions are not supported");
    } else if (uses.startsWith("./") || uses.startsWith("../")) {
      if (posix.normalize(uses).startsWith("..")) add(at, "unsupported_uses", "local action escapes the revision");
    } else {
      const ref = uses.slice(uses.lastIndexOf("@") + 1);
      if (!uses.includes("@") || !SHA.test(ref)) {
        add(at, "unpinned_action", "third-party action must be pinned to a full 40-hex commit SHA");
      }
    }
  }

  return findings;
}

export function main(argv: string[]): number {
  if (argv.length === 0) {
    process.stderr.write("usage: bun tools/ci-validate/validate.ts <file.yml>...\n");
    return 2;
  }
  let status = 0;
  for (const file of argv) {
    let text: string;
    try {
      text = readFileSync(file, "utf8");
    } catch (err) {
      process.stderr.write(`error: cannot read ${file}: ${(err as Error).message}\n`);
      status = 2;
      continue;
    }
    for (const finding of validateText(file, text)) {
      process.stdout.write(JSON.stringify(finding) + "\n");
      if (status === 0) status = 1;
    }
  }
  return status;
}

if (import.meta.main) process.exit(main(process.argv.slice(2)));
