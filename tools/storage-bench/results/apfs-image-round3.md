# Storage benchmark

```text
APFS isolated 30GiB sparse image; round3 sustained plateau and remount control
ProductName:		macOS
ProductVersion:		26.6.2
BuildVersion:		25G83
CPU: Apple M1
RAM bytes: 17179869184
Device Identifier:         disk7s1
   Device Node:               /dev/disk7s1
   Whole:                     No
   Part of Whole:             disk7

   Volume Name:               jbench-r3
   Mounted:                   Yes
   Mount Point:               /Volumes/jbench-r3

   Partition Type:            41504653-0000-11AA-AA11-00306543ECAC
   File System Personality:   APFS
   Type (Bundle):             apfs
   Name (User Visible):       APFS
   Owners:                    Disabled

   OS Can Be Installed:       Yes
   Media Type:                Generic
   Protocol:                  Disk Image
   SMART Status:              Not Supported
   Volume UUID:               D2009693-46C0-4E2B-9D49-6599269BF42A
   Disk / Partition UUID:     D2009693-46C0-4E2B-9D49-6599269BF42A

   Disk Size:                 32.0 GB (32002498560 Bytes) (exactly 62504880 512-Byte-Units)
   Device Block Size:         4096 Bytes

   Volume Used Space:         24.6 KB (24576 Bytes) (exactly 48 512-Byte-Units)
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
/dev/disk7s1    30Gi    24Ki    30Gi     1%       1  312M    0%   /Volumes/jbench-r3
cache hook: none: cold unmeasured
Args: ["/Users/dhruv2mars/.t3/worktrees/javelin/bench-storage-split/tools/storage-bench/target/release/storage-bench", "--dir", "/Volumes/jbench-r3/disk", "--files", "100000", "--reps", "5", "--disk-only", "--stores", "A-tuned/durable,B/durable", "--page-size", "4096", "--remount-cmd", "/tmp/jbench-r3-remount.sh", "--label", "APFS isolated 30GiB sparse image; round3 sustained plateau and remount control", "--json", "/Users/dhruv2mars/.local/share/javelin-storage-bench/round3/apfs-image-round3.json"]
```

