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
  ["accept-line-conflict", "return None;", "mine"],
  ["region-convergence", "} else if other == region {", "} else if other == region || mine == other {"],
  ["reverse-region", "out.extend_from_slice(taken);", "out.extend(taken.iter().rev());"],
  ["one-sided-anchor", "Some((i, in_ours[i]?, in_theirs[i]?))", "Some((i, in_ours[i]?, in_theirs[i].unwrap_or(t)))"],
  ["unresolved-with", "conflict(kind, collisions.get(&path).cloned())", "conflict(kind, None)"],
  ["addadd-kind", "return Ok(Path::Unresolved(if base.is_none() {", "return Ok(Path::Unresolved(if false {"],
  ["symlink-as-regular", "return Ok(Path::Unresolved(ConflictKind::EntryType));", "return Ok(Path::Resolved(Merged::Retained(ours.clone())));"],
  ["one-side-delete", "if ours == theirs || theirs == base {", "if ours == theirs {"],
];

const test = () =>
  Bun.spawnSync(["cargo", "test", "--locked", "-q"], { stdout: "pipe", stderr: "pipe" }).exitCode === 0;
if (!test()) throw new Error("unmutated tests must pass before mutation checks");

let survived = 0;
try {
  for (const [name, from, to] of mutations) {
    if (original.split(from).length !== 2) throw new Error(`${name}: guard text not found exactly once`);
    writeFileSync(file, original.replace(from, to));
    const killed = !test();
    if (!killed) survived++;
    console.log(`${killed ? "killed" : "SURVIVED"} ${name}`);
  }
} finally {
  writeFileSync(file, original);
}
console.log(`${mutations.length - survived}/${mutations.length} mutations killed`);
process.exit(survived === 0 ? 0 : 1);
