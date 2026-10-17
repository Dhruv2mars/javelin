// Guard-removal check: each mutation removes one protocol guard, and the suite
// must fail. Run from crates/javelin-protocol: `bun scripts/mutate.ts`.
// Files are restored after every mutation.
import { readFileSync, writeFileSync } from "node:fs";
import { spawnSync } from "node:child_process";

// Never run concurrently with another mutation run in the same worktree.

const mutations: [string, string, string, string][] = [
 [
  "src/receiver.rs",
  "if actual != *claimed {\n                    return Err(mismatch(*claimed, actual));\n                }\n                repo.put_blob(bytes)?;\n            }\n            Ok(Message::Stored {\n                count: blobs.len()",
  "if false {\n                    return Err(mismatch(*claimed, actual));\n                }\n                repo.put_blob(bytes)?;\n            }\n            Ok(Message::Stored {\n                count: blobs.len()",
  "blob id compare"
 ],
 [
  "src/receiver.rs",
  "if actual != *claimed {\n                    return Err(mismatch(*claimed, actual));\n                }\n                // put_manifest still validates the complete referenced content.\n                repo.put_manifest(manifest)?;\n            }\n            Ok(Message::Stored {\n                count: manifests.len()",
  "if false {\n                    return Err(mismatch(*claimed, actual));\n                }\n                // put_manifest still validates the complete referenced content.\n                repo.put_manifest(manifest)?;\n            }\n            Ok(Message::Stored {\n                count: manifests.len()",
  "manifest id compare"
 ],
 [
  "src/codec.rs",
  "let n = self.u32()? as usize;\n        if n > max {",
  "let n = self.u32()? as usize;\n        if false {",
  "count limit"
 ],
 [
  "src/codec.rs",
  "if len > MAX_BLOB {\n                    return Err(limit(\"blob exceeds size limit\"));\n                }\n                blobs.push",
  "blobs.push",
  "blob limit"
 ],
 [
  "src/codec.rs",
  "    if len > MAX_FRAME {\n        return Err(limit(\"frame exceeds size limit\"));\n    }\n    let mut body",
  "    let mut body",
  "frame limit"
 ],
 [
  "src/codec.rs",
  "if len > MAX_PATH {",
  "if false {",
  "path limit"
 ],
 [
  "src/codec.rs",
  "== b'/').count() >= MAX_DEPTH {",
  "== b'/').count() >= usize::MAX {",
  "depth limit"
 ],
 [
  "src/codec.rs",
  "if path.matches('/').count() >= MAX_DEPTH {",
  "if false {",
  "encode depth"
 ],
 [
  "src/codec.rs",
  "if previous.is_some_and(|p| p >= raw) {",
  "if false {",
  "ascending paths"
 ],
 [
  "src/codec.rs",
  "if !r.0.is_empty() {",
  "if false {",
  "trailing bytes"
 ],
 [
  "src/codec.rs",
  "Ok(0) => return Err(truncated()),",
  "Ok(0) => return Ok(None),",
  "header truncation"
 ],
 [
  "src/receiver.rs",
  "Ok(Some(message)) if !negotiated =>",
  "Ok(Some(message)) if false =>",
  "negotiation required"
 ],
 [
  "src/receiver.rs",
  "if (min..=max).contains(&VERSION)",
  "",
  "version refusal"
 ],
 [
  "src/receiver.rs",
  "if items.iter().collect::<BTreeSet<_>>().len() != items.len() {",
  "if false {",
  "want dedup"
 ],
 [
  "src/receiver.rs",
  "}?;\n                if !found {",
  "}.unwrap_or(false);\n                if !found {",
  "want kind separation"
 ],
 [
  "src/sender.rs",
  "&& items.iter().collect::<BTreeSet<_>>().len() == items.len()",
  "",
  "missing dedup"
 ],
 [
  "src/sender.rs",
  "if items.iter().all(|item| chunk_set.contains(item))",
  "if true",
  "missing subset"
 ],
 [
  "src/sender.rs",
  "Some(number.get()) == expected.get().checked_add(1)",
  "true",
  "accepted number"
 ],
 [
  "src/sender.rs",
  "&& *manifest == manifest_id",
  "",
  "accepted manifest"
 ],
 [
  "src/sender.rs",
  "&& accepted == operation",
  "",
  "accepted operation"
 ],
 [
  "src/sender.rs",
  "Outcome::Stale { actual } => *actual > expected,",
  "Outcome::Stale { .. } => true,",
  "stale bound"
 ],
 [
  "src/codec.rs",
  "pub fn decode(body: &[u8]) -> Result<Message, ProtocolError> {\n    if body.len() > MAX_FRAME {",
  "pub fn decode(body: &[u8]) -> Result<Message, ProtocolError> {\n    if false {",
  "decode size"
 ],
 [
  "src/codec.rs",
  "    if len > MAX_FRAME {\n        return Err(limit(\"frame exceeds size limit\"));\n    }\n    Ok(len)",
  "    Ok(len)",
  "encode total"
 ],
 [
  "src/codec.rs",
  "if bytes.len() > MAX_BLOB {\n                    return Err(limit(\"blob exceeds size limit\"));\n                }\n                len",
  "len",
  "encode blob"
 ]

 ,[
  "src/receiver.rs",
  "Kind::Blob => repo.has_blob(id),",
  "Kind::Blob => repo.read_blob(id).map(|_| true).or_else(|e| match e { javelin::Error::MissingObject(_) => Ok(false), _ => Err(e) }),",
  "Want blob allocation regression"
 ],
 [
  "src/receiver.rs",
  "Kind::Manifest => repo.has_manifest(id),",
  "Kind::Manifest => repo.manifest_bytes(id).map(|_| true).or_else(|e| match e { javelin::Error::MissingObject(_) => Ok(false), _ => Err(e) }),",
  "Want manifest graph allocation regression"
 ],
 [
  "src/sender.rs",
  "source.read_blob_bounded(id, MAX_BLOB)?",
  "source.read_blob(id)?",
  "sender blob read cap"
 ],
 [
  "src/sender.rs",
  "source.manifest_bytes_bounded(manifest_id, MAX_FRAME)?",
  "source.manifest_bytes(manifest_id)?",
  "sender manifest read cap and graph isolation"
 ],
 [
  "src/receiver.rs",
  "let actual = javelin::ObjectId::for_blob(bytes);",
  "repo.put_blob(bytes)?; let actual = javelin::ObjectId::for_blob(bytes);",
  "blob pre-store ordering"
 ],
 [
  "src/receiver.rs",
  "let actual = javelin::ObjectId::for_manifest(manifest);",
  "repo.put_manifest(manifest)?; let actual = javelin::ObjectId::for_manifest(manifest);",
  "manifest pre-store ordering"
 ]
];

