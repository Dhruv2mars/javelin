import { mkdir, mkdtemp, readFile, writeFile, rm } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { tmpdir, platform } from "node:os";
import { fileURLToPath } from "node:url";
import { createHash } from "node:crypto";
import { bounded, diskBytes, seal, budget } from "./run.mjs";

const here=dirname(fileURLToPath(import.meta.url));
const binary=join(here,"target/release/javelin-native-profiles");
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
const digest=x=>createHash("sha256").update(x).digest("hex");
const json=(path,v)=>writeFile(path,JSON.stringify(v,null,2)+"\n",{flag:"wx"});
const events=async path=>(await readFile(path,"utf8")).trim().split("\n").filter(Boolean).map(s=>JSON.parse(s));

async function main(out,source) {
  await mkdir(out);
  const work=await mkdtemp(join(tmpdir(),"javelin-cpu-profile-"));
  const start=performance.now();
  const result={format:1,source_label:source,preregistration:"b493add",
    binary_sha256:digest(await readFile(binary)),platform:platform(),case:"depth64",
    build:"same unchanged ordinary release executable; no Rust allocator instrumentation",
    scope:"400 public history calls or 1 stale call plus 399 exact stale replays, seeded in a separate process",
    tools:{},runs:[],attribution:"INCONCLUSIVE until raw stacks/heap reports inspected",
    overhead:"separate profiled loops compared with unprofiled fixed-iteration loops; excluded from ordinary timings"};
  for(const name of ["sample","heap","perf","heaptrack","valgrind","xctrace"]) {
    result.tools[name]={path:Bun.which(name),checked:["PATH","/usr/bin/"+name,"/usr/local/bin/"+name]};
  }
  const deadline=start+budget.total_wall_ms;
  async function invoke(id,cmd) {
    const outPath=join(out,id+".stdout"),errPath=join(out,id+".stderr");
    const r=await bounded(cmd,{out:outPath,err:errPath,timeout:Math.min(90000,Math.max(1,deadline-performance.now())),
      onPoll:async()=>{
        if(performance.now()>deadline) return "total_wall_cap";
        const active=await diskBytes(work), retained=await diskBytes(out);
        if(active>budget.active_fixture_bytes || active+retained>budget.retained_bytes) return "disk_cap";
        return null;
      }});
    return {id,cmd,stdout:id+".stdout",stderr:id+".stderr",...r};
  }
  try {
    const meta=await invoke("metadata",[binary,"metadata"]);
    result.metadata=(await events(join(out,meta.stdout)))[0];
    if(result.metadata.debug_assertions||result.metadata.profile!=="release") throw Error("release required");
    for(const action of ["head","stale"]) {
      for(const mode of ["control","cpu","heap"]) {
        const id=action+"-"+mode,fixture=join(work,id);
        const prep=await invoke(id+"-prepare",[binary,"prepare",fixture,"depth64"]);
        result.runs.push({...prep,kind:"prepare",action,mode});
        if(prep.exit_code!==0) throw Error("profile fixture prepare failed");
        let command=[binary,"profile",fixture,action],tool=null;
        if(mode==="cpu" && platform()!=="darwin") {
          tool=result.tools.perf.path;
          if(tool) command=[tool,"record","-e","cpu-clock","-F","99","-g","-o",join(out,id+".perf.data"),"--",...command];
        }
        if(mode==="heap" && platform()!=="darwin") {
          tool=result.tools.heaptrack.path;
          if(tool) command=[tool,"-o",join(out,id+".heaptrack"),...command];
        }
        if(mode!=="control"&&platform()!=="darwin"&&!tool) {
          result.runs.push({id,action,mode,status:"INCONCLUSIVE",reason:"profiler unavailable; checked tool paths retained"});
          await rm(fixture,{recursive:true});continue;
        }
        const pending=invoke(id,command);
        const attached=[];
        if(platform()==="darwin"&&mode!=="control") {
          let pid=null;
          for(let i=0;i<100;i++) {
            const rows=await events(join(out,id+".stdout")).catch(()=>[]);
            pid=rows.find(r=>r.type==="profile_start")?.pid;
            if(pid) break;
            await sleep(20);
          }
          const profiler=mode==="cpu"?result.tools.sample.path:result.tools.heap.path;
          if(pid&&profiler) {
            const args=mode==="cpu"?[profiler,String(pid),"5","1","-file",join(out,id+".sample.txt")]:
              [profiler,"-s","--showSizes",String(pid)];
            attached.push(await invoke(id+"-attach",args));
          } else attached.push({status:"INCONCLUSIVE",reason:"profiler unavailable or loop ended before attach"});
        }
        const r=await pending;
        const rows=await events(join(out,r.stdout)).catch(()=>[]);
        const loop=rows.find(x=>x.type==="profile_loop");
        result.runs.push({...r,action,mode,kind:"loop",attached,loop:loop??null,
          status:r.exit_code===0&&loop?"RECORDED":"INCONCLUSIVE"});
        if(mode==="cpu"&&tool&&r.exit_code===0) {
          result.runs.push({...await invoke(id+"-report",[tool,"report","--stdio","-i",join(out,id+".perf.data")]),kind:"report",action,mode});
        }
        await rm(fixture,{recursive:true});
      }
    }
  } catch(e) {result.failure=e.message;process.exitCode=1;}
  finally {
    result.elapsed_ms=performance.now()-start;
    await json(join(out,"profile.json"),result);await seal(out);
    console.log(JSON.stringify({out,elapsed_ms:result.elapsed_ms,failure:result.failure??null}));
  }
}

if(import.meta.main) {
  const [out,source]=process.argv.slice(2);
  if(!out||!source) throw Error("usage: bun profile.mjs NEW_DIR DEVELOPER_SOURCE_LABEL");
  await main(resolve(out),source);
}
