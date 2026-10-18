# Storage benchmark

```text
APFS native internal SSD; round3 rotated 100k decision cohort; no cache reset available
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

   Volume Used Space:         419.6 GB (419611664384 Bytes) (exactly 819554032 512-Byte-Units)
   Container Total Space:     494.4 GB (494384795648 Bytes) (exactly 965595304 512-Byte-Units)
   Container Free Space:      44.1 GB (44107563008 Bytes) (exactly 86147584 512-Byte-Units)
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
/dev/disk3s1   460Gi   391Gi    41Gi    91%     17M  431M    4%   /System/Volumes/Data
cache hook: none: cold unmeasured
Args: ["tools/storage-bench/target/release/storage-bench", "--dir", "/tmp/jbench-r3-native-100k", "--files", "100000", "--reps", "5", "--decision-only", "--label", "APFS native internal SSD; round3 rotated 100k decision cohort; no cache reset available", "--json", "/Users/dhruv2mars/.local/share/javelin-storage-bench/round3/apfs-native-round3.json"]
```

| files | corpus bytes | store | operation | unit | samples | median/p50 | p95 | p99 | note |
| ---: | ---: | --- | --- | --- | ---: | ---: | ---: | ---: | --- |
| 100000 | 697364836 | decision-order | ingest/rep-1 | order | 0 | — | — | — | A-tuned/durable -> A-tuned/none -> B/durable -> B/none |
| 100000 | 697364836 | decision-order | ingest/rep-2 | order | 0 | — | — | — | A-tuned/none -> B/durable -> B/none -> A-tuned/durable |
| 100000 | 697364836 | decision-order | ingest/rep-3 | order | 0 | — | — | — | B/durable -> B/none -> A-tuned/durable -> A-tuned/none |
| 100000 | 697364836 | decision-order | ingest/rep-4 | order | 0 | — | — | — | B/none -> A-tuned/durable -> A-tuned/none -> B/durable |
| 100000 | 697364836 | decision-order | ingest/rep-5 | order | 0 | — | — | — | A-tuned/durable -> A-tuned/none -> B/durable -> B/none |
| 100000 | 697364836 | A-tuned/durable | decision/ingest | ms | 5 | 7767.553 | 9604.453 | 9604.453 | rotated matched cohort; fixed 4KiB/SELECT; fresh store setup, hashes and WAL checkpoint included; ordinary fsync matched durability |
| 100000 | 697364836 | A-tuned/none | decision/ingest | ms | 5 | 5346.418 | 6476.401 | 6476.401 | rotated matched cohort; fixed 4KiB/SELECT; fresh store setup, hashes and WAL checkpoint included; ordinary fsync matched durability |
| 100000 | 697364836 | B/durable | decision/ingest | ms | 5 | 32232.619 | 33827.449 | 33827.449 | rotated matched cohort; fixed 4KiB/SELECT; fresh store setup, hashes and WAL checkpoint included; ordinary fsync matched durability |
| 100000 | 697364836 | B/none | decision/ingest | ms | 5 | 33449.079 | 37874.742 | 37874.742 | rotated matched cohort; fixed 4KiB/SELECT; fresh store setup, hashes and WAL checkpoint included; ordinary fsync matched durability |
| 100000 | 697364836 | decision-order | materialize/rep-1 | order | 0 | — | — | — | A-tuned/durable/read-write/warm -> B/durable/B-clone/warm -> B/durable/B-copy/warm |
| 100000 | 697364836 | decision-order | materialize/rep-2 | order | 0 | — | — | — | B/durable/B-clone/warm -> B/durable/B-copy/warm -> A-tuned/durable/read-write/warm |
| 100000 | 697364836 | decision-order | materialize/rep-3 | order | 0 | — | — | — | B/durable/B-copy/warm -> A-tuned/durable/read-write/warm -> B/durable/B-clone/warm |
| 100000 | 697364836 | decision-order | materialize/rep-4 | order | 0 | — | — | — | A-tuned/durable/read-write/warm -> B/durable/B-clone/warm -> B/durable/B-copy/warm |
| 100000 | 697364836 | decision-order | materialize/rep-5 | order | 0 | — | — | — | B/durable/B-clone/warm -> B/durable/B-copy/warm -> A-tuned/durable/read-write/warm |
| 100000 | 697364836 | A-tuned/durable | decision/materialize/read-write/warm | ms | 5 | 18405.713 | 20348.003 | 20348.003 | rotated matched cohort; common sync+2s boundary before each case (isolated runs require a plateau; native statfs deltas diagnostic); warm reads entire source; dirs/chmod included; sync/verification excluded |
| 100000 | 697364836 | A-tuned/durable | decision/base-physical/read-write/warm | bytes | 5 | 994189312.000 | 1007972352.000 | 1007972352.000 | rotated matched cohort; common sync+2s boundary before each case (isolated runs require a plateau; native statfs deltas diagnostic); warm reads entire source; dirs/chmod included; sync/verification excluded |
| 100000 | 697364836 | B/durable | decision/materialize/B-clone/warm | ms | 5 | 21551.469 | 23880.127 | 23880.127 | rotated matched cohort; common sync+2s boundary before each case (isolated runs require a plateau; native statfs deltas diagnostic); warm reads entire source; dirs/chmod included; sync/verification excluded |
| 100000 | 697364836 | B/durable | decision/base-physical/B-clone/warm | bytes | 5 | 47333376.000 | 65925120.000 | 65925120.000 | rotated matched cohort; common sync+2s boundary before each case (isolated runs require a plateau; native statfs deltas diagnostic); warm reads entire source; dirs/chmod included; sync/verification excluded |
| 100000 | 697364836 | B/durable | decision/materialize/B-copy/warm | ms | 5 | 25486.402 | 30007.965 | 30007.965 | rotated matched cohort; common sync+2s boundary before each case (isolated runs require a plateau; native statfs deltas diagnostic); warm reads entire source; dirs/chmod included; sync/verification excluded |
| 100000 | 697364836 | B/durable | decision/base-physical/B-copy/warm | bytes | 5 | 994107392.000 | 1043410944.000 | 1043410944.000 | rotated matched cohort; common sync+2s boundary before each case (isolated runs require a plateau; native statfs deltas diagnostic); warm reads entire source; dirs/chmod included; sync/verification excluded |
| 100000 | 697364836 | A-tuned/durable | decision/materialize/read-write/cold-start | ms | 0 | — | — | — | unmeasured: no native cache-reset hook |
| 100000 | 697364836 | A-tuned/durable | decision/base-physical/read-write/cold-start | bytes | 0 | — | — | — | unmeasured: no native cache-reset hook |
| 100000 | 697364836 | B/durable | decision/materialize/B-clone/cold-start | ms | 0 | — | — | — | unmeasured: no native cache-reset hook |
| 100000 | 697364836 | B/durable | decision/base-physical/B-clone/cold-start | bytes | 0 | — | — | — | unmeasured: no native cache-reset hook |
| 100000 | 697364836 | B/durable | decision/materialize/B-copy/cold-start | ms | 0 | — | — | — | unmeasured: no native cache-reset hook |
| 100000 | 697364836 | B/durable | decision/base-physical/B-copy/cold-start | bytes | 0 | — | — | — | unmeasured: no native cache-reset hook |
