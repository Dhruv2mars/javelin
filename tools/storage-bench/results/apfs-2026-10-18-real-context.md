# Storage benchmark

```text
APFS (30 GiB image; real-context supplement; native ingest)
ProductName:		macOS
ProductVersion:		26.6.2
BuildVersion:		25G83
CPU: Apple M1
RAM bytes: 17179869184
Device Identifier:         disk7s1
   Device Node:               /dev/disk7s1
   Whole:                     No
   Part of Whole:             disk7

   Volume Name:               jbench-storage
   Mounted:                   Yes
   Mount Point:               /Volumes/jbench-storage

   Partition Type:            41504653-0000-11AA-AA11-00306543ECAC
   File System Personality:   APFS
   Type (Bundle):             apfs
   Name (User Visible):       APFS
   Owners:                    Disabled

   OS Can Be Installed:       Yes
   Media Type:                Generic
   Protocol:                  Disk Image
   SMART Status:              Not Supported
   Volume UUID:               B6C5CADF-02D5-43C0-9C91-789D2ABC772F
   Disk / Partition UUID:     B6C5CADF-02D5-43C0-9C91-789D2ABC772F

   Disk Size:                 32.0 GB (32002498560 Bytes) (exactly 62504880 512-Byte-Units)
   Device Block Size:         4096 Bytes

   Volume Used Space:         28.7 KB (28672 Bytes) (exactly 56 512-Byte-Units)
   Container Total Space:     32.0 GB (32002498560 Bytes) (exactly 62504880 512-Byte-Units)
   Container Free Space:      31.9 GB (31927320576 Bytes) (exactly 62358048 512-Byte-Units)
   Allocation Block Size:     4096 Bytes

   Media OS Use Only:         No
   Media Read-Only:           No
   Volume Read-Only:          No

   Device Location:           External
   Removable Media:           Removable
   Media Removal:             Software-Activated

   Solid State:               Info not available

   This disk is an APFS Volume.  APFS Information:
   APFS Container:            disk7
   APFS Physical Store:       disk6s2
   Encrypted:                 No
   FileVault:                 No
   Sealed:                    No
   Locked:                    No
Darwin d2m.local 25.6.0 Darwin Kernel Version 25.6.0: Fri Jul 31 19:17:12 PDT 2026; root:xnu-12377.161.14~5/RELEASE_ARM64_T8103 arm64
rustc 1.93.0 (254b59607 2026-01-19)
Filesystem      Size    Used   Avail Capacity iused ifree %iused  Mounted on
/dev/disk7s1    30Gi    28Ki    30Gi     1%       1  312M    0%   /Volumes/jbench-storage
cache hook: /tmp/storage-cache-reset.sh
Args: ["tools/storage-bench/target/release/storage-bench", "--dir", "/Volumes/jbench-storage/real-context-run", "--ingest-dir", "/tmp/jbench-real-context-ingest", "--reps", "5", "--page-size", "4096", "--label", "APFS (30 GiB image; real-context supplement; native ingest)", "--drop-caches-cmd", "/tmp/storage-cache-reset.sh", "--json", "tools/storage-bench/results/apfs-2026-10-18-real-context.json", "--files", "100000", "--stores", "A-real", "--real10k-ms", "218169.449291"]
INGEST LOCATION (all stores): /tmp/jbench-real-context-ingest
separate ingest volume; materialize/disk on --dir
ProductName:		macOS
ProductVersion:		26.6.2
BuildVersion:		25G83
CPU: Apple M1
RAM bytes: 17179869184
Device Identifier:         disk3s1
   Device Node:               /dev/disk3s1
   Whole:                     No
   Part of Whole:             disk3

   Volume Name:               Macintosh HD - Data
   Mounted:                   Yes
   Mount Point:               /System/Volumes/Data

   Partition Type:            41504653-0000-11AA-AA11-00306543ECAC
   File System Personality:   APFS
   Type (Bundle):             apfs
   Name (User Visible):       APFS
   Owners:                    Enabled

   OS Can Be Installed:       Yes
   Booter Disk:               disk3s4
   Recovery Disk:             disk3s5
   Media Type:                Generic
   Protocol:                  Apple Fabric
   SMART Status:              Verified
   Volume UUID:               C150C628-FD63-4B1E-AC70-50E178FA6A63
   Disk / Partition UUID:     C150C628-FD63-4B1E-AC70-50E178FA6A63

   Disk Size:                 494.4 GB (494384795648 Bytes) (exactly 965595304 512-Byte-Units)
   Device Block Size:         4096 Bytes

   Volume Used Space:         410.9 GB (410946887680 Bytes) (exactly 802630640 512-Byte-Units)
   Container Total Space:     494.4 GB (494384795648 Bytes) (exactly 965595304 512-Byte-Units)
   Container Free Space:      59.2 GB (59215392768 Bytes) (exactly 115655064 512-Byte-Units)
   Allocation Block Size:     4096 Bytes

   Media OS Use Only:         No
   Media Read-Only:           No
   Volume Read-Only:          No

   Device Location:           Internal
   Removable Media:           Fixed

   Solid State:               Yes
   Hardware AES Support:      Yes

   This disk is an APFS Volume.  APFS Information:
   APFS Container:            disk3
   APFS Physical Store:       disk0s2
   APFS Volume Group:         C150C628-FD63-4B1E-AC70-50E178FA6A63
   FileVault:                 Yes
   Sealed:                    No
   Locked:                    No
Darwin d2m.local 25.6.0 Darwin Kernel Version 25.6.0: Fri Jul 31 19:17:12 PDT 2026; root:xnu-12377.161.14~5/RELEASE_ARM64_T8103 arm64
rustc 1.93.0 (254b59607 2026-01-19)
Filesystem      Size    Used   Avail Capacity iused ifree %iused  Mounted on
/dev/disk3s1   460Gi   383Gi    55Gi    88%     16M  578M    3%   /System/Volumes/Data
cache hook: none: cold unmeasured
Args: ["tools/storage-bench/target/release/storage-bench", "--dir", "/Volumes/jbench-storage/real-context-run", "--ingest-dir", "/tmp/jbench-real-context-ingest", "--reps", "5", "--page-size", "4096", "--label", "APFS (30 GiB image; real-context supplement; native ingest)", "--drop-caches-cmd", "/tmp/storage-cache-reset.sh", "--json", "tools/storage-bench/results/apfs-2026-10-18-real-context.json", "--files", "100000", "--stores", "A-real", "--real10k-ms", "218169.449291"]
```

