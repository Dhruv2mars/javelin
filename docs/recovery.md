# Recovery

The Javelin Store, not a Managed view, selects accepted truth. If a process stops before the Publish database commit, the target remains old. If it stops after commit, the new target remains accepted even when view update did not finish.

## Diagnose

```sh
javelin doctor --json
javelin fsck --json
```

`doctor` reports version, platform, architecture, Current World, database path, Monitor readiness, and materialization backend. `fsck` runs SQLite integrity and foreign-key checks, validates every object header, zstd stream, length, domain hash, root/blob reference type, and portable tree metadata.

## Rebuild views

```sh
javelin repair
javelin repair --view api
```

Repair invalidates the selected immutable root cache and reconstructs each Managed view from the retained Layer head. It fixes reconstructable view and cache damage. It cannot recreate a missing or corrupt canonical object; restore that store from backup instead.

## Discard recovery

```sh
javelin discarded list
javelin discarded recover experiment
javelin discarded purge throwaway
```

Discarded named Layers remain retained for seven days by default. Recover recreates the view. Purge validates the exact Layer target, then irreversibly deletes its retained metadata; unreferenced objects become eligible for garbage collection.

## Retention

```sh
javelin gc --dry-run
javelin gc
```

GC expires claims, raw provenance past policy, and discarded Layers past grace, then removes unreachable objects. World Versions, active and retained Layer Checkpoints, Conflict evidence, validation output, and retained provenance stay reachable.

When investigating corruption, copy the complete project including `.javelin/` before attempting recovery. Never replace canonical object bytes with materialized-view bytes.
