import { test, expect } from "bun:test";
import { mkdtemp, mkdir, writeFile, rm, symlink } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { diskBytes, bounded } from "./run.mjs";

test("disk monitoring tolerates disappearing SQLite journals and rejects missing or unsafe input", async()=>{
  const dir=await mkdtemp(join(tmpdir(),"javelin-disk-monitor-"));
  try {
    const store=join(dir,".javelin");await mkdir(store);
    await writeFile(join(store,"repository.sqlite3"),"retained");
    const journal=join(store,"repository.sqlite3-journal");
    const writer=(async()=>{
      for(let i=0;i<250;i++) {
        await writeFile(journal,"transient");
        await rm(journal);
      }
    })();
    for(let i=0;i<250;i++) expect(await diskBytes(dir)).toBeGreaterThanOrEqual(8);
    await writer;
    expect(await diskBytes(dir)).toBe(8);
    await expect(diskBytes(join(dir,"missing"))).rejects.toThrow();
    await symlink("missing",journal);
    await expect(diskBytes(dir)).rejects.toThrow("nonregular");
  } finally {await rm(dir,{recursive:true,force:true});}
});

test("bounded child timeout terminates its process group and retains output files", async()=>{
  const dir=await mkdtemp(join(tmpdir(),"javelin-child-timeout-"));
  try {
    const result=await bounded(["/bin/sleep","5"],{out:join(dir,"stdout"),err:join(dir,"stderr"),timeout:100});
    expect(result.timed_out).toBe(true);
    expect(result.reason).toBe("wall_timeout");
    expect(result.exit_code).not.toBe(0);
    expect(await Bun.file(join(dir,"stdout")).exists()).toBe(true);
    expect(await Bun.file(join(dir,"stderr")).exists()).toBe(true);
  } finally {await rm(dir,{recursive:true,force:true});}
});