| files | corpus bytes | store | operation | unit | samples | median/p50 | p95 | p99 | note |
| ---: | ---: | --- | --- | --- | ---: | ---: | ---: | ---: | --- |
| 100 | 0 | B/durable | crash-sigkill | passed | 1 | 1.000 | 1.000 | 1.000 | 100 complete finals hash-verified; partial tmp detected and swept; deterministic pre-rename boundary, not power-loss proof |
| 1000 | 7001918 | A-tuned-pilot | ingest/page-4096 | ms | 5 | 55.359 | 58.173 | 58.173 | durable FULL; 5 fresh stores; choose fastest median |
| 1000 | 7001918 | A-tuned-pilot | ingest/page-16384 | ms | 5 | 51.014 | 54.844 | 54.844 | durable FULL; 5 fresh stores; choose fastest median |
| 1000 | 7001918 | A-tuned-pilot | read/select | ms | 5 | 3.629 | 4.706 | 4.706 | warm 1000 reads; cached statement; includes lookup and Vec allocation |
| 1000 | 7001918 | A-tuned-pilot | read/incremental | ms | 5 | 5.328 | 6.093 | 6.093 | warm 1000 reads; cached statement; includes lookup and Vec allocation |
| 0 | 0 | configuration | chosen-page-size | bytes | 1 | 4096.000 | 4096.000 | 4096.000 | 64MiB cache; 2GiB mmap; batch 1000; incremental-read=false |
| 100000 | 697364836 | corpus | sha256 | digest | 0 | — | — | — | 25ab49fa2369c3a3fbf53bf5da1d15e8fe9431c8177f389e0734dd5761ea5010 |
| 100000 | 697364836 | corpus | rounded-4k-payload | bytes | 1 | 932982784.000 | 932982784.000 | 932982784.000 | deterministic 4KiB payload-block bound; excludes inode/directory metadata; filesystem inline data may differ |
| 100000 | 697364836 | A-real | ingest/extrapolated-from-10k | ms | 1 | 2181694.493 | 2181694.493 | 2181694.493 | NOT measured; projected >15min. Read/materialize context uses untimed batched SQL seeding into real schema, then real Repository API reads |
| 100000 | 697364836 | A-real | setup/real-read-context | setup | 0 | — | — | — | 100k objects batch-seeded outside timings; read_blob is actual API; production per-blob ingest is not claimed |
| 100000 | 697364836 | A-real | disk/store | corpus-x | 5 | 1.131 | 1.134 | 1.134 | fresh independent allocations from closed seed; sync+settle; APFS directory clone / Linux per-file reflink Layers |
| 100000 | 697364836 | A-real | disk/store+base | corpus-x | 5 | 2.546 | 2.549 | 2.549 | fresh independent allocations from closed seed; sync+settle; APFS directory clone / Linux per-file reflink Layers |
| 100000 | 697364836 | A-real | disk/store+base+1layers | corpus-x | 5 | 2.600 | 2.603 | 2.603 | fresh independent allocations from closed seed; sync+settle; APFS directory clone / Linux per-file reflink Layers |
| 100000 | 697364836 | A-real | disk/store+base+5layers | corpus-x | 5 | 2.817 | 2.820 | 2.820 | fresh independent allocations from closed seed; sync+settle; APFS directory clone / Linux per-file reflink Layers |
| 100000 | 697364836 | A-real | disk/store-apparent | bytes | 5 | 774451200.000 | 774451200.000 | 774451200.000 | sum st_size; includes db + wal; logical, not exclusive blocks |
| 100000 | 697364836 | A-real | disk/sqlite-db-wal | bytes | 5 | 774451200.000 | 774451200.000 | 774451200.000 | closed checkpointed DB plus any WAL/SHM |
| 100000 | 697364836 | A-real | disk/apparent-base+1layers | bytes | 5 | 1394729672.000 | 1394729672.000 | 1394729672.000 | apparent sizes count clone contents repeatedly |
| 100000 | 697364836 | A-real | disk/apparent-base+5layers | bytes | 5 | 4184189016.000 | 4184189016.000 | 4184189016.000 | apparent sizes count clone contents repeatedly |
| 100000 | 697364836 | A-real | context/base-to-layer | ms | 5 | 6794.500 | 8159.772 | 8159.772 | not comparison target; APFS directory clone / Linux per-file reflink |
| 100000 | 697364836 | A-real | materialize/read-write/warm | ms | 5 | 23001.971 | 28332.938 | 28332.938 | includes dirs and writable chmod; excludes store-handle opening, verification, sync and cleanup |
| 100000 | 697364836 | A-real | base-physical/read-write/warm | bytes | 5 | 987942912.000 | 988250112.000 | 988250112.000 | statfs allocation delta; clone metadata included |
| 100000 | 697364836 | A-real | materialize/read-write/cold-start | ms | 5 | 21946.166 | 31902.990 | 31902.990 | includes dirs and writable chmod; excludes store-handle opening, verification, sync and cleanup |
| 100000 | 697364836 | A-real | base-physical/read-write/cold-start | bytes | 5 | 987336704.000 | 987754496.000 | 987754496.000 | statfs allocation delta; clone metadata included |
| 100000 | 697364836 | A-real | incremental/1/read-write | ms | 5 | 0.183 | 0.401 | 0.401 | 1 = modified; 100 = 34 modified + 33 deleted + 33 added; atomic temp+rename, no base fsync |
| 100000 | 697364836 | A-real | incremental/100/read-write | ms | 5 | 12.176 | 21.696 | 21.696 | 1 = modified; 100 = 34 modified + 33 deleted + 33 added; atomic temp+rename, no base fsync |
| 100000 | 697364836 | A-real | random-read/warm | us | 5000 | 13.833 | 34.458 | 69.958 | p50 and p99 over 1000 reads/rep; cold-start drops cache once per sequence, then warms; open store handle excluded |
| 100000 | 697364836 | A-real | random-read/cold-start | us | 5000 | 305.042 | 1089.458 | 3653.500 | p50 and p99 over 1000 reads/rep; cold-start drops cache once per sequence, then warms; open store handle excluded |
| 100000 | 697364836 | checkpoint-context | layer-construction | context | 0 | — | — | — | actual APFS directory clone / Linux per-file reflink; base remains live during edits |
| 100000 | 697364836 | checkpoint-context | manifest-canonical-size | bytes | 1 | 14233271.000 | 14233271.000 | 14233271.000 | real Manifest::canonical_bytes, 100k entries; capture itself writes sparse checkpoint metadata, not a fresh full manifest |
| 100000 | 697364836 | A-real | checkpoint-10/blobs-only | ms | 20 | 324.821 | 416.755 | 492.358 | 20 reps; 5 single-byte appends + 5 same-size first-byte rewrites; file reads, canonical hashes, store commits included; no stat scan/serialization/cache-save/head CAS; real uses per-blob API, tuned single transaction, split keeps metadata in WAL FULL SQLite; with-checkpoint writes canonical-shaped sparse metadata row; with-manifest adds requested conservative full-tree metadata proxy instead; neither executes graph validation/head publication |
| 100000 | 697364836 | A-real | checkpoint-stat-scan/blobs-only | ms | 20 | 311.226 | 399.451 | 1135.301 | 100k metadata-only lstat walk alongside each 10-edit trial, following capture_bench baseline; not the complete production scan/cache algorithm |
| 100000 | 697364836 | A-real | checkpoint-10/with-checkpoint | ms | 20 | 337.584 | 396.173 | 653.877 | 20 reps; 5 single-byte appends + 5 same-size first-byte rewrites; file reads, canonical hashes, store commits included; no stat scan/serialization/cache-save/head CAS; real uses per-blob API, tuned single transaction, split keeps metadata in WAL FULL SQLite; with-checkpoint writes canonical-shaped sparse metadata row; with-manifest adds requested conservative full-tree metadata proxy instead; neither executes graph validation/head publication |
| 100000 | 697364836 | A-real | checkpoint-stat-scan/with-checkpoint | ms | 20 | 325.068 | 392.017 | 470.365 | 100k metadata-only lstat walk alongside each 10-edit trial, following capture_bench baseline; not the complete production scan/cache algorithm |
| 100000 | 697364836 | A-real | checkpoint-10/with-manifest | ms | 20 | 444.098 | 638.292 | 644.603 | 20 reps; 5 single-byte appends + 5 same-size first-byte rewrites; file reads, canonical hashes, store commits included; no stat scan/serialization/cache-save/head CAS; real uses per-blob API, tuned single transaction, split keeps metadata in WAL FULL SQLite; with-checkpoint writes canonical-shaped sparse metadata row; with-manifest adds requested conservative full-tree metadata proxy instead; neither executes graph validation/head publication |
| 100000 | 697364836 | A-real | checkpoint-stat-scan/with-manifest | ms | 20 | 330.397 | 384.546 | 462.766 | 100k metadata-only lstat walk alongside each 10-edit trial, following capture_bench baseline; not the complete production scan/cache algorithm |
| 100000 | 697364836 | A-tuned/durable | checkpoint-10/blobs-only | ms | 20 | 5.333 | 12.608 | 59.812 | 20 reps; 5 single-byte appends + 5 same-size first-byte rewrites; file reads, canonical hashes, store commits included; no stat scan/serialization/cache-save/head CAS; real uses per-blob API, tuned single transaction, split keeps metadata in WAL FULL SQLite; with-checkpoint writes canonical-shaped sparse metadata row; with-manifest adds requested conservative full-tree metadata proxy instead; neither executes graph validation/head publication |
| 100000 | 697364836 | A-tuned/durable | checkpoint-stat-scan/blobs-only | ms | 20 | 301.412 | 344.729 | 383.867 | 100k metadata-only lstat walk alongside each 10-edit trial, following capture_bench baseline; not the complete production scan/cache algorithm |
| 100000 | 697364836 | A-tuned/durable | checkpoint-10/with-checkpoint | ms | 20 | 5.455 | 7.010 | 7.499 | 20 reps; 5 single-byte appends + 5 same-size first-byte rewrites; file reads, canonical hashes, store commits included; no stat scan/serialization/cache-save/head CAS; real uses per-blob API, tuned single transaction, split keeps metadata in WAL FULL SQLite; with-checkpoint writes canonical-shaped sparse metadata row; with-manifest adds requested conservative full-tree metadata proxy instead; neither executes graph validation/head publication |
| 100000 | 697364836 | A-tuned/durable | checkpoint-stat-scan/with-checkpoint | ms | 20 | 300.376 | 329.651 | 447.124 | 100k metadata-only lstat walk alongside each 10-edit trial, following capture_bench baseline; not the complete production scan/cache algorithm |
| 100000 | 697364836 | A-tuned/durable | checkpoint-10/with-manifest | ms | 20 | 73.918 | 104.817 | 115.932 | 20 reps; 5 single-byte appends + 5 same-size first-byte rewrites; file reads, canonical hashes, store commits included; no stat scan/serialization/cache-save/head CAS; real uses per-blob API, tuned single transaction, split keeps metadata in WAL FULL SQLite; with-checkpoint writes canonical-shaped sparse metadata row; with-manifest adds requested conservative full-tree metadata proxy instead; neither executes graph validation/head publication |
| 100000 | 697364836 | A-tuned/durable | checkpoint-stat-scan/with-manifest | ms | 20 | 305.146 | 363.406 | 447.140 | 100k metadata-only lstat walk alongside each 10-edit trial, following capture_bench baseline; not the complete production scan/cache algorithm |
| 100000 | 697364836 | B/durable | checkpoint-10/blobs-only | ms | 20 | 3.812 | 24.906 | 27.927 | 20 reps; 5 single-byte appends + 5 same-size first-byte rewrites; file reads, canonical hashes, store commits included; no stat scan/serialization/cache-save/head CAS; real uses per-blob API, tuned single transaction, split keeps metadata in WAL FULL SQLite; with-checkpoint writes canonical-shaped sparse metadata row; with-manifest adds requested conservative full-tree metadata proxy instead; neither executes graph validation/head publication |
| 100000 | 697364836 | B/durable | checkpoint-stat-scan/blobs-only | ms | 20 | 306.305 | 469.779 | 799.310 | 100k metadata-only lstat walk alongside each 10-edit trial, following capture_bench baseline; not the complete production scan/cache algorithm |
| 100000 | 697364836 | B/durable | checkpoint-10/with-checkpoint | ms | 20 | 4.746 | 29.899 | 38.438 | 20 reps; 5 single-byte appends + 5 same-size first-byte rewrites; file reads, canonical hashes, store commits included; no stat scan/serialization/cache-save/head CAS; real uses per-blob API, tuned single transaction, split keeps metadata in WAL FULL SQLite; with-checkpoint writes canonical-shaped sparse metadata row; with-manifest adds requested conservative full-tree metadata proxy instead; neither executes graph validation/head publication |
| 100000 | 697364836 | B/durable | checkpoint-stat-scan/with-checkpoint | ms | 20 | 308.870 | 382.238 | 428.847 | 100k metadata-only lstat walk alongside each 10-edit trial, following capture_bench baseline; not the complete production scan/cache algorithm |
| 100000 | 697364836 | B/durable | checkpoint-10/with-manifest | ms | 20 | 77.938 | 110.838 | 153.340 | 20 reps; 5 single-byte appends + 5 same-size first-byte rewrites; file reads, canonical hashes, store commits included; no stat scan/serialization/cache-save/head CAS; real uses per-blob API, tuned single transaction, split keeps metadata in WAL FULL SQLite; with-checkpoint writes canonical-shaped sparse metadata row; with-manifest adds requested conservative full-tree metadata proxy instead; neither executes graph validation/head publication |
| 100000 | 697364836 | B/durable | checkpoint-stat-scan/with-manifest | ms | 20 | 317.116 | 434.847 | 562.724 | 100k metadata-only lstat walk alongside each 10-edit trial, following capture_bench baseline; not the complete production scan/cache algorithm |
| 100000 | 0 | A-tuned/durable | crash-random-kill | passed | 20 | 1.000 | 1.000 | 1.000 | 20/20 SIGKILL trials; all final objects fully hash-verified; SQLite integrity_check=ok each trial; seeded 100 baseline blobs; stream includes one 2MiB random payload plus 100 fixture blobs per batch; ready handshake then deterministic pseudorandom delays ms=[27, 197, 84, 179, 35, 40, 115, 72, 102, 36, 36, 191, 183, 140, 153, 114, 153, 8, 106, 3]; process-crash only, not power-loss |
| 100000 | 0 | A-tuned/durable | crash-final-objects-verified | count | 20 | 403.000 | 1009.000 | 1211.000 | every final row/file checked after every kill, including newly committed objects |
| 100000 | 0 | A-tuned/durable | crash-tmp-swept | count | 20 | 0.000 | 0.000 | 0.000 | leftover tmp files counted then removed; zero remain after each sweep |
| 100000 | 0 | B/durable | crash-random-kill | passed | 20 | 1.000 | 1.000 | 1.000 | 20/20 SIGKILL trials; all final objects fully hash-verified; file-object namespace checked each trial; no SQLite writer in this case; seeded 100 baseline blobs; stream includes one 2MiB random payload plus 100 fixture blobs per batch; ready handshake then deterministic pseudorandom delays ms=[96, 172, 192, 20, 122, 23, 162, 200, 25, 188, 81, 141, 55, 20, 94, 92, 79, 182, 143, 117]; process-crash only, not power-loss |
| 100000 | 0 | B/durable | crash-final-objects-verified | count | 20 | 456.000 | 833.000 | 854.000 | every final row/file checked after every kill, including newly committed objects |
| 100000 | 0 | B/durable | crash-tmp-swept | count | 20 | 0.000 | 1.000 | 1.000 | leftover tmp files counted then removed; zero remain after each sweep |
