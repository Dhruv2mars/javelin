use javelin::{
    AdvanceOutcome, FileEntry, LayerId, LayerState, Manifest, ObjectId, OperationId, RepoPath,
    Repository, VersionNumber,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Write},
    path::Path,
    thread,
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const PAYLOAD: usize = 262_144;
const RSS_CAP: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Fixture {
    case: String,
    depth: u64,
    entries: usize,
    shared: bool,
    distinct_manifests: bool,
    pool: Vec<String>,
    shared_manifests: Vec<String>,
    distinct_blob_manifests: Vec<String>,
    source_checkpoint: String,
}

fn emit(v: Value) {
    println!("{v}");
    io::stdout().flush().unwrap();
}

fn v(n: u64) -> VersionNumber {
    VersionNumber::new(n).unwrap()
}
fn op(s: &str) -> OperationId {
    OperationId::parse(s).unwrap()
}
fn oid(s: &str) -> ObjectId {
    ObjectId::parse(s).unwrap()
}
fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn data(index: usize, entries: usize) -> Vec<u8> {
    let mut state = 1_u64 ^ (index as u64 + 1).wrapping_mul(0x9e3779b97f4a7c15);
    let mut out = Vec::with_capacity(PAYLOAD / entries);
    while out.len() < PAYLOAD / entries {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(PAYLOAD / entries);
    out
}

fn cpu_ns() -> u64 {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: getrusage writes a correctly sized, writable rusage.
    assert_eq!(unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) }, 0);
    let ns = |t: libc::timeval| t.tv_sec as u64 * 1_000_000_000 + t.tv_usec as u64 * 1000;
    ns(usage.ru_utime) + ns(usage.ru_stime)
}

fn rss() -> u64 {
    #[cfg(target_os = "linux")]
    {
        let s = fs::read_to_string("/proc/self/status").unwrap();
        s.lines()
            .find(|l| l.starts_with("VmHWM:"))
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse::<u64>()
            .unwrap()
            * 1024
    }
    #[cfg(target_os = "macos")]
    {
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        // SAFETY: getrusage writes a correctly sized, writable rusage.
        assert_eq!(unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) }, 0);
        usage.ru_maxrss as u64
    }
}

