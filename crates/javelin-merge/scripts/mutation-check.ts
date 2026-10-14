// Removes each merge guard in turn and requires `cargo test` to fail. Restores src/lib.rs.
// Usage (from crates/javelin-merge): bun scripts/mutation-check.ts
import { readFileSync, writeFileSync } from "node:fs";

const file = "src/lib.rs";
const original = readFileSync(file, "utf8");
const mutations: [string, string, string][] = [
  ["binary-guard", "bytes.contains(&0)", "false && bytes.contains(&0)"],
  ["structural-pass", "if present.contains(&parent) {", "if false && present.contains(&parent) {"],
  ["unresolved-presence", "Path::Unresolved(_) => o.is_some() || t.is_some(),", "Path::Unresolved(_) => false,"],
  ["exec-rule", "let executable = if ox == bx { *tx } else { *ox };", "let executable = *ox;"],
  ["content-fastpath", "let content = if o == t || t == b {", "let content = if false {"],
  ["accept-line-conflict", "(false, false) => return None,", "(false, false) => (Some(true), mine),"],
  ["region-convergence", "(false, false) => return None,", "(false, false) if mine == other => (Some(true), mine),\n            (false, false) => return None,"],
  ["reverse-region", "out.extend_from_slice(taken);", "out.extend(taken.iter().rev());"],
  ["any-anchor-separates", "separated = separated || unique(line);", "separated = true;"],
  ["base-only-uniqueness", ".all(|lines| lines.iter().filter(|l| **l == line).count() == 1)", ".any(|lines| lines.iter().filter(|l| **l == line).count() == 1)"],
  ["one-sided-anchor", "Some((i, in_ours[i]?, in_theirs[i]?))", "Some((i, in_ours[i]?, in_theirs[i].unwrap_or(t)))"],
  ["unresolved-with", "conflict(kind, collisions.get(&path).cloned())", "conflict(kind, None)"],
  ["addadd-kind", "return Ok(Path::Unresolved(if base.is_none() {", "return Ok(Path::Unresolved(if false {"],
  ["symlink-as-regular", "return Ok(Path::Unresolved(ConflictKind::EntryType));", "return Ok(Path::Resolved(Merged::Retained(ours.clone())));"],
  ["one-side-delete", "if ours == theirs || theirs == base {", "if ours == theirs {"],
];

const cargo = (...args: string[]) =>
  Bun.spawnSync(["cargo", ...args, "--locked", "-q"], { stdout: "pipe", stderr: "pipe" }).exitCode === 0;
const test = () => cargo("test");
if (!test()) throw new Error("unmutated tests must pass before mutation checks");

let survived = 0;
try {
  for (const [name, from, to] of mutations) {
    if (original.split(from).length !== 2) throw new Error(`${name}: guard text not found exactly once`);
    writeFileSync(file, original.replace(from, to));
    // A mutant that does not compile proves nothing about the tests.
    if (!cargo("test", "--no-run")) throw new Error(`${name}: mutant does not compile`);
    const killed = !test();
    if (!killed) survived++;
    console.log(`${killed ? "killed" : "SURVIVED"} ${name}`);
  }
} finally {
  writeFileSync(file, original);
}
console.log(`${mutations.length - survived}/${mutations.length} mutations killed`);
process.exit(survived === 0 ? 0 : 1);
