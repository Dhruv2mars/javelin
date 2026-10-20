# Robustness

This page states what the local core guarantees under concurrent processes, crashes,
retries and damaged storage, and which tests prove each guarantee. The architecture is in
`docs/architecture.md`; the command surface is in `docs/cli.md`.

## Guarantees

**Concurrent Publish.** Processes racing to advance one World get exactly one winner per
version. Each loser gets an explicit `conflict` receipt (exit 3) when its Layer overlaps the winner. Versions stay contiguous, with no lost or duplicated
version. A process that cannot get the write lock within the 5 second busy timeout gets
`busy`, and nothing changed. Racing processes that do not conflict all land, one version each.

**Crash recovery.** A process stopped at any named point during Layer create, capture,
discard, Publish, or workspace open and discard leaves a repository that passes `check()`.
Each operation is either fully recorded or not recorded at all. A stopped workspace open leaves
no directory behind. A stopped workspace discard is finished by recovery.

**Idempotent retries.** A retry with the same operation ID and the same input after a lost
reply returns the recorded receipt and applies nothing twice. The same ID with different input
is refused as `operation_id_reused`.

**Corruption detection.** Damage is detected, never read as different content:

- `check()` hashes every stored object and refuses any flipped byte in a blob, manifest or
  tree, as `corrupt_object`. This holds for unreferenced objects too.
- Reads go through reachable objects only. A read returns an error with a code, or exactly the
  original content.
- An altered `versions` or `changes` row is refused as `corrupt_repository`.
- A damaged SQLite file (header, schema, stored text) is refused as `corrupt_repository`,
  or fails to open, and is never reported as a retryable `storage_error`.

## Failure codes

| Situation | Code | Retry? |
| --- | --- | --- |
| Write lock not acquired within the busy timeout | `busy` | Yes; nothing changed |
| Other SQLite failure; a Publish may have been attempted | `storage_error` | Yes, with the same operation ID and input |
| Object bytes do not match their ID | `corrupt_object` | No; repair or restore |
| Repository rows or SQLite file damaged | `corrupt_repository` | No; restore |

## How each guarantee is proven

| Guarantee | Test | What it runs |
| --- | --- | --- |
| Racing Publish, one winner | `racing_publishers_of_one_path_have_one_winner_and_explicit_conflicts` (`crates/javelin-cli/tests/robustness.rs`) | Six `javelin` processes publish Layers that touch one path |
| Racing Publish, all land | `racing_publishers_of_disjoint_paths_all_land_once_in_order` | Six processes publish disjoint paths; versions 1 to 6 each appear once |
| Racing retries | `racing_retries_of_one_operation_publish_once_with_identical_receipts` | Six processes share one operation ID; one version, identical receipts |
| Lock held past timeout | `lock_held_past_the_timeout_is_busy_and_retry_publishes_once` | An exclusive lock held for the timeout; `busy`, then a retry publishes once |
| Layer create crash | `layer_create_crash_points_leave_no_partial_layer_and_retries_are_idempotent` | Failpoints around the commit |
| Capture crash | `capture_crash_points_never_leave_a_half_recorded_checkpoint` | Failpoints around the commit; the revision count is checked through the library |
| Layer discard crash | `layer_discard_crash_points_are_atomic_and_retries_end_discarded_once` | Failpoints around the commit |
| Publish crash | `publish_crash_before_commit_publishes_once_on_retry` | Failpoint before commit; retry publishes once |
| Lost reply | `publish_lost_reply_retry_returns_the_same_version_and_never_applies_twice` | Failpoint after commit; the retry returns the same receipt |
| Workspace open crash | `workspace_open_crash_before_rename_leaves_no_directory_and_retry_opens` | Failpoint before the rename |
| Workspace discard crash | `workspace_discard_crash_before_trash_is_finished_by_recovery_and_retry_is_idempotent` | Failpoint before the trash move; recovery finishes it |
| Racing and retry in-process | `process_tests` in `src/repository.rs` | The library harness: concurrent initializers, publishers, retries and kill points |
| Blob, manifest and tree byte flips | `every_flipped_*_byte_is_detected_as_corrupt_object` (`tests/corruption.rs`) | Every byte of every stored object of each kind |
| Substituted blob | `a_blob_whose_bytes_are_replaced_by_another_valid_blob_is_detected` | A valid but wrong blob for an ID |
| Altered rows | `an_altered_version_row_is_detected` | Version id and tree, changeset content and path |
| Whole-file SQLite flips | `every_sampled_sqlite_file_byte_flip_fails_or_returns_original_content` | Header bytes and a sampled stride through the file |

Run them with:

```sh
cargo test --locked -p javelin-cli --test robustness
cargo test --locked --test corruption
cargo test --locked --lib process_tests
```

## Failpoints

`JAVELIN_FAILPOINT=<name>` makes the process exit with status 86 at a named point, without
unwinding, dropping connections or finishing the operation. The check is compiled only in debug
builds (`#[cfg(debug_assertions)]` in `src/repository.rs`); release builds contain no hook.
Exit status 86 proves the point was reached.

The named points are `layer_create_{before,after}_commit`, `layer_capture_{before,after}_commit`,
`layer_discard_{before,after}_commit`, `publish_{before,after}_commit`,
`workspace_open_before_rename` and `workspace_discard_before_trash`.

## Limits

- The whole-file SQLite sweep is sampled: the first 100 bytes, then a stride of about 1/600 of
  the file. It is not exhaustive. Exhaustive flips of every byte would be too slow for the
  debug test run.
- Crash points are process exits, not power loss. They do not simulate lost or reordered
  writes below the OS, so the durability of `fullfsync` is not tested here.
- Unreferenced objects are detected by `check()` only. Reads never reach them.
- The failpoints exist only in debug builds, so the crash tests run only against debug binaries.
- CI runs these tests on macOS, Linux and Windows. Local runs covered macOS only.
