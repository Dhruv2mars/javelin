# Storage benchmark

```text
APFS (30 GiB image; sqlite-disk supplement; native ingest)
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
Args: ["tools/storage-bench/target/release/storage-bench", "--dir", "/Volumes/jbench-storage/sqlite-disk-run", "--ingest-dir", "/tmp/jbench-sqlite-disk-ingest", "--reps", "5", "--page-size", "4096", "--label", "APFS (30 GiB image; sqlite-disk supplement; native ingest)", "--drop-caches-cmd", "/tmp/storage-cache-reset.sh", "--json", "tools/storage-bench/results/apfs-2026-10-18-sqlite-disk.json", "--files", "1000,10000,100000", "--stores", "A-tuned/durable,A-tuned/none", "--disk-only"]
INGEST LOCATION (all stores): /tmp/jbench-sqlite-disk-ingest
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

   Volume Used Space:         412.1 GB (412095668224 Bytes) (exactly 804874352 512-Byte-Units)
   Container Total Space:     494.4 GB (494384795648 Bytes) (exactly 965595304 512-Byte-Units)
   Container Free Space:      58.1 GB (58066612224 Bytes) (exactly 113411352 512-Byte-Units)
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
/dev/disk3s1   460Gi   384Gi    54Gi    88%     16M  567M    3%   /System/Volumes/Data
cache hook: none: cold unmeasured
Args: ["tools/storage-bench/target/release/storage-bench", "--dir", "/Volumes/jbench-storage/sqlite-disk-run", "--ingest-dir", "/tmp/jbench-sqlite-disk-ingest", "--reps", "5", "--page-size", "4096", "--label", "APFS (30 GiB image; sqlite-disk supplement; native ingest)", "--drop-caches-cmd", "/tmp/storage-cache-reset.sh", "--json", "tools/storage-bench/results/apfs-2026-10-18-sqlite-disk.json", "--files", "1000,10000,100000", "--stores", "A-tuned/durable,A-tuned/none", "--disk-only"]
```

