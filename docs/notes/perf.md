# Performance (Task 28)

Measured on the development machine (Apple Silicon laptop, APFS SSD), release build, server
on localhost — so network time is near zero and the numbers show spoko's own cost.

Reproduce: `cargo test --release -p spoko --test perf -- --ignored --nocapture --test-threads 1`

## Targets (spec "Wydajność") and results

| | target | result |
|---|---|---|
| small text edit A → B through both daemons, 100 000-file project | p50 < 1 s, p95 < 3 s | **p50 0.53 s, p95 1.16 s** (50 samples) |
| same, small project (E2E test) | | 0.33–0.44 s |
| full rescan of 100 000 files with a warm stat cache (`spoko sync`, nothing changed — includes process start and engine load) | < 2 s | **1.54 s** |
| idle daemon, typical project (5 000 files) | < 50 MB RAM, ~0% CPU | **25 MiB, 0.03%** |
| idle daemon, 100 000 files | no degradation | 94 MiB, 0.02% CPU |
| client binary | "a dozen MB or less" | 6.6 MiB (`spoko`, stripped, LTO) |

Not targets, for reference (100 000 files of ~10 bytes): `spoko init` (scan + hash + encrypt
+ upload) 8.9 s, `spoko clone` (download + checkout) 22 s.

## What it took

The first 100 000-file run missed every target (init hit the 120 s request timeout, clone
took 561 s, an empty rescan 11 s). Profiling with `sample` found, in order:

1. **One file + fsync per object on the server.** Upload batches are now appended to one
   pack file per batch (one fsync) with an SQLite index.
2. **One loose object per file in the local git store** (`link`/`unlink` dominated).
   `Store::batch` buffers small objects and writes a single git packfile (own pack v2
   encoder + libgit2 indexer); big blobs stay loose so they can still be streamed.
3. **`F_FULLFSYNC` per checked-out file** (Rust's `sync_all` on Apple flushes the whole drive
   cache, ~5 ms). Files get a plain `fsync`; the state file written after an apply uses
   `F_FULLFSYNC` as the barrier.
4. **Rebuilding all 18 500 trees through libgit2's TreeBuilder** (an existence check per
   entry, `utimes` on every existing object). Trees are now serialized directly in one pass
   over the sorted map, and trees known to exist skip the lookup.
5. **libgit2 re-listing `objects/pack` on every lookup miss** (`git_odb_refresh`) — every
   object during a download is a miss. Existence checks use `NO_REFRESH`; the store refreshes
   after writing its own packs.
6. **A full tree walk on every engine open** to delete crash leftovers — now only after an
   interrupted apply (marker file).
7. **Single-threaded scan and checkout.** The full scan walks with up to 8 threads (stat,
   filters, stat-cache lookups and small reads); checkout writes small files on a pool, one
   directory per task (APFS serializes creates/renames within a directory).
8. **Writing the 100 000-entry stat cache on every change.** Large caches are written at most
   every 30 s and on shutdown (a stale cache only costs re-hashing recently changed files).
