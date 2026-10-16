import { readEvidenceDirectory, checkProfileFixture, checkProfileLoop } from "./verify.mjs";
import { isDeepStrictEqual } from "node:util";

const assert=(ok,message)=>{if(!ok) throw Error(message);};
export async function verifyProfiles(dir) {
  const bytes=await readEvidenceDirectory(dir);
  const result=JSON.parse(bytes.get("profile.json"));
  assert(!result.failure && result.elapsed_ms<=600000,"INCONCLUSIVE incomplete profiling run");
  assert(result.metadata?.profile==="release" && result.metadata?.opt_level==="3" &&
    !result.metadata.debug_assertions,"ordinary optimized profile executable required");
  assert(/^[a-f0-9]{40}$/.test(result.source_label) && /^[a-f0-9]{64}$/.test(result.binary_sha256),"profile identity");
  const context={pools:new Map(),checkpoints:new Set(),empty:undefined};
  const loops=[],reports=[];
  const parse=name=>{
    assert(bytes.has(name),"missing profile file "+name);
    return bytes.get(name).toString().trim().split("\n").map(s=>JSON.parse(s));
  };
  let index=0;
  for(const action of ["head","stale"]) {
    let control=null;
    for(const mode of ["control","cpu","heap"]) {
      const prep=result.runs[index++];
      assert(prep?.kind==="prepare"&&prep.action===action&&prep.mode===mode&&prep.exit_code===0&&!prep.reason,
        "profile prepare coverage or exit");
      const fixture=checkProfileFixture(parse(prep.stdout),context);
      const run=result.runs[index++];
      assert(run?.action===action && run.mode===mode,"profile run coverage");
      if(run.status==="INCONCLUSIVE" && !run.stdout) {
        assert(mode!=="control" && !result.tools[mode==="cpu"?"perf":"heaptrack"].path,"missing available profiler run");
        reports.push({action,mode,status:"INCONCLUSIVE",reason:run.reason});continue;
      }
      const rows=run.exit_code!==0||run.reason?[]:parse(run.stdout);
      const loop=rows.find(r=>r.type==="profile_loop");
      if(run.exit_code!==0 || run.reason || !loop) {
        assert(mode!=="control","INCONCLUSIVE unprofiled control failed");
        reports.push({action,mode,status:"INCONCLUSIVE",reason:"profiler command failed; raw stderr retained"});
        continue;
      }
      assert(rows.length===2&&rows[0].type==="profile_start","unexpected profile events");
      assert(isDeepStrictEqual(loop,run.loop),"profile summary differs from raw loop");
      checkProfileLoop(loop,fixture,context.empty,action);
      if(mode==="control") control=loop;
      assert(control,"profile control order");
      loops.push({action,mode,wall_ns:loop.wall_ns,cpu_ns:loop.cpu_ns,peak_rss_bytes:loop.peak_rss_bytes,
        wall_over_control:loop.wall_ns/control.wall_ns,cpu_over_control:loop.cpu_ns/control.cpu_ns});
      if(mode!=="control") {
        let usable=false;
        if(result.platform==="darwin") {
          usable=run.attached?.length===1 && run.attached[0].exit_code===0 && !run.attached[0].reason;
          if(mode==="cpu") usable=usable&&(bytes.get(action+"-cpu.sample.txt")?.length??0)>0;
        } else if(mode==="cpu") {
          const report=result.runs[index++];
          assert(report?.kind==="report"&&report.action===action&&report.mode===mode,"perf report coverage");
          usable=report.exit_code===0 && (bytes.get(report.stdout)?.length??0)>0;
        } else usable=[...bytes.keys()].some(n=>n.startsWith(action+"-heap.heaptrack"));
        reports.push({action,mode,status:usable?"RAW_REPORT_RECORDED":"INCONCLUSIVE",
          note:"Inspect actual symbols and sampling scope before attributing costs."});
      }
    }
  }
  assert(index===result.runs.length,"unexpected profile records");
  return {format:1,source_label:result.source_label,binary_sha256:result.binary_sha256,loops,reports};
}

if(import.meta.main) {
  try {console.log(JSON.stringify(await verifyProfiles(process.argv[2])));}
  catch(e) {console.error(e.message);process.exitCode=1;}
}
