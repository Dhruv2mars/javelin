import { afterAll, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { verifyProfiles } from "./verify-profile.mjs";
import { fakeID, fixture, history, check, prepared } from "./test-fixtures.mjs";

const directories=[];
afterAll(async()=>{for(const dir of directories) await rm(dir,{recursive:true,force:true});});
const hash=bytes=>createHash("sha256").update(bytes).digest("hex");
const json=value=>Buffer.from(JSON.stringify(value)+"\n");
const jsonl=events=>Buffer.from(events.map(e=>JSON.stringify(e)).join("\n")+"\n");

// Fabricated records test consistency and branch behavior only. They are never
// retained as native measurements and do not start a profiler or native process.
async function dataset(platform="darwin", availability="available") {
  const dir=await mkdtemp(join(tmpdir(),"javelin-synthetic-profile-"));directories.push(dir);
  const metadata={type:"metadata",profile:"release",opt_level:"3",debug_assertions:false,
    debug:"false",rustc:"synthetic compiler",target:"synthetic target",rustflags:"",
    native_sha256:fakeID("native"),tool_sha256:fakeID("tool")};
  const result={format:1,source_label:"db992f0542b877a2635ed358ff09627656e166a8",
    binary_sha256:fakeID("binary"),platform,elapsed_ms:1000,metadata,
    tools:{perf:{path:availability==="missing"?null:"/synthetic/perf"},
      heaptrack:{path:availability==="available"?"/synthetic/heaptrack":null}},runs:[]};
  const files=new Map([["metadata.stdout",json(metadata)]]);
  for(const action of ["head","stale"]) for(const mode of ["control","cpu","heap"]) {
    const id=action+"-"+mode;
    const event=fixture("depth64",id),f=event.fixture;
    const prep={id:id+"-prepare",kind:"prepare",action,mode,exit_code:0,reason:null,
      stdout:id+"-prepare.stdout"};
    result.runs.push(prep);files.set(prep.stdout,jsonl(prepared(event)));
    if(platform==="linux"&&mode!=="control" &&
      (availability==="missing" || (availability==="blocked"&&mode==="heap"))) {
      result.runs.push({id,action,mode,status:"INCONCLUSIVE",reason:"synthetic missing tool"});
      continue;
    }
    const loop={type:"profile_loop",action,iterations:400,wall_ns:1000000,cpu_ns:900000,
      peak_rss_bytes:4096,history:history(f),check:check(f,f.depth+1,f.depth+(action==="stale"?1:0))};
    const blocked=platform==="linux"&&mode==="cpu"&&availability==="blocked";
    const run={id,kind:"loop",action,mode,exit_code:blocked?1:0,reason:null,
      stdout:id+".stdout",stderr:id+".stderr",loop:blocked?null:structuredClone(loop),
      status:blocked?"INCONCLUSIVE":"RECORDED",attached:[]};
    result.runs.push(run);
    files.set(run.stdout,blocked?Buffer.alloc(0):jsonl([{type:"profile_start",action,pid:123},loop]));
    files.set(run.stderr,Buffer.from(blocked?"synthetic permission refusal":""));
    if(mode!=="control"&&!blocked) {
      if(platform==="darwin") {
        run.attached=[{exit_code:0,reason:null}];
        if(mode==="cpu") files.set(action+"-cpu.sample.txt",Buffer.from("synthetic sample report"));
      } else if(mode==="cpu") {
        const report={kind:"report",action,mode,exit_code:0,stdout:id+"-report.stdout"};
        result.runs.push(report);files.set(report.stdout,Buffer.from("synthetic perf report"));
      } else files.set(action+"-heap.heaptrack",Buffer.from("synthetic heap report"));
    }
  }
  const save=async()=>{
    files.set("profile.json",json(result));
    for(const [name,bytes] of files) await writeFile(join(dir,name),bytes);
    const inventory={files:Object.fromEntries([...files].map(([n,b])=>[n,hash(b)]))};
    await writeFile(join(dir,"integrity.json"),json(inventory));
  };
  await save();
  return {dir,result,files,save};
}

test.each(["darwin","linux"])("successful %s record order verifies without changing input bytes", async platform=>{
  const data=await dataset(platform);
  const before=await readFile(join(data.dir,"integrity.json"));
  const summary=await verifyProfiles(data.dir);
  expect(summary.loops).toHaveLength(6);
  expect(summary.reports).toHaveLength(4);
  expect(summary.reports.every(r=>r.status==="RAW_REPORT_RECORDED")).toBe(true);
  expect(await readFile(join(data.dir,"integrity.json"))).toEqual(before);
  for(const [name,bytes] of data.files) expect(await readFile(join(data.dir,name))).toEqual(bytes);
});

test("missing Linux tools are inconclusive while control loops remain checked", async()=>{
  const data=await dataset("linux","missing"),summary=await verifyProfiles(data.dir);
  expect(summary.loops).toHaveLength(2);
  expect(summary.reports.every(r=>r.status==="INCONCLUSIVE")).toBe(true);
});

test("blocked Linux tools with empty stdout are inconclusive", async()=>{
  const data=await dataset("linux","blocked"),summary=await verifyProfiles(data.dir);
  expect(summary.loops).toHaveLength(2);
  expect(summary.reports.every(r=>r.status==="INCONCLUSIVE")).toBe(true);
});

test("failed Mac profiler attach does not turn a checked loop into stack attribution", async()=>{
  const data=await dataset();
  data.result.runs.find(r=>r.id==="head-cpu").attached[0].exit_code=1;
  await data.save();
  const summary=await verifyProfiles(data.dir);
  expect(summary.reports.find(r=>r.action==="head"&&r.mode==="cpu").status).toBe("INCONCLUSIVE");
});

test("copied metadata cannot disagree with raw output after rehashing inventory", async()=>{
  const data=await dataset();
  data.result.metadata.rustc="altered copied compiler";
  await data.save();
  await expect(verifyProfiles(data.dir)).rejects.toThrow("saved build metadata differs");
});

test("raw metadata must contain exactly one build event", async()=>{
  const data=await dataset();
  data.files.set("metadata.stdout",jsonl([data.result.metadata,data.result.metadata]));
  await data.save();
  await expect(verifyProfiles(data.dir)).rejects.toThrow("saved build metadata differs");
});

test("matching raw and copied unoptimized metadata still fails", async()=>{
  const data=await dataset();
  data.result.metadata.opt_level="2";
  data.files.set("metadata.stdout",json(data.result.metadata));
  await data.save();
  await expect(verifyProfiles(data.dir)).rejects.toThrow("ordinary optimized");
});

test("changed loop summaries fail even with newly hashed file inventory", async()=>{
  const data=await dataset();
  data.result.runs.find(r=>r.id==="head-cpu").loop.wall_ns++;
  await data.save();
  await expect(verifyProfiles(data.dir)).rejects.toThrow("profile summary differs");
});

test("changing both raw loop and copied iteration count cannot hide missing work", async()=>{
  const data=await dataset();
  const run=data.result.runs.find(r=>r.id==="head-cpu");
  const rows=data.files.get(run.stdout).toString().trim().split("\n").map(JSON.parse);
  run.loop.iterations=rows[1].iterations=399;
  data.files.set(run.stdout,jsonl(rows));await data.save();
  await expect(verifyProfiles(data.dir)).rejects.toThrow("profile iteration count");
});

test("successful Linux perf capture requires its report in the declared order", async()=>{
  const data=await dataset("linux");
  const index=data.result.runs.findIndex(r=>r.kind==="report");
  data.result.runs.splice(index,1);
  await data.save();
  await expect(verifyProfiles(data.dir)).rejects.toThrow("perf report coverage");
});