| files | corpus bytes | store | operation | unit | samples | median/p50 | p95 | p99 | note |
| ---: | ---: | --- | --- | --- | ---: | ---: | ---: | ---: | --- |
| 100 | 0 | B/durable | crash-sigkill | passed | 1 | 1.000 | 1.000 | 1.000 | 100 complete finals hash-verified; partial tmp detected and swept; deterministic pre-rename boundary, not power-loss proof |
| 1000 | 7001918 | A-tuned-pilot | ingest/page-4096 | ms | 5 | 47.570 | 49.234 | 49.234 | durable FULL; 5 fresh stores; choose fastest median |
| 1000 | 7001918 | A-tuned-pilot | ingest/page-16384 | ms | 5 | 41.037 | 66.143 | 66.143 | durable FULL; 5 fresh stores; choose fastest median |
| 1000 | 7001918 | A-tuned-pilot | read/select | ms | 5 | 4.478 | 4.939 | 4.939 | warm 1000 reads; cached statement; includes lookup and Vec allocation |
| 1000 | 7001918 | A-tuned-pilot | read/incremental | ms | 5 | 6.573 | 7.805 | 7.805 | warm 1000 reads; cached statement; includes lookup and Vec allocation |
| 0 | 0 | configuration | chosen-page-size | bytes | 1 | 4096.000 | 4096.000 | 4096.000 | 64MiB cache; 2GiB mmap; batch 1000; incremental-read=false |
| 1000 | 7001918 | corpus | sha256 | digest | 0 | — | — | — | e8611b2463fdf89ea169490657c0438fab2b64f2be08f8226e9b44cc5ca80095 |
| 1000 | 7001918 | corpus | rounded-4k-payload | bytes | 1 | 9375744.000 | 9375744.000 | 9375744.000 | deterministic 4KiB payload-block bound; excludes inode/directory metadata; filesystem inline data may differ |
| 1000 | 7001918 | A-tuned/durable | disk/store | corpus-x | 5 | 1.117 | 1.117 | 1.117 | fresh independent allocations from closed seed; sync+settle; APFS directory clone / Linux per-file reflink Layers |
| 1000 | 7001918 | A-tuned/durable | disk/store+base | corpus-x | 5 | 2.529 | 2.529 | 2.529 | fresh independent allocations from closed seed; sync+settle; APFS directory clone / Linux per-file reflink Layers |
| 1000 | 7001918 | A-tuned/durable | disk/store+base+1layers | corpus-x | 5 | 2.579 | 2.579 | 2.579 | fresh independent allocations from closed seed; sync+settle; APFS directory clone / Linux per-file reflink Layers |
| 1000 | 7001918 | A-tuned/durable | disk/store+base+5layers | corpus-x | 5 | 2.777 | 2.777 | 2.777 | fresh independent allocations from closed seed; sync+settle; APFS directory clone / Linux per-file reflink Layers |
| 1000 | 7001918 | A-tuned/durable | disk/store-apparent | bytes | 5 | 7819264.000 | 7819264.000 | 7819264.000 | sum st_size; includes db + wal; logical, not exclusive blocks |
| 1000 | 7001918 | A-tuned/durable | disk/sqlite-db-wal | bytes | 5 | 7819264.000 | 7819264.000 | 7819264.000 | closed checkpointed DB plus any WAL/SHM |
| 1000 | 7001918 | A-tuned/durable | disk/apparent-base+1layers | bytes | 5 | 14003836.000 | 14003836.000 | 14003836.000 | apparent sizes count clone contents repeatedly |
| 1000 | 7001918 | A-tuned/durable | disk/apparent-base+5layers | bytes | 5 | 42011508.000 | 42011508.000 | 42011508.000 | apparent sizes count clone contents repeatedly |
| 1000 | 7001918 | A-tuned/durable | context/base-to-layer | ms | 5 | 8.694 | 10.878 | 10.878 | not comparison target; APFS directory clone / Linux per-file reflink |
| 1000 | 7001918 | A-tuned/none | disk/store | corpus-x | 5 | 1.117 | 1.117 | 1.117 | fresh independent allocations from closed seed; sync+settle; APFS directory clone / Linux per-file reflink Layers |
| 1000 | 7001918 | A-tuned/none | disk/store+base | corpus-x | 5 | 2.529 | 2.529 | 2.529 | fresh independent allocations from closed seed; sync+settle; APFS directory clone / Linux per-file reflink Layers |
| 1000 | 7001918 | A-tuned/none | disk/store+base+1layers | corpus-x | 5 | 2.579 | 2.579 | 2.579 | fresh independent allocations from closed seed; sync+settle; APFS directory clone / Linux per-file reflink Layers |
| 1000 | 7001918 | A-tuned/none | disk/store+base+5layers | corpus-x | 5 | 2.777 | 2.777 | 2.777 | fresh independent allocations from closed seed; sync+settle; APFS directory clone / Linux per-file reflink Layers |
| 1000 | 7001918 | A-tuned/none | disk/store-apparent | bytes | 5 | 7819264.000 | 7819264.000 | 7819264.000 | sum st_size; includes db + wal; logical, not exclusive blocks |
| 1000 | 7001918 | A-tuned/none | disk/sqlite-db-wal | bytes | 5 | 7819264.000 | 7819264.000 | 7819264.000 | closed checkpointed DB plus any WAL/SHM |
| 1000 | 7001918 | A-tuned/none | disk/apparent-base+1layers | bytes | 5 | 14003836.000 | 14003836.000 | 14003836.000 | apparent sizes count clone contents repeatedly |
| 1000 | 7001918 | A-tuned/none | disk/apparent-base+5layers | bytes | 5 | 42011508.000 | 42011508.000 | 42011508.000 | apparent sizes count clone contents repeatedly |
| 1000 | 7001918 | A-tuned/none | context/base-to-layer | ms | 5 | 8.422 | 8.826 | 8.826 | not comparison target; APFS directory clone / Linux per-file reflink |
| 10000 | 70521647 | corpus | sha256 | digest | 0 | — | — | — | 187be301ccb3400fc936937dda254a520d6afbfa0388a94cd1ef776e01f549f6 |
| 10000 | 70521647 | corpus | rounded-4k-payload | bytes | 1 | 93962240.000 | 93962240.000 | 93962240.000 | deterministic 4KiB payload-block bound; excludes inode/directory metadata; filesystem inline data may differ |
| 10000 | 70521647 | A-tuned/durable | disk/store | corpus-x | 5 | 1.111 | 1.111 | 1.111 | fresh independent allocations from closed seed; sync+settle; APFS directory clone / Linux per-file reflink Layers |
| 10000 | 70521647 | A-tuned/durable | disk/store+base | corpus-x | 5 | 2.519 | 2.519 | 2.519 | fresh independent allocations from closed seed; sync+settle; APFS directory clone / Linux per-file reflink Layers |
| 10000 | 70521647 | A-tuned/durable | disk/store+base+1layers | corpus-x | 5 | 2.572 | 2.573 | 2.573 | fresh independent allocations from closed seed; sync+settle; APFS directory clone / Linux per-file reflink Layers |
| 10000 | 70521647 | A-tuned/durable | disk/store+base+5layers | corpus-x | 5 | 2.783 | 2.783 | 2.783 | fresh independent allocations from closed seed; sync+settle; APFS directory clone / Linux per-file reflink Layers |
| 10000 | 70521647 | A-tuned/durable | disk/store-apparent | bytes | 5 | 78323712.000 | 78323712.000 | 78323712.000 | sum st_size; includes db + wal; logical, not exclusive blocks |
| 10000 | 70521647 | A-tuned/durable | disk/sqlite-db-wal | bytes | 5 | 78323712.000 | 78323712.000 | 78323712.000 | closed checkpointed DB plus any WAL/SHM |
| 10000 | 70521647 | A-tuned/durable | disk/apparent-base+1layers | bytes | 5 | 141043294.000 | 141043294.000 | 141043294.000 | apparent sizes count clone contents repeatedly |
| 10000 | 70521647 | A-tuned/durable | disk/apparent-base+5layers | bytes | 5 | 423129882.000 | 423129882.000 | 423129882.000 | apparent sizes count clone contents repeatedly |
| 10000 | 70521647 | A-tuned/durable | context/base-to-layer | ms | 5 | 311.703 | 421.551 | 421.551 | not comparison target; APFS directory clone / Linux per-file reflink |
| 10000 | 70521647 | A-tuned/none | disk/store | corpus-x | 5 | 1.111 | 1.111 | 1.111 | fresh independent allocations from closed seed; sync+settle; APFS directory clone / Linux per-file reflink Layers |
| 10000 | 70521647 | A-tuned/none | disk/store+base | corpus-x | 5 | 2.519 | 2.520 | 2.520 | fresh independent allocations from closed seed; sync+settle; APFS directory clone / Linux per-file reflink Layers |
| 10000 | 70521647 | A-tuned/none | disk/store+base+1layers | corpus-x | 5 | 2.572 | 2.573 | 2.573 | fresh independent allocations from closed seed; sync+settle; APFS directory clone / Linux per-file reflink Layers |
| 10000 | 70521647 | A-tuned/none | disk/store+base+5layers | corpus-x | 5 | 2.783 | 2.783 | 2.783 | fresh independent allocations from closed seed; sync+settle; APFS directory clone / Linux per-file reflink Layers |
| 10000 | 70521647 | A-tuned/none | disk/store-apparent | bytes | 5 | 78323712.000 | 78323712.000 | 78323712.000 | sum st_size; includes db + wal; logical, not exclusive blocks |
| 10000 | 70521647 | A-tuned/none | disk/sqlite-db-wal | bytes | 5 | 78323712.000 | 78323712.000 | 78323712.000 | closed checkpointed DB plus any WAL/SHM |
| 10000 | 70521647 | A-tuned/none | disk/apparent-base+1layers | bytes | 5 | 141043294.000 | 141043294.000 | 141043294.000 | apparent sizes count clone contents repeatedly |
| 10000 | 70521647 | A-tuned/none | disk/apparent-base+5layers | bytes | 5 | 423129882.000 | 423129882.000 | 423129882.000 | apparent sizes count clone contents repeatedly |
| 10000 | 70521647 | A-tuned/none | context/base-to-layer | ms | 5 | 401.548 | 540.170 | 540.170 | not comparison target; APFS directory clone / Linux per-file reflink |
| 100000 | 697364836 | corpus | sha256 | digest | 0 | — | — | — | 25ab49fa2369c3a3fbf53bf5da1d15e8fe9431c8177f389e0734dd5761ea5010 |
| 100000 | 697364836 | corpus | rounded-4k-payload | bytes | 1 | 932982784.000 | 932982784.000 | 932982784.000 | deterministic 4KiB payload-block bound; excludes inode/directory metadata; filesystem inline data may differ |
| 100000 | 697364836 | A-tuned/durable | disk/store | corpus-x | 5 | 1.128 | 1.131 | 1.131 | fresh independent allocations from closed seed; sync+settle; APFS directory clone / Linux per-file reflink Layers |
| 100000 | 697364836 | A-tuned/durable | disk/store+base | corpus-x | 5 | 2.543 | 2.546 | 2.546 | fresh independent allocations from closed seed; sync+settle; APFS directory clone / Linux per-file reflink Layers |
| 100000 | 697364836 | A-tuned/durable | disk/store+base+1layers | corpus-x | 5 | 2.597 | 2.600 | 2.600 | fresh independent allocations from closed seed; sync+settle; APFS directory clone / Linux per-file reflink Layers |
| 100000 | 697364836 | A-tuned/durable | disk/store+base+5layers | corpus-x | 5 | 2.814 | 2.817 | 2.817 | fresh independent allocations from closed seed; sync+settle; APFS directory clone / Linux per-file reflink Layers |
| 100000 | 697364836 | A-tuned/durable | disk/store-apparent | bytes | 5 | 775364608.000 | 775364608.000 | 775364608.000 | sum st_size; includes db + wal; logical, not exclusive blocks |
| 100000 | 697364836 | A-tuned/durable | disk/sqlite-db-wal | bytes | 5 | 775364608.000 | 775364608.000 | 775364608.000 | closed checkpointed DB plus any WAL/SHM |
| 100000 | 697364836 | A-tuned/durable | disk/apparent-base+1layers | bytes | 5 | 1394729672.000 | 1394729672.000 | 1394729672.000 | apparent sizes count clone contents repeatedly |
| 100000 | 697364836 | A-tuned/durable | disk/apparent-base+5layers | bytes | 5 | 4184189016.000 | 4184189016.000 | 4184189016.000 | apparent sizes count clone contents repeatedly |
| 100000 | 697364836 | A-tuned/durable | context/base-to-layer | ms | 5 | 6799.566 | 9726.526 | 9726.526 | not comparison target; APFS directory clone / Linux per-file reflink |
| 100000 | 697364836 | A-tuned/none | disk/store | corpus-x | 5 | 1.125 | 1.134 | 1.134 | fresh independent allocations from closed seed; sync+settle; APFS directory clone / Linux per-file reflink Layers |
| 100000 | 697364836 | A-tuned/none | disk/store+base | corpus-x | 5 | 2.541 | 2.550 | 2.550 | fresh independent allocations from closed seed; sync+settle; APFS directory clone / Linux per-file reflink Layers |
| 100000 | 697364836 | A-tuned/none | disk/store+base+1layers | corpus-x | 5 | 2.594 | 2.603 | 2.603 | fresh independent allocations from closed seed; sync+settle; APFS directory clone / Linux per-file reflink Layers |
| 100000 | 697364836 | A-tuned/none | disk/store+base+5layers | corpus-x | 5 | 2.811 | 2.820 | 2.820 | fresh independent allocations from closed seed; sync+settle; APFS directory clone / Linux per-file reflink Layers |
| 100000 | 697364836 | A-tuned/none | disk/store-apparent | bytes | 5 | 775364608.000 | 775364608.000 | 775364608.000 | sum st_size; includes db + wal; logical, not exclusive blocks |
| 100000 | 697364836 | A-tuned/none | disk/sqlite-db-wal | bytes | 5 | 775364608.000 | 775364608.000 | 775364608.000 | closed checkpointed DB plus any WAL/SHM |
| 100000 | 697364836 | A-tuned/none | disk/apparent-base+1layers | bytes | 5 | 1394729672.000 | 1394729672.000 | 1394729672.000 | apparent sizes count clone contents repeatedly |
| 100000 | 697364836 | A-tuned/none | disk/apparent-base+5layers | bytes | 5 | 4184189016.000 | 4184189016.000 | 4184189016.000 | apparent sizes count clone contents repeatedly |
| 100000 | 697364836 | A-tuned/none | context/base-to-layer | ms | 5 | 9166.213 | 11083.517 | 11083.517 | not comparison target; APFS directory clone / Linux per-file reflink |
