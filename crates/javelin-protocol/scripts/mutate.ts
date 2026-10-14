// Guard-removal check: each mutation removes one protocol guard, and the suite
// must fail. Run from crates/javelin-protocol: `bun scripts/mutate.ts`.
// Files are restored after every mutation.
import { readFileSync, writeFileSync } from "node:fs";
import { spawnSync } from "node:child_process";

const mutations: [string, string, string, string][] = [
 [
  "src/receiver.rs",
  "if stored != *claimed {\n                    return Err(mismatch(*claimed, stored));\n                }\n            }\n            Ok(Message::Stored {\n                count: blobs.len()",
  "if false {\n                    return Err(mismatch(*claimed, stored));\n                }\n            }\n            Ok(Message::Stored {\n                count: blobs.len()",
  "blob id compare"
 ],
 [
  "src/receiver.rs",
  "if stored != *claimed {\n                    return Err(mismatch(*claimed, stored));\n                }\n            }\n            Ok(Message::Stored {\n                count: manifests.len()",
  "if false {\n                    return Err(mismatch(*claimed, stored));\n                }\n            }\n            Ok(Message::Stored {\n                count: manifests.len()",
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
  "Err(javelin::Error::MissingObject(missing_id)) if missing_id == id =>",
  "Err(_) =>",
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
  "if items.iter().all(|item| chunk.contains(item))",
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
];

let survived = 0;
for (const [path, from, to, name] of mutations) {
  const source = readFileSync(path, "utf8");
  if (source.split(from).length !== 2) throw new Error(`selector not unique: ${name}`);
  writeFileSync(path, source.replace(from, to));
  try {
    const run = spawnSync("cargo", ["test", "-q"], { stdio: "ignore" });
    const detected = run.status !== 0;
    if (!detected) survived++;
    console.log(`${detected ? "DETECTED" : "SURVIVED"} ${name}`);
  } finally {
    writeFileSync(path, source);
  }
}
console.log(`${mutations.length - survived}/${mutations.length} detected`);
process.exit(survived ? 1 : 0);
