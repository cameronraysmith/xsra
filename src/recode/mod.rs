use std::fs::File;
use std::io::BufWriter;
use std::path::Path;
use std::sync::Arc;

use anyhow::{bail, Result};
use binseq::{
    write::{BinseqWriterBuilder, Format},
    BitSize, SequencingRecordBuilder,
};
use log::{debug, info};
use ncbi_vdb_sys::SraReader;
use parking_lot::Mutex;

use crate::cli::RecodeArgs;
use crate::describe::describe_inner;
use crate::prefetch::identify_url;
use crate::utils::get_num_records;

const THREAD_UPDATE_INTERVAL: usize = 1024;

pub fn recode(args: &RecodeArgs) -> Result<()> {
    args.validate()?;
    let accession = if !Path::new(&args.input.accession).exists() {
        info!(accession = args.input.accession.as_str(); "Identifying SRA data URL for accession");
        let runtime = tokio::runtime::Runtime::new()?;
        let url = runtime.block_on(identify_url(&args.input.accession, &args.input.options))?;
        info!(url = url.as_str(); "Streaming SRA records from URL");
        url
    } else {
        debug!(path = args.input.accession.as_str(); "Using local SRA file");
        args.input.accession.to_string()
    };

    recode_inner(
        &accession,
        &args.output.name(),
        args.primary_sid(),
        args.extended_sid(),
        args.output.bitsize(),
        args.output.block_size,
        args.runtime.threads(),
        args.output.flavor.to_format(),
    )
}

fn recode_inner(
    accession: &str,
    output_path: &str,
    primary_sid: usize,
    extended_sid: Option<usize>,
    bitsize: BitSize,
    block_size: usize,
    num_threads: u64,
    format: Format,
) -> Result<()> {
    let output = File::create(output_path).map(BufWriter::new)?;
    let mut builder = BinseqWriterBuilder::new(format)
        .headers(false)
        .quality(true)
        .bitsize(bitsize)
        .paired(extended_sid.is_some())
        .block_size(block_size);

    // BQ requires fixed sequence lengths determined upfront
    if matches!(format, Format::Bq) {
        let stats = describe_inner(accession, 0, 100)?;
        let sid_lengths = stats.segment_lengths();

        let slen = if sid_lengths[primary_sid].fract() == 0.0 {
            sid_lengths[primary_sid] as u32
        } else {
            bail!("Segment ID {primary_sid} shows variance in length. Cannot encode to BQ (try VBQ or CBQ instead)")
        };
        builder = builder.slen(slen);

        if let Some(esid) = extended_sid {
            let xlen = if sid_lengths[esid].fract() == 0.0 {
                sid_lengths[esid] as u32
            } else {
                bail!("Segment ID {esid} shows variance in length. Cannot encode to BQ (try VBQ or CBQ instead)")
            };
            builder = builder.xlen(xlen);
        }
    }

    let g_writer = Arc::new(Mutex::new(builder.build(output)?));

    let num_records = get_num_records(accession)?;
    let records_per_thread = num_records / num_threads;
    let remainder = num_records % num_threads;

    let mut handles = Vec::new();
    for tid in 0..num_threads {
        let start = (tid * records_per_thread) + 1; // 1-indexed
        let stop = if tid == num_threads - 1 {
            start + records_per_thread + remainder - 1
        } else {
            start + records_per_thread - 1
        };
        let t_accession = accession.to_string();
        let mut t_writer = g_writer.lock().new_headless_buffer()?;
        let g_writer = g_writer.clone();

        let handle = std::thread::spawn(move || -> Result<()> {
            let reader = SraReader::new(&t_accession)?;

            for (iter_index, record) in reader.into_range_iter(start as i64, stop)?.enumerate() {
                let record = record?;

                if let Some(esid) = extended_sid {
                    let primary_seg = record.get_segment(primary_sid).unwrap();
                    let extended_seg = record.get_segment(esid).unwrap();
                    let record = SequencingRecordBuilder::default()
                        .s_seq(primary_seg.seq())
                        .opt_s_qual(Some(primary_seg.qual()))
                        .x_seq(extended_seg.seq())
                        .opt_x_qual(Some(extended_seg.qual()))
                        .build()?;
                    t_writer.push(record)?;
                } else {
                    let primary_seg = record.get_segment(primary_sid).unwrap();
                    let record = SequencingRecordBuilder::default()
                        .s_seq(primary_seg.seq())
                        .opt_s_qual(Some(primary_seg.qual()))
                        .build()?;
                    t_writer.push(record)?;
                };

                if iter_index % THREAD_UPDATE_INTERVAL == 0 {
                    g_writer.lock().ingest(&mut t_writer)?;
                }
            }

            g_writer.lock().ingest(&mut t_writer)?;

            Ok(())
        });

        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap().unwrap();
    }

    g_writer.lock().finish()?;

    Ok(())
}
