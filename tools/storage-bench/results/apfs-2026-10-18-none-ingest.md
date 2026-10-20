# Storage benchmark

```text
APFS (native internal SSD; corrected NONE schema initialization)
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

   Volume Used Space:         413.6 GB (413565100032 Bytes) (exactly 807744336 512-Byte-Units)
   Container Total Space:     494.4 GB (494384795648 Bytes) (exactly 965595304 512-Byte-Units)
   Container Free Space:      56.6 GB (56597254144 Bytes) (exactly 110541512 512-Byte-Units)
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
/dev/disk3s1   460Gi   385Gi    53Gi    88%     17M  553M    3%   /System/Volumes/Data
cache hook: none: cold unmeasured
Args: ["tools/storage-bench/target/release/storage-bench", "--dir", "/tmp/jbench-none-corrected", "--files", "1000,10000,100000", "--reps", "5", "--page-size", "4096", "--stores", "A-tuned/none", "--ingest-only", "--label", "APFS (native internal SSD; corrected NONE schema initialization)", "--json", "tools/storage-bench/results/apfs-2026-10-18-none-ingest.json"]
```

| files | corpus bytes | store | operation | unit | samples | median/p50 | p95 | p99 | note |
| ---: | ---: | --- | --- | --- | ---: | ---: | ---: | ---: | --- |
| 100 | 0 | B/durable | crash-sigkill | passed | 1 | 1.000 | 1.000 | 1.000 | 100 complete finals hash-verified; partial tmp detected and swept; deterministic pre-rename boundary, not power-loss proof |
| 1000 | 7001918 | A-tuned-pilot | ingest/page-4096 | ms | 5 | 211.128 | 246.399 | 246.399 | durable FULL; 5 fresh stores; choose fastest median |
| 1000 | 7001918 | A-tuned-pilot | ingest/page-16384 | ms | 5 | 110.533 | 113.656 | 113.656 | durable FULL; 5 fresh stores; choose fastest median |
| 1000 | 7001918 | A-tuned-pilot | read/select | ms | 5 | 7.852 | 13.750 | 13.750 | warm 1000 reads; cached statement; includes lookup and Vec allocation |
| 1000 | 7001918 | A-tuned-pilot | read/incremental | ms | 5 | 6.797 | 22.415 | 22.415 | warm 1000 reads; cached statement; includes lookup and Vec allocation |
| 0 | 0 | configuration | chosen-page-size | bytes | 1 | 4096.000 | 4096.000 | 4096.000 | 64MiB cache; 2GiB mmap; batch 1000; incremental-read=true |
| 1000 | 7001918 | corpus | sha256 | digest | 0 | — | — | — | e8611b2463fdf89ea169490657c0438fab2b64f2be08f8226e9b44cc5ca80095 |
| 1000 | 7001918 | corpus | rounded-4k-payload | bytes | 1 | 9375744.000 | 9375744.000 | 9375744.000 | deterministic 4KiB payload-block bound; excludes inode/directory metadata; filesystem inline data may differ |
| 1000 | 7001918 | A-tuned/none | ingest | ms | 5 | 151.368 | 189.796 | 189.796 | RAM corpus; includes hashing, fresh store setup and final WAL checkpoint; A-real 100k single run |
| 1000 | 7001918 | A-tuned/none | ingest-throughput | MB/s | 5 | 46.258 | 53.421 | 53.421 | decimal MB |
| 1000 | 7001918 | A-tuned/none | ingest-files | files/s | 5 | 6606.429 | 7629.487 | 7629.487 |  |
| 10000 | 70521647 | corpus | sha256 | digest | 0 | — | — | — | 187be301ccb3400fc936937dda254a520d6afbfa0388a94cd1ef776e01f549f6 |
| 10000 | 70521647 | corpus | rounded-4k-payload | bytes | 1 | 93962240.000 | 93962240.000 | 93962240.000 | deterministic 4KiB payload-block bound; excludes inode/directory metadata; filesystem inline data may differ |
| 10000 | 70521647 | A-tuned/none | ingest | ms | 5 | 973.849 | 1061.294 | 1061.294 | RAM corpus; includes hashing, fresh store setup and final WAL checkpoint; A-real 100k single run |
| 10000 | 70521647 | A-tuned/none | ingest-throughput | MB/s | 5 | 72.415 | 75.712 | 75.712 | decimal MB |
| 10000 | 70521647 | A-tuned/none | ingest-files | files/s | 5 | 10268.533 | 10735.975 | 10735.975 |  |
| 100000 | 697364836 | corpus | sha256 | digest | 0 | — | — | — | 25ab49fa2369c3a3fbf53bf5da1d15e8fe9431c8177f389e0734dd5761ea5010 |
| 100000 | 697364836 | corpus | rounded-4k-payload | bytes | 1 | 932982784.000 | 932982784.000 | 932982784.000 | deterministic 4KiB payload-block bound; excludes inode/directory metadata; filesystem inline data may differ |
| 100000 | 697364836 | A-tuned/none | ingest | ms | 5 | 9738.563 | 11249.377 | 11249.377 | RAM corpus; includes hashing, fresh store setup and final WAL checkpoint; A-real 100k single run |
| 100000 | 697364836 | A-tuned/none | ingest-throughput | MB/s | 5 | 71.609 | 75.839 | 75.839 | decimal MB |
| 100000 | 697364836 | A-tuned/none | ingest-files | files/s | 5 | 10268.456 | 10875.146 | 10875.146 |  |
