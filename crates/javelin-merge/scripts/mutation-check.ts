// Each mutant must compile and fail a test. All three source files are restored.
// Usage (from crates/javelin-merge): bun scripts/mutation-check.ts
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";


const semanticMutations: [string, string, string][] = [
  ["binary-guard", "bytes.contains(&0)", "false && bytes.contains(&0)"],
  ["structural-pass", "if present.contains(&parent) {", "if false && present.contains(&parent) {"],
  ["unresolved-presence", "Path::Unresolved(_) => o.is_some() || t.is_some(),", "Path::Unresolved(_) => false,"],
  ["exec-rule", "let executable = if ox == bx { *tx } else { *ox };", "let executable = *ox;"],
  ["content-fastpath", "let content = if o == t || t == b {", "let content = if false {"],
  ["accept-line-conflict", "(false, false) => return None,", "(false, false) => (Some(true), mine),"],
  ["region-convergence", "(false, false) => return None,", "(false, false) if mine == other => (Some(true), mine),\n            (false, false) => return None,"],
  ["reverse-region", "out.extend_from_slice(taken);", "out.extend(taken.iter().rev());"],
  ["any-anchor-separates", "separated = separated || (unique(base[i]) && kept(i));", "separated = true;"],
  ["unique-without-kept", "separated = separated || (unique(base[i]) && kept(i));", "separated = separated || unique(base[i]);"],
  ["base-only-uniqueness", ".all(|lines| lines.iter().filter(|l| **l == line).count() == 1)", ".any(|lines| lines.iter().filter(|l| **l == line).count() == 1)"],
  ["one-sided-anchor", "Some((i, in_ours[i]?, in_theirs[i]?))", "Some((i, in_ours[i]?, in_theirs[i].unwrap_or(t)))"],
  ["unresolved-with", "conflict(kind, collisions.get(&path).cloned())", "conflict(kind, None)"],
  ["addadd-kind", "return Ok(Path::Unresolved(if base.is_none() {", "return Ok(Path::Unresolved(if false {"],
  ["symlink-as-regular", "return Ok(Path::Unresolved(ConflictKind::EntryType));", "return Ok(Path::Resolved(Merged::Retained(ours.clone())));"],
  ["one-side-delete", "if ours == theirs || theirs == base {", "if ours == theirs {"],
];

const mutations: [string, string, string, string][] = [
  ...semanticMutations.map(([name, from, to]): [string, string, string, string] => [name, "src/lib.rs", from, to]),
  ["entry-cap", "src/lib.rs", "Resource::Entries.check((b.len() + o.len() + t.len()) as u64)?;", ""],
  ["path-cap", "src/lib.rs", "Resource::PathBytes.check(len)?;", ""],
  ["total-path-cap", "src/lib.rs", "Resource::TotalPathBytes.check(path_bytes)?;", ""],
  ["line-cap", "src/lib.rs", "Resource::Lines.check(*count)?;", ""],
  ["table-cap", "src/lib.rs", "Resource::TableCells.check(cells)?;", ""],
  ["blob-cap", "src/resources.rs", "Resource::BlobBytes.check(bytes as u64)?;", ""],
  ["loaded-cap", "src/resources.rs", "Resource::LoadedBytes.check(required)?;", ""],
  ["work-cap", "src/resources.rs", "Resource::Work.check(required)?;", ""],
  ["inclusive-cap", "src/resources.rs", "required > self.limit()", "required >= self.limit()"],
  ["work-reset", "src/resources.rs", "self.work_units.saturating_add(units)", "units"],
  ["loaded-reset", "src/resources.rs", "self.loaded_bytes.saturating_add(bytes as u64)", "bytes as u64"],
  ["mandatory-off-by-one", "src/matching.rs", "mandatory[i] = without < total;", "mandatory[i] = without <= total;"],
  ["wrong-omission-suffix", "src/matching.rs", "prefix[i * width + j] + suffix[j]", "prefix[i * width + j] + suffix[0]"],
];

const files = ["src/lib.rs", "src/resources.rs", "src/matching.rs"];
const originals = new Map(files.map(file => [file, readFileSync(file, "utf8")]));
const cargo = (...args: string[]) => Bun.spawnSync(["cargo", ...args], { stdout: "pipe", stderr: "pipe" });
const test = () => cargo("test", "--locked", "-q", "--", "--skip", "adversarial_large_inputs_have_deterministic_typed_outcomes");
if (test().exitCode !== 0) throw new Error("unmutated tests must pass before mutation checks");
mkdirSync("ci-logs", { recursive: true });
let survived = 0;
try {
  for (const [name, file, from, to] of mutations) {
    const original = originals.get(file);
    if (original === undefined || original.split(from).length !== 2) throw new Error(`${name}: guard text not found exactly once`);
    writeFileSync(file, original.replace(from, to));
    const compiled = cargo("test", "--locked", "--no-run", "-q");
    if (compiled.exitCode !== 0) throw new Error(`${name}: mutant does not compile\n${compiled.stderr.toString()}`);
    const result = test();
    const output = result.stdout.toString() + result.stderr.toString();
    writeFileSync(`ci-logs/mutant-${name}.log`, `compiled=true exit=${result.exitCode}\n${output}`);
    const killed = result.exitCode !== 0 && output.includes("test result: FAILED");
    if (!killed) survived++;
    console.log(`${killed ? "killed" : "SURVIVED"} ${name} compiled=true`);
    writeFileSync(file, original);
  }
} finally {
  for (const [file, original] of originals) writeFileSync(file, original);
}
for (const [file, original] of originals) {
  if (readFileSync(file, "utf8") !== original) throw new Error(`${file}: restoration failed`);
  console.log(`restored sha256=${new Bun.CryptoHasher("sha256").update(original).digest("hex")} ${file}`);
}
console.log(`${mutations.length - survived}/${mutations.length} compiling mutations killed`);
process.exit(survived === 0 ? 0 : 1);
