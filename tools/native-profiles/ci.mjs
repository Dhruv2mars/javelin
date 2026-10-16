import { mkdir } from "node:fs/promises";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { bounded } from "./run.mjs";

const here=dirname(fileURLToPath(import.meta.url));
const out=join(here,"scratch/ci");
await mkdir(out,{recursive:true});
const manifest=join(here,"Cargo.toml");
const commands=[
  ["toolchain",["rustup","toolchain","install","1.93.0","--profile","minimal","--component","rustfmt","--component","clippy"]],
  ["fmt",["cargo","fmt","--manifest-path",manifest,"--check"]],
  ["clippy",["cargo","clippy","--manifest-path",manifest,"--locked","--all-targets","--","-D","warnings"]],
  ["tests",["cargo","test","--manifest-path",manifest,"--locked"]],
  ["tests-release",["cargo","test","--manifest-path",manifest,"--locked","--release"]],
  ["verifier-tests",["bun","test",join(here,"verify.test.mjs"),join(here,"run.test.mjs")]],
  ["release",["cargo","build","--manifest-path",manifest,"--locked","--release"]],
  ["controls",["bun",join(here,"run.mjs"),"run",join(out,"controls"),process.env.NP_SOURCE]],
  ["verify",["bun",join(here,"verify.mjs"),join(out,"controls")]],
  ["profiles",["bun",join(here,"profile.mjs"),join(out,"profiles"),process.env.NP_SOURCE]],
  ["verify-profiles",["bun",join(here,"verify-profile.mjs"),join(out,"profiles")]],
];
for(const [name,cmd] of commands) {
  console.log(name);
  const result=await bounded(cmd,{out:join(out,name+".stdout"),err:join(out,name+".stderr"),
    timeout:["controls","profiles"].includes(name)?610000:90000});
  if(result.exit_code!==0||result.reason) throw Error(name+" failed: "+JSON.stringify(result));
}
