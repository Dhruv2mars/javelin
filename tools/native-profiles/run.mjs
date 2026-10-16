import { mkdir, mkdtemp, readdir, lstat, readFile, writeFile, rm, chmod } from "node:fs/promises";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { createHash } from "node:crypto";
import { tmpdir, cpus, loadavg, totalmem, platform, arch } from "node:os";

export const cases = ["base","depth4","depth64","entries8","entries128","distinct-blobs","distinct-manifests"];
export const actions = ["head","accepted","stale","clone"];
export const budget = { child_wall_ms:90000, child_cpu_s:60, child_rss_bytes:268435456,
  profile_rss_bytes:536870912, active_fixture_bytes:134217728, retained_bytes:1073741824,
  total_wall_ms:600000, max_native_workers:2 };
const here=dirname(fileURLToPath(import.meta.url));
const root=resolve(here,"../..");
const binary=join(here,"target/release/javelin-native-profiles");
const digest=bytes=>createHash("sha256").update(bytes).digest("hex");
const json=(p,v)=>writeFile(p,JSON.stringify(v,null,2)+"\n",{flag:"wx"});
const pause=ms=>new Promise(r=>setTimeout(r,ms));

export async function diskBytes(dir) {
  let sum=0;
  for (const e of await readdir(dir,{withFileTypes:true})) {
    const path=join(dir,e.name), stat=await lstat(path);
    if (stat.isSymbolicLink() || (!stat.isDirectory() && !stat.isFile())) throw Error("nonregular evidence path");
    sum+=stat.isDirectory()?await diskBytes(path):stat.size;
  }
  return sum;
}

export async function bounded(cmd,{out,err,env=process.env,timeout=budget.child_wall_ms,onPoll=async()=>{}}) {
  const start=performance.now();
  const p=Bun.spawn({cmd:["rtk","proxy",...cmd],stdout:Bun.file(out),stderr:Bun.file(err),
    stdin:"ignore",env,detached:true});
  let reason=null,done=false;
  const completion=p.exited.then(code=>{done=true;return code;});
  while(!done) {
    if (performance.now()-start>timeout) reason="wall_timeout";
    if (!reason) {
      try { reason=await onPoll(p.pid); } catch(e) { reason="monitor_error: "+e.message; }
    }
    if(reason) {
      try {process.kill(-p.pid,"SIGKILL");} catch {p.kill("SIGKILL");}
      break;
    }
    await pause(50);
  }
  const code=await completion;
  return {exit_code:code,timed_out:reason==="wall_timeout",reason,wall_ms:performance.now()-start,pid:p.pid};
}

async function probe(cmd,dir,name,env=process.env) {
  const out=join(dir,name+".stdout"),err=join(dir,name+".stderr");
  let result;
  try { result=await bounded(cmd,{out,err,timeout:5000,env}); }
  catch(e) {result={exit_code:null,error:e.message};}
  return {cmd,...result,stdout:await readFile(out,"utf8").catch(()=>""),stderr:await readFile(err,"utf8").catch(()=>"")};
}

async function sourceFiles() {
  const names=["Cargo.lock","Cargo.toml","src/domain.rs","src/layer.rs","src/lib.rs","src/main.rs","src/repository.rs",
    "tools/native-profiles/Cargo.lock","tools/native-profiles/Cargo.toml","tools/native-profiles/build.rs",
    "tools/native-profiles/src/main.rs","tools/native-profiles/run.mjs","tools/native-profiles/verify.mjs",
    "tools/native-profiles/profile.mjs","tools/native-profiles/verify-profile.mjs",
    "tools/native-profiles/ci.mjs","docs/native-validation-profile.md"];
  return Object.fromEntries(await Promise.all(names.map(async n=>[n,digest(await readFile(join(root,n)))])));
}

export async function seal(dir) {
  const files={};
  async function walk(base,prefix="") {
    for (const e of (await readdir(base,{withFileTypes:true})).sort((a,b)=>a.name.localeCompare(b.name))) {
      const path=join(base,e.name),name=prefix+e.name;
      if(e.isDirectory()) await walk(path,name+"/");
      else if(e.isFile() && name!=="integrity.json") files[name]=digest(await readFile(path));
      else if(!e.isFile()) throw Error("nonregular sealed path");
    }
  }
  await walk(dir);
  await json(join(dir,"integrity.json"),{files});
}

