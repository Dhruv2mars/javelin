"""Summarizes retained Linux Layer runtime evidence; a missing part is never VERIFIED."""
import json
from pathlib import Path
import re
import sys

evidence = Path(sys.argv[1])


def load(name):
    try:
        return json.loads((evidence / name).read_text())
    except (OSError, ValueError):
        return None


log = (evidence / "mount-tests.log").read_text() if (evidence / "mount-tests.log").exists() else ""
totals = re.findall(r"test result: (\w+)\. (\d+) passed; (\d+) failed", log)
checks = {name: (load(f"{name}.json") or {}).get("verdict", "NOT VERIFIED")
          for name in ["flow", "recovery", "probe", "stale", "race", "replay", "init-names", "foreign", "reused", "sameid", "orphanfreeze", "transient"]}
cleanup = load("cleanup.json") or {}
initial = load("initial.json") or {}
verified = (cleanup.get("exitCode") == 0 and cleanup.get("allUnmountedAndRemoved") is True
            and all(v == "VERIFIED" for v in checks.values())
            and totals and totals[-1][0] == "ok" and totals[-1][2] == "0")
summary = {
    "verdict": "VERIFIED" if verified else "NOT VERIFIED",
    "scope": "bounded Linux Layer runtime binding on one hosted runner",
    "sourceSHA": initial.get("sourceSHA"), "probeDependencySHA": initial.get("probeDependencySHA"),
    "checks": checks, "testTotals": totals, "cleanup": cleanup,
    # Observation, reported separately: does the read-only freeze exclude a shared
    # writable mapping whose descriptor was closed?
    "sharedWritableMappingDuringFreeze": (load("recovery.json") or {}).get("sharedWritableMapping"),
    "notClaimed": ["autosave", "busy-writer continuous editing", "Stack/refresh/Publish",
                   "macOS/Windows", "production privilege model", "hostile-code confinement",
                   "power loss", "network filesystems", "scale", "Gate 2", "GA"],
}
(evidence / "summary.json").write_text(json.dumps(summary, indent=1) + "\n")
print(summary["verdict"])
sys.exit(0 if verified else 1)
