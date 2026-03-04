# xsra

[![MIT licensed](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE.md)
[![Crates.io](https://img.shields.io/crates/d/xsra?color=orange&label=crates.io)](https://crates.io/crates/xsra)

A performant and storage-efficient CLI tool to extract sequences from an SRA archive with support for FASTA, FASTQ, and [BINSEQ](https://github.com/arcinstitute/binseq) outputs.

## Overview

The NCBI [Sequence Read Archive (SRA)](https://www.ncbi.nlm.nih.gov/sra) is a repository of raw sequencing data.
The file format used by the SRA is a complicated binary database format that isn't directly readable by most bioinformatics tools.
This tool makes use of the `ncbi_vdb` c-library through [`ncbi-vdb-sys`](https://github.com/arcinstitute/ncbi-vdb-sys) to interact with the SRA archive with safe abstractions.

This tool is designed to be a fast, storage-efficient, and more convenient replacement for the `fastq-dump` and `fasterq-dump` tools provided by the NCBI.
However, it is not a complete feature-for-feature replacement, and some functionality may be missing.

## Features

- Multi-threaded extraction to FASTA, FASTQ, and [BINSEQ](https://github.com/arcinstitute/binseq) records.
- Optional built-in compression of output files (FASTA, FASTQ) - [gzip, bgzip, zstd]
- Choice of BINSEQ output format (`*.bq`, `*.vbq`, `*.cbq`)
- Minimum read length filtering
- Technical / biological read segment selection
- Spot subsetting
- Stream directly from NCBI without intermediate prefetch
- Prefetch SRA records for faster IO
- Named pipes (FIFO) support

## Limitations

- May not support every possible SRA archive layout (let us know if you encounter one that fails)
- Does not support all the options provided by `fastq-dump` or `fasterq-dump`
- Will not output sequence identifiers in the same format as `fastq-dump` or `fasterq-dump`
- Spot ordering is not guaranteed to be the same as the SRA archive
  - Read segments are in order to keep paired-end reads together, but the order of spots is dependent on the order of completion of the threads.
- Installation bundles `ncbi-vdb` source code and builds it as a static library
  - This may not work on all systems
  - The resulting builds will likely be system-specific and the resulting binary may not be portable.

## Installation

You will need to install the rust package manager [`cargo`](https://rustup.rs/) first.

```bash
# install using cargo
cargo install xsra

# validate installation
xsra --help
```

## Usage

`xsra` can either be run with on-disk accessions or can be streamed from SRA directly.

```bash
# Write all records to stdout (defaults to fastq)
xsra dump <ACCESSION>.sra

# Write all records to stdout (as fasta)
xsra dump <ACCESSION>.sra -fa

# Write all records to stdout (as fastq)
xsra dump <ACCESSION>.sra -fq

# Split records into multiple files (will create an output directory and write files there)
xsra dump <ACCESSION>.sra -s

# Split records into multiple files and compress them (gzip)
xsra dump <ACCESSION>.sra -s -cg

# Split records into multiple files, compress them (zstd), and filter out reads shorter than 11bp
xsra dump <ACCESSION>.sra -s -cz -L 11

# Write all records to stdout but only use 4 threads and compress the output (bgzip)
xsra dump <ACCESSION>.sra -T4 -cb

# Write only the first 100 spots to stdout
xsra dump <ACCESSION>.sra -l 100

# Write only segments 1 and 2 to stdout
xsra dump <ACCESSION>.sra -I 1,2

# Describe the SRA file (spot statistics)
xsra describe <ACCESSION>.sra

# Download an accession to disk
xsra prefetch <ACCESSION>.sra

# Download multiple accessions to disk
xsra prefetch <ACCESSION>.sra <ACCESSION2>.sra <ACCESSION3>.sra
```

You can also write [BINSEQ](https://github.com/arcinstitute/binseq) files (`.bq`, `.vbq`, `.cbq`) directly from SRA without an intermediate FASTA or FASTQ file.
These operations can be done with multiple threads for faster processing as well (following same arguments as above).

```bash
# Write a CBQ file to (output.cbq) selecting segments 1 and 2 (zero-indexed) as primary and extended.
xsra recode <ACCESSION>.sra -I 0,1

# Write a CBQ file to (output.cbq) selecting segments 1 and 2 (zero-indexed) as primary and extended.
xsra recode <ACCESSION>.sra -fc -I 0,1

# Write a BQ file to (output.bq) selecting segment 3 (zero-indexed) as primary.
xsra recode <ACCESSION>.sra -fb -I 2

# Write a VBQ file to (output.vbq) selecting segments 3 and 1 (zero-indexed) as primary and extended.
xsra recode <ACCESSION>.sra -fv -I 3,1
```

You can also use alternative data providers such as `GCP`.
You will need to provide a project ID.

```bash
xsra prefetch <ACCESSION> -P gcp -G <GCP_PROJECT_ID>
```

### Named Pipes (FIFO)

`xsra` supports writing to [named pipes](https://en.wikipedia.org/wiki/Named_pipe) which can lead to improved disk usage by directly streaming records to downstream tools without an intermediary output file.

The [FIFO file](https://www.man7.org/linux/man-pages/man7/fifo.7.html) creation is done by `xsra` and follows the naming format `<prefix>.<segment>.<ext>`.

```bash

# Stream an accession (segments 1,2) on a background thread
xsra dump -T0 SRR27592687 -I 1,2 -sn &

# Pipe segments directly to downstream tools
minimap2 -t12 -xsr <reference.fa> output.seg_1.fq output.seg_2.fq > output.paf
```

The fifo output can be combined with the supported compression flags, in which case, the compressed stream will be written to the named pipes. Named pipes expect that each pipe being written to has some other process reading the data being produced. As such, be certain to have `xsra` produce a named pipe for a segment if and only if the downstream process will consume this named pipe.

## Contributing

Please feel free to open an issue or pull request if you have any suggestions or improvements.