// Optional name filters let a change run only its relevant regressions.
const filters = process.argv.slice(2);
const selected = mutations.filter(([, , , name]) => !filters.length || filters.some(f => name.includes(f)));
if (!selected.length) throw new Error("no matching mutations");

// Refuse stale selectors before mutating any file.
for (const [path, from, , name] of selected) {
  if (readFileSync(path, "utf8").split(from).length !== 2) {
    throw new Error(`selector not unique: ${name}`);
  }
}

let survived = 0;
for (const [path, from, to, name] of selected) {
  const source = readFileSync(path, "utf8");
  if (source.split(from).length !== 2) throw new Error(`selector not unique: ${name}`);
  writeFileSync(path, source.replace(from, to));
  try {
    // A mutant that does not compile proves nothing about the tests.
    const build = spawnSync("cargo", ["test", "--locked", "--all-targets", "--no-run"], { stdio: "ignore" });
    if (build.status !== 0) {
      survived++;
      console.log(`BUILD-FAILED ${name}`);
      continue;
    }
    const run = spawnSync("cargo", ["test", "--locked", "--all-targets"], {
      encoding: "utf8",
      maxBuffer: 8 << 20,
    });
    const output = run.stdout + run.stderr;
    const detected = run.status === 101 && output.includes("test result: FAILED");
    const failures = output.split("\n").filter(line => line.startsWith("test ") && line.endsWith("FAILED"));
    if (!detected) survived++;
    console.log(`${detected ? "DETECTED" : "SURVIVED"} ${name}: ${failures.join("; ")}`);
  } finally {
    writeFileSync(path, source);
  }
}
console.log(`${selected.length - survived}/${selected.length} detected`);
process.exit(survived ? 1 : 0);