fn limits(profile: bool) -> Result<()> {
    let cpu = libc::rlimit {
        rlim_cur: 60,
        rlim_max: 60,
    };
    // SAFETY: these limits affect only this disposable diagnostic child.
    if unsafe { libc::setrlimit(libc::RLIMIT_CPU, &cpu) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    #[cfg(target_os = "linux")]
    if !profile {
        let mem = libc::rlimit {
            rlim_cur: RSS_CAP,
            rlim_max: RSS_CAP,
        };
        // SAFETY: as above. Profilers may require a larger virtual address range.
        if unsafe { libc::setrlimit(libc::RLIMIT_AS, &mem) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
    }
    let _ = profile;
    Ok(())
}

fn timed<T>(phase: &str, f: impl FnOnce() -> javelin::Result<T>) -> javelin::Result<T> {
    let cpu = cpu_ns();
    let start = Instant::now();
    let result = f();
    let wall = start.elapsed().as_nanos() as u64;
    let cpu = cpu_ns() - cpu;
    emit(
        json!({"type":"timing","phase":phase,"wall_ns":wall,"cpu_ns":cpu,
        "peak_rss_bytes":rss(),"error":result.as_ref().err().map(|e|json!({"code":e.code(),"message":e.to_string()}))}),
    );
    if rss() > RSS_CAP {
        emit(json!({"type":"budget","reason":"rss_cap","peak_rss_bytes":rss()}));
        std::process::exit(124);
    }
    result
}

fn config(case: &str) -> Result<Fixture> {
    let (depth, entries, shared, distinct_manifests) = match case {
        "base" => (16, 32, true, false),
        "depth4" => (4, 32, true, false),
        "depth64" => (64, 32, true, false),
        "entries8" => (16, 8, true, false),
        "entries128" => (16, 128, true, false),
        "distinct-blobs" => (16, 32, false, false),
        "distinct-manifests" => (16, 32, true, true),
        _ => return Err("unknown case".into()),
    };
    Ok(Fixture {
        case: case.into(),
        depth,
        entries,
        shared,
        distinct_manifests,
        pool: vec![],
        shared_manifests: vec![],
        distinct_blob_manifests: vec![],
        source_checkpoint: String::new(),
    })
}

fn manifest(f: &Fixture, shared: bool, variant: usize) -> Manifest {
    Manifest::new((0..f.entries).map(|i| {
        (
            RepoPath::parse(&format!("f{i:03}")).unwrap(),
            FileEntry::Regular {
                content: oid(&f.pool[if shared { 0 } else { i }]),
                executable: i < 6 && (variant & (1 << i)) != 0,
            },
        )
    }))
    .unwrap()
}

impl Fixture {
    fn manifests(&self) -> &[String] {
        if self.shared {
            &self.shared_manifests
        } else {
            &self.distinct_blob_manifests
        }
    }
    fn at(&self, number: u64) -> ObjectId {
        let variant = if self.distinct_manifests {
            number % self.depth
        } else {
            0
        };
        oid(&self.manifests()[variant as usize])
    }
    fn candidate(&self) -> ObjectId {
        self.at(self.depth)
    }
}

fn verify_history(f: &Fixture, history: &[javelin::WorldVersion]) -> Result<()> {
    if history.len() < f.depth as usize + 1 {
        return Err("history shorter than fixture".into());
    }
    for (i, w) in history.iter().take(f.depth as usize + 1).enumerate() {
        if w.number.get() != i as u64 {
            return Err("noncontiguous history".into());
        }
        if i == 0 {
            if w.operation.is_some() {
                return Err("initial operation".into());
            }
        } else if w.manifest != f.at(i as u64)
            || w.operation.as_ref() != Some(&op(&format!("seed-{i}")))
        {
            return Err("seed history mismatch".into());
        }
    }
    Ok(())
}

fn prepare(root: &Path, case: &str) -> Result<()> {
    fs::create_dir(root)?;
    let mut f = config(case)?;
    let mut repo = timed("init", || Repository::init(root))?;
    let mut blobs = vec![];
    for i in 0..f.entries {
        let bytes = data(i, f.entries);
        let id = timed("put_blob", || repo.put_blob(&bytes))?;
        if repo.read_blob(id)? != bytes {
            return Err("blob readback mismatch".into());
        }
        f.pool.push(id.to_string());
        blobs.push(json!({"id":id,"length":bytes.len(),"sha256":sha(&bytes)}));
    }
    let mut manifests = vec![];
    for shared in [true, false] {
        for variant in 0..64 {
            let m = manifest(&f, shared, variant);
            let id = timed("put_manifest", || repo.put_manifest(&m))?;
            let read = repo.read_manifest(id)?;
            if read != m {
                return Err("manifest readback mismatch".into());
            }
            manifests.push(json!({"id":id,"shared":shared,"variant":variant,
                "canonical":serde_json::from_slice::<Value>(&m.canonical_bytes())?,
                "length":m.canonical_bytes().len()}));
            if shared {
                f.shared_manifests.push(id.to_string());
            } else {
                f.distinct_blob_manifests.push(id.to_string());
            }
        }
    }
    for n in 1..=f.depth {
        let result = timed("seed_advance", || {
            repo.compare_and_advance(&op(&format!("seed-{n}")), v(n - 1), f.at(n))
        })?;
        if !matches!(result,AdvanceOutcome::Accepted{ref version} if version.number==v(n) && version.manifest==f.at(n))
        {
            return Err("seed outcome mismatch".into());
        }
    }
    let base = timed("layer_base", || repo.layer_base(v(f.depth)))?;
    let head = timed("create_layer", || {
        repo.create_layer(&LayerId::parse("source").unwrap(), &base)
    })?;
    f.source_checkpoint = head.checkpoint().object().to_string();
    let history = repo.history()?;
    verify_history(&f, &history)?;
    let check = repo.check()?;
    fs::write(root.join("fixture.json"), serde_json::to_vec(&f)?)?;
    emit(
        json!({"type":"fixture","fixture":f,"blobs":blobs,"manifests":manifests,
        "history":history,"check":check,"payload_bytes":PAYLOAD,
        "verified_blob_reads":blobs.len(),"verified_manifest_reads":manifests.len()}),
    );
    Ok(())
}

fn load(root: &Path) -> Result<Fixture> {
    Ok(serde_json::from_slice(&fs::read(
        root.join("fixture.json"),
    )?)?)
}

fn advance(
    root: &Path,
    repo: &mut Repository,
    operation: &str,
    expected: u64,
    candidate: ObjectId,
    phase: &str,
) -> Result<AdvanceOutcome> {
    for attempt in 0..3 {
        let cpu = cpu_ns();
        let start = Instant::now();
        let result = repo.compare_and_advance(&op(operation), v(expected), candidate);
        let wall = start.elapsed().as_nanos() as u64;
        let cpu = cpu_ns() - cpu;
        emit(
            json!({"type":"call","phase":phase,"operation":operation,"expected":expected,
            "manifest":candidate,"attempt":attempt,"wall_ns":wall,"cpu_ns":cpu,"peak_rss_bytes":rss(),
            "result":match &result { Ok(o)=>json!({"ok":o}),Err(e)=>json!({"error":{"code":e.code(),"message":e.to_string()}})}}),
        );
        if rss() > RSS_CAP {
            return Err("RSS cap exceeded".into());
        }
        match result {
            Ok(outcome) => return Ok(outcome),
            Err(e)
                if matches!(e, javelin::Error::Storage(_) | javelin::Error::Io(_))
                    && attempt < 2 =>
            {
                let reopened = Repository::open(root);
                emit(
                    json!({"type":"reopen","phase":phase,"operation":operation,"after_attempt":attempt,
                    "error":reopened.as_ref().err().map(|e|json!({"code":e.code(),"message":e.to_string()}))}),
                );
                *repo = reopened?;
            }
            Err(e) => return Err(e.into()),
        }
    }
    unreachable!()
}

fn measure(root: &Path, action: &str, worker: &str) -> Result<()> {
    let f = load(root)?;
    let mut repo = timed("open", || Repository::open(root))?;
    let before = timed("head_discovery", || repo.history())?;
    verify_history(&f, &before)?;
    if before.len() != f.depth as usize + 1 {
        return Err("unexpected premeasurement head".into());
    }
    let negative_bytes = data(0, f.entries);
    if timed("put_blob_negative_control", || {
        repo.put_blob(&negative_bytes)
    })? != oid(&f.pool[0])
    {
        return Err("negative blob ID mismatch".into());
    }
    if action == "race" {
        fs::write(root.join(format!("ready-{worker}")), b"ready")?;
        let start = Instant::now();
        while !root.join("go").try_exists()? {
            if start.elapsed() > Duration::from_secs(10) {
                return Err("barrier timeout".into());
            }
            thread::sleep(Duration::from_millis(2));
        }
    }
    let expected = if action == "stale" {
        f.depth - 1
    } else {
        f.depth
    };
    let name = format!("{action}-{worker}");
    let mut outcome = Value::Null;
    let mut replay = Value::Null;
    let mut changed_error = Value::Null;
    let mut clone_head = Value::Null;
    match action {
        "head" => {}
        "accepted" | "stale" | "race" => {
            let got = advance(root, &mut repo, &name, expected, f.candidate(), "advance")?;
            let good = match (&got, action) {
                (AdvanceOutcome::Accepted { version }, "accepted" | "race") => {
                    version.number == v(f.depth + 1)
                        && version.manifest == f.candidate()
                        && version.operation.as_ref() == Some(&op(&name))
                }
                (AdvanceOutcome::Stale { actual }, "stale") => *actual == v(f.depth),
                (AdvanceOutcome::Stale { actual }, "race") => *actual == v(f.depth + 1),
                _ => false,
            };
            if !good {
                return Err("wrong advance outcome".into());
            }
            repo = timed("reopen_for_replay", || Repository::open(root))?;
            let again = advance(
                root,
                &mut repo,
                &name,
                expected,
                f.candidate(),
                "exact_replay",
            )?;
            if again != got {
                return Err("retry changed outcome".into());
            }
            let changed = repo.compare_and_advance(&op(&name), v(expected), oid(&f.manifests()[1]));
            changed_error = json!(changed.as_ref().err().map(|e| e.code()));
            if !matches!(changed, Err(javelin::Error::OperationIdReused(_))) {
                return Err("changed input accepted".into());
            }
            outcome = json!(got);
            replay = json!(again);
        }
        "clone" => {
            let head = timed("clone_layer", || {
                repo.clone_layer(
                    &LayerId::parse("clone").unwrap(),
                    javelin::CheckpointId::parse(&f.source_checkpoint).unwrap(),
                )
            })?;
            if head.checkpoint().object().to_string() != f.source_checkpoint || head.revision() != 0
            {
                return Err("clone mismatch".into());
            }
            let state = repo.layer_state(&LayerId::parse("clone")?)?;
            if state != (LayerState::Active { head: head.clone() }) {
                return Err("clone state mismatch".into());
            }
            if repo.read_checkpoint(head.checkpoint())?.manifest() != &manifest(&f, f.shared, 0) {
                return Err("clone contents mismatch".into());
            }
            clone_head = json!(head);
        }
        _ => return Err("unknown action".into()),
    }
    let after = repo.history()?;
    verify_history(&f, &after)?;
    let check = repo.check()?;
    emit(
        json!({"type":"measurement","action":action,"worker":worker,"fixture":f,
        "before":before,"after":after,"outcome":outcome,"replay":replay,
        "changed_error":changed_error,"clone_head":clone_head,"check":check}),
    );
    Ok(())
}

fn inspect(root: &Path) -> Result<()> {
    let f = load(root)?;
    let mut repo = Repository::open(root)?;
    let history = repo.history()?;
    verify_history(&f, &history)?;
    let mut receipts = vec![];
    for worker in ["0", "1"] {
        let name = format!("race-{worker}");
        let got = advance(
            root,
            &mut repo,
            &name,
            f.depth,
            f.candidate(),
            "race_final_replay",
        )?;
        receipts.push(json!({"worker":worker,"outcome":got}));
    }
    if receipts
        .iter()
        .filter(|r| r["outcome"]["outcome"] == "accepted")
        .count()
        != 1
    {
        return Err("race winner count".into());
    }
    if history.len() != f.depth as usize + 2 {
        return Err("race history length".into());
    }
    let check = repo.check()?;
    if check.attempts != f.depth as usize + 2 {
        return Err("race attempt count".into());
    }
    emit(
        json!({"type":"race_final","fixture":f,"history":history,"receipts":receipts,"check":check}),
    );
    Ok(())
}

fn profile(root: &Path, action: &str) -> Result<()> {
    let f = load(root)?;
    let mut repo = Repository::open(root)?;
    emit(json!({"type":"profile_start","pid":std::process::id(),"action":action}));
    let start = Instant::now();
    let cpu = cpu_ns();
    let mut iterations = 0;
    // Fixed iteration count makes profiler overhead comparable. No writes for head;
    // stale creates one durable outcome then replays exactly that operation.
    for _ in 0..400 {
        if rss() > 512 * 1024 * 1024 {
            return Err("profile RSS cap exceeded".into());
        }
        if action == "head" {
            let h = repo.history()?;
            verify_history(&f, &h)?;
        } else if action == "stale" {
            if repo.compare_and_advance(&op("profile-stale"), v(f.depth - 1), f.candidate())?
                != (AdvanceOutcome::Stale { actual: v(f.depth) })
            {
                return Err("profile stale".into());
            }
        } else {
            return Err("unknown profile action".into());
        }
        iterations += 1;
    }
    emit(
        json!({"type":"profile_loop","action":action,"iterations":iterations,
        "wall_ns":start.elapsed().as_nanos() as u64,"cpu_ns":cpu_ns()-cpu,"peak_rss_bytes":rss(),
        "history":repo.history()?,"check":repo.check()?}),
    );
    Ok(())
}

fn run() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    if args.get(1).map(String::as_str) == Some("metadata") {
        emit(
            json!({"type":"metadata","native_sha256":env!("NP_NATIVE"),"tool_sha256":env!("NP_TOOL"),
            "rustc":env!("NP_RUSTC"),"profile":env!("NP_PROFILE"),"opt_level":env!("NP_OPT_LEVEL"),
            "debug":env!("NP_DEBUG"),"target":env!("NP_TARGET"),"rustflags":env!("NP_CARGO_ENCODED_RUSTFLAGS"),
            "debug_assertions":cfg!(debug_assertions)}),
        );
        return Ok(());
    }
    if cfg!(debug_assertions) && std::env::var_os("NP_TEST_DEBUG").is_none() {
        return Err("release build required".into());
    }
    let command = args.get(1).ok_or("command required")?;
    let root = Path::new(args.get(2).ok_or("root required")?);
    limits(command == "profile")?;
    match command.as_str() {
        "prepare" => prepare(root, args.get(3).ok_or("case required")?),
        "measure" => measure(
            root,
            args.get(3).ok_or("action required")?,
            args.get(4).map(String::as_str).unwrap_or("0"),
        ),
        "inspect" => inspect(root),
        "profile" => profile(root, args.get(3).ok_or("action required")?),
        _ => Err("unknown command".into()),
    }
}

fn main() {
    if let Err(e) = run() {
        emit(json!({"type":"failure","message":e.to_string()}));
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shape_and_content_are_bounded_and_deterministic() {
        for c in [
            "base",
            "depth4",
            "depth64",
            "entries8",
            "entries128",
            "distinct-blobs",
            "distinct-manifests",
        ] {
            let f = config(c).unwrap();
            assert_eq!(data(0, f.entries).len() * f.entries, PAYLOAD);
            assert_eq!(data(0, f.entries), data(0, f.entries));
            assert_ne!(data(0, f.entries), data(1, f.entries));
        }
        assert!(config("oversized").is_err());
    }
}