| files | corpus bytes | store | operation | unit | samples | median/p50 | p95 | p99 | note |
| ---: | ---: | --- | --- | --- | ---: | ---: | ---: | ---: | --- |
| 100 | 0 | B/durable | crash-sigkill | passed | 1 | 1.000 | 1.000 | 1.000 | 100 complete finals hash-verified; partial tmp detected and swept; deterministic pre-rename boundary, not power-loss proof |
| 1000 | 7001918 | A-tuned-pilot | ingest/page-4096 | ms | 5 | 67.740 | 89.537 | 89.537 | durable FULL; 5 fresh stores; choose fastest median |
| 1000 | 7001918 | A-tuned-pilot | ingest/page-16384 | ms | 5 | 39.794 | 42.621 | 42.621 | durable FULL; 5 fresh stores; choose fastest median |
| 1000 | 7001918 | A-tuned-pilot | read/select | ms | 5 | 8.701 | 36.966 | 36.966 | warm 1000 reads; cached statement; includes lookup and Vec allocation |
| 1000 | 7001918 | A-tuned-pilot | read/incremental | ms | 5 | 11.152 | 12.494 | 12.494 | warm 1000 reads; cached statement; includes lookup and Vec allocation |
| 0 | 0 | configuration | chosen-page-size | bytes | 1 | 4096.000 | 4096.000 | 4096.000 | 64MiB cache; 2GiB mmap; batch 1000; incremental-read=false |
| 100000 | 697364836 | corpus | sha256 | digest | 0 | — | — | — | 25ab49fa2369c3a3fbf53bf5da1d15e8fe9431c8177f389e0734dd5761ea5010 |
| 100000 | 697364836 | corpus | rounded-4k-payload | bytes | 1 | 932982784.000 | 932982784.000 | 932982784.000 | deterministic 4KiB payload-block bound; excludes inode/directory metadata; filesystem inline data may differ |
| 100000 | 697364836 | control | disk/standalone-tree | bytes | 5 | 987992064.000 | 988176384.000 | 988176384.000 | fresh explicit-write tree, no store, sync+2s plateau; btrfs physical used including metadata DUP; allocated-tree denominator context only |
| 100000 | 697364836 | control | disk/deletion-residual | bytes | 5 | 0.000 | 4096.000 | 4096.000 | allocation remaining after deleting the standalone control; negative is filesystem metadata reclamation/noise; raw statfs retained alongside btrfs usage |
| 100000 | 697364836 | A-tuned/durable | disk/remount-store+base | corpus-x | 1 | 2.550 | 2.550 | 2.550 | one remount control at 100k, same live store+base; compare disk/store+base first sample |
| 100000 | 697364836 | A-tuned/durable | disk/remount-drift | bytes | 1 | 0.000 | 0.000 | 0.000 | positive means more allocation reported after remount |
| 100000 | 697364836 | A-tuned/durable | disk/store | corpus-x | 5 | 1.122 | 1.134 | 1.134 | fresh independent allocations from closed seed; sync+2s plateau; btrfs physical used including metadata DUP, others statfs; APFS directory clone / Linux per-file reflink Layers |
| 100000 | 697364836 | A-tuned/durable | disk/store+base | corpus-x | 5 | 2.537 | 2.550 | 2.550 | fresh independent allocations from closed seed; sync+2s plateau; btrfs physical used including metadata DUP, others statfs; APFS directory clone / Linux per-file reflink Layers |
| 100000 | 697364836 | A-tuned/durable | disk/store+base+1layers | corpus-x | 5 | 2.591 | 2.603 | 2.603 | fresh independent allocations from closed seed; sync+2s plateau; btrfs physical used including metadata DUP, others statfs; APFS directory clone / Linux per-file reflink Layers |
| 100000 | 697364836 | A-tuned/durable | disk/store+base+5layers | corpus-x | 5 | 2.808 | 2.820 | 2.820 | fresh independent allocations from closed seed; sync+2s plateau; btrfs physical used including metadata DUP, others statfs; APFS directory clone / Linux per-file reflink Layers |
| 100000 | 697364836 | A-tuned/durable | disk/store-apparent | bytes | 5 | 775364608.000 | 775364608.000 | 775364608.000 | sum st_size; includes db + wal; logical, not exclusive blocks |
| 100000 | 697364836 | A-tuned/durable | disk/sqlite-db-wal | bytes | 5 | 775364608.000 | 775364608.000 | 775364608.000 | closed checkpointed DB plus any WAL/SHM |
| 100000 | 697364836 | A-tuned/durable | disk/apparent-base+1layers | bytes | 5 | 1394729672.000 | 1394729672.000 | 1394729672.000 | apparent sizes count clone contents repeatedly |
| 100000 | 697364836 | A-tuned/durable | disk/apparent-base+5layers | bytes | 5 | 4184189016.000 | 4184189016.000 | 4184189016.000 | apparent sizes count clone contents repeatedly |
| 100000 | 697364836 | A-tuned/durable | context/base-to-layer | ms | 5 | 7822.818 | 23247.127 | 23247.127 | not comparison target; APFS directory clone / Linux per-file reflink |
| 100000 | 697364836 | B/durable | disk/remount-store+base | corpus-x | 1 | 1.477 | 1.477 | 1.477 | one remount control at 100k, same live store+base; compare disk/store+base first sample |
| 100000 | 697364836 | B/durable | disk/remount-drift | bytes | 1 | 0.000 | 0.000 | 0.000 | positive means more allocation reported after remount |
| 100000 | 697364836 | B/durable | disk/store | corpus-x | 5 | 1.425 | 1.425 | 1.425 | fresh independent allocations from closed seed; sync+2s plateau; btrfs physical used including metadata DUP, others statfs; APFS directory clone / Linux per-file reflink Layers |
| 100000 | 697364836 | B/durable | disk/store+base | corpus-x | 5 | 1.476 | 1.477 | 1.477 | fresh independent allocations from closed seed; sync+2s plateau; btrfs physical used including metadata DUP, others statfs; APFS directory clone / Linux per-file reflink Layers |
| 100000 | 697364836 | B/durable | disk/store+base+1layers | corpus-x | 5 | 1.530 | 1.531 | 1.531 | fresh independent allocations from closed seed; sync+2s plateau; btrfs physical used including metadata DUP, others statfs; APFS directory clone / Linux per-file reflink Layers |
| 100000 | 697364836 | B/durable | disk/store+base+5layers | corpus-x | 5 | 1.745 | 1.745 | 1.745 | fresh independent allocations from closed seed; sync+2s plateau; btrfs physical used including metadata DUP, others statfs; APFS directory clone / Linux per-file reflink Layers |
| 100000 | 697364836 | B/durable | disk/store-apparent | bytes | 5 | 697364836.000 | 697364836.000 | 697364836.000 | sum st_size; includes db + wal; logical, not exclusive blocks |
| 100000 | 697364836 | B/durable | disk/apparent-base+1layers | bytes | 5 | 1394729672.000 | 1394729672.000 | 1394729672.000 | apparent sizes count clone contents repeatedly |
| 100000 | 697364836 | B/durable | disk/apparent-base+5layers | bytes | 5 | 4184189016.000 | 4184189016.000 | 4184189016.000 | apparent sizes count clone contents repeatedly |
| 100000 | 697364836 | B/durable | context/base-to-layer | ms | 5 | 7751.833 | 8009.644 | 8009.644 | not comparison target; APFS directory clone / Linux per-file reflink |
