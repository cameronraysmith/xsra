# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

`xsra` is a Rust CLI that extracts sequences from NCBI SRA archives, intended as a faster,
storage-efficient replacement for `fastq-dump`/`fasterq-dump`. It links the `ncbi_vdb` C
library through the [`ncbi-vdb-sys`](https://github.com/arcinstitute/ncbi-vdb-sys) crate and
outputs FASTA, FASTQ, or [BINSEQ](https://github.com/arcinstitute/binseq) (`.bq`/`.vbq`/`.cbq`).

## Commands

```bash
cargo build                 # debug build
cargo build --release       # release build (use for any perf testing)
cargo test                  # run all integration tests
cargo test test_simple_fastq_dump_cli   # run a single test by name
cargo clippy --all-targets  # lint
cargo fmt                   # format
```

Runtime logging is controlled by the `XSRA_LOG` env var (e.g. `XSRA_LOG=debug`), parsed by
`env_logger`; default level is `info`.

### Tests require network access

Integration tests in `tests/` use `TestFixtures` (`tests/fixtures/setup.rs`), which **downloads
real SRA files from NCBI on first run** (`SRR5150787` ~1.7MB variable-length, `SRR1574235` ~17MB
fixed-length) into `tests/fixtures/data/`. Subsequent runs reuse the cached files. Corrupt and
invalid fixtures are synthesized locally. A first `cargo test` without internet will fail.

## Domain model

SRA terminology drives the whole codebase:
- A **spot** (a.k.a. record) is one sequencing event, addressed by a 1-based index.
- A spot contains one or more **segments** (reads), each with a zero-based segment id (`sid()`).
  Segments may be **technical** (e.g. barcodes/adapters) or biological.
- `-I/--include` selects segments by sid; `-t` skips technical segments; `-L` filters by length;
  `-l/--limit` caps the number of *spots* (not reads).

## Architecture

The four subcommands are dispatched in `src/main.rs` from the clap tree in `src/cli/`. Only
`prefetch` is fully async (tokio); the other commands are synchronous but may spin up a short-lived
tokio runtime solely to resolve a remote accession URL.

- **`cli/`** — clap arg structs, composed via `#[clap(flatten)]`. Reusable groups: `InputOptions`
  / `AccessionOptions` (accession + provider + retry), `FilterOptions`, `RuntimeOptions` (threads).
  `Provider` is `Https | Gcp | Aws` (gcp requires `-G <project>`). Each command has its own output
  struct (`DumpOutput`, `RecodeOutput`, etc.).

- **`dump/`** — multi-threaded extraction (`dump/mod.rs::launch_threads`). The spot range is split
  evenly across N threads; each thread opens its *own* `SraReader` over its sub-range, fills
  thread-local buffers, and flushes to a single shared writer (`Arc<Mutex<BoxedSegmentWriter>>`)
  every `RECORD_CAPACITY` spots. **Spot output order is not deterministic** — it depends on thread
  completion order; paired segments from one spot stay together. Per-segment statistics are summed
  across threads at join. Empty per-segment files are deleted afterward unless `--keep-empty`.

- **`recode/`** — writes BINSEQ directly from SRA without an intermediate FASTQ. Requires exactly
  1 or 2 included segments (`include[0]` = primary, `include[1]` = extended/paired); validated in
  `RecodeArgs::validate`. Flavor `c`=CBQ (default), `b`=BQ, `v`=VBQ.

- **`prefetch/`** — async download of accessions to disk. `identify_url` (also used by dump/recode
  to stream remote accessions) resolves an accession to a concrete URL via the NCBI SRA Data
  Locator API, honoring provider and lite-vs-full quality preference, with retry/backoff.

- **`describe/`** — reads a sample of spots and reports per-segment statistics.

- **`output.rs`** (crate root) — `Compression` (uncompressed/gzip/bgzip/zstd; gzip & bgzip via
  parallel `gzp`, zstd via `zstd`), `OutputFileType` (regular file / named pipe / stdout), path
  construction, and FIFO creation. `dump/output.rs` builds the actual segment writers on top of it.

### lib vs bin

`src/lib.rs` re-exports all modules so integration tests can call internals directly (e.g.
`xsra::dump::dump`, `xsra::prefetch::prefetch`). The constants `BUFFER_SIZE` and `RECORD_CAPACITY`
are defined in **both** `main.rs` and `lib.rs` — keep them in sync if changed.

## Conventions

- Errors use `anyhow::Result` throughout; `bail!` for user-facing validation failures.
- Structured logging via the `log` crate with key-value fields (`info!(url = ...; "msg")`).
- The `ncbi_vdb` static library is built from bundled source at install time, so builds are
  system-specific and binaries are not portable.