async function run(out,source) {
  if(!source) throw Error("opaque developer --source label required");
  await mkdir(out,{recursive:false});
  const work=await mkdtemp(join(tmpdir(),"javelin-profile-"));
  const started=performance.now();
  const run={format:1,source_label:source,preregistration:"b493add",cases,actions,repetitions:3,
    budget,children:[],complete:false,elapsed_ms:0,disk_peak_bytes:0,
    host:{platform:platform(),arch:arch(),cpus:cpus(),totalmem:totalmem(),load_before:loadavg(),
      workdir:work,working_set:"warm OS cache; native handles reopen between prepare and measurement",
      concurrent_load:"No load waiting or affinity; ps snapshots and load averages retained."},
    identity:{run_id:process.env.GITHUB_RUN_ID??null,run_attempt:process.env.GITHUB_RUN_ATTEMPT??null,
      job:process.env.GITHUB_JOB??null,artifact_name:process.env.NP_ARTIFACT_NAME??null},
    source_files:await sourceFiles(),binary_sha256:digest(await readFile(binary))};
  const stub=join(work,"bin");await mkdir(stub);
  const marker=join(work,"git-invoked");
  const shim=join(stub,"git");
  await writeFile(shim,'#!/bin/sh\nprintf invoked >> "$NP_GIT_MARKER"\nexit 97\n');await chmod(shim,0o755);
  const env={...process.env,PATH:stub+":"+process.env.PATH,NP_GIT_MARKER:marker};
  const positive=await probe([shim],out,"git-detector-positive",env);
  const positiveBytes=await readFile(marker,"utf8");
  if(positive.exit_code!==97||positiveBytes!=="invoked") throw Error("Git detector positive control failed");
  await rm(marker);
  run.git_detector={positive_control:true,product_invocations:0};
  run.host.filesystem=await probe(platform()==="darwin"?["df","-P","-k",work]:["stat","-f","-c","%T",work],out,"filesystem");
  run.host.mounts=await probe(["mount"],out,"mounts");
  run.host.processes_before=await probe(["ps","-Ao","pid,pcpu,pmem,comm"],out,"load-before");
  const metadata=await probe([binary,"metadata"],out,"metadata");
  if(metadata.exit_code!==0) throw Error("metadata command failed");
  run.metadata=JSON.parse(metadata.stdout);
  if(run.metadata.debug_assertions||run.metadata.profile!=="release"||run.metadata.opt_level!=="3") throw Error("ordinary release executable required");
  const records=new Map();
  let nextMonitor=0;
  async function poll() {
    if(performance.now()-started>budget.total_wall_ms) return "total_wall_cap";
    if(performance.now()<nextMonitor) return null;
    nextMonitor=performance.now()+250;
    const size=await diskBytes(work),retained=await diskBytes(out);
    run.disk_peak_bytes=Math.max(run.disk_peak_bytes,size+retained);
    if(size>budget.active_fixture_bytes) return "fixture_disk_cap";
    if(size+retained>budget.retained_bytes) return "retained_disk_cap";
    return null;
  }
  async function child(info,args) {
    const stdout=info.id+".jsonl",stderr=info.id+".stderr";
    const entry={...info,stdout,stderr};run.children.push(entry);
    const result=await bounded([binary,...args],{out:join(out,stdout),err:join(out,stderr),env,onPoll:poll});
    Object.assign(entry,result);
    const bytes=await readFile(join(out,stdout),"utf8");
    const events=bytes.trim()?bytes.trim().split("\n").map(line=>JSON.parse(line)):[];
    records.set(info.id,events);
    if(result.exit_code!==0||result.reason) throw Error("INCONCLUSIVE child "+info.id+": "+JSON.stringify(result));
    return events;
  }
  try {
    // One separate uncontended accepted fixture precedes any bounded race.
    for(let rep=0;rep<3;rep++) {
      const order=[...cases.slice(rep),...cases.slice(0,rep)];
      for(const caseName of order) {
        for(const action of actions) {
          const id="r"+rep+"-"+caseName+"-"+action;
          const fixture=join(work,id);
          console.log("sample",id);
          await child({id:id+"-prepare",case:caseName,action,rep,kind:"prepare"},["prepare",fixture,caseName]);
          await child({id:id+"-measure",case:caseName,action,rep,kind:"measure"},["measure",fixture,action,"0"]);
          await rm(fixture,{recursive:true});
        }
      }
      const id="r"+rep+"-base-race",fixture=join(work,id);
      await child({id:id+"-prepare",case:"base",action:"race",rep,kind:"prepare"},["prepare",fixture,"base"]);
      const workers=["0","1"].map(worker=>child({id:id+"-"+worker,case:"base",action:"race",rep,kind:"race-worker",worker},
        ["measure",fixture,"race",worker]));
      const settled=Promise.allSettled(workers);
      let ready=false;
      for(let i=0;i<200;i++) {
        if(await Bun.file(join(fixture,"ready-0")).exists() && await Bun.file(join(fixture,"ready-1")).exists()) {ready=true;break;}
        await pause(50);
      }
      if(ready) await writeFile(join(fixture,"go"),"go",{flag:"wx"});
      const results=await settled;
      if(!ready||results.some(r=>r.status==="rejected")) throw Error("INCONCLUSIVE race readiness or worker failure");
      await child({id:id+"-inspect",case:"base",action:"race",rep,kind:"race-inspect"},["inspect",fixture]);
      await rm(fixture,{recursive:true});
    }
    const {analyze}=await import("./verify.mjs");
    run.complete=true;run.elapsed_ms=performance.now()-started;
    run.host.load_after=loadavg();
    run.host.processes_after=await probe(["ps","-Ao","pid,pcpu,pmem,comm"],out,"load-after");
    run.git_detector.product_invocations=await Bun.file(marker).exists()?1:0;
    await json(join(out,"summary.json"),analyze(run,records));
  } catch(e) {
    run.complete=false;run.failure=e.message;run.elapsed_ms=performance.now()-started;
    console.error(e.message);
    process.exitCode=1;
  } finally {
    run.host.load_after=loadavg();
    if(await Bun.file(marker).exists()) run.git_detector.product_invocations=1;
    await json(join(out,"run.json"),run);
    await seal(out);
    // Incomplete fixtures are retained for investigation. Successful ones are already removed.
    if(run.complete) await rm(work,{recursive:true});
    console.log(JSON.stringify({out,complete:run.complete,workdir:work,elapsed_ms:run.elapsed_ms}));
  }
}

if(import.meta.main) {
  const [cmd,out,source]=process.argv.slice(2);
  if(cmd!=="run"||!out) throw Error("usage: bun run.mjs run NEW_DIR DEVELOPER_SOURCE_LABEL");
  await run(resolve(out),source);
}
