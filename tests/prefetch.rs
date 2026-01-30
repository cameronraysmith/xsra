use assert_cmd::cargo;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

/// Integration tests for prefetch using CLI approach

#[test]
#[ignore = "requires network access"]
fn test_prefetch_multiple_accessions_https() {
    let temp_dir = TempDir::new().unwrap();
    let output_path = temp_dir.path();

    // Use same accessions as fixtures to save time
    let mut cmd = cargo::cargo_bin_cmd!("xsra");
    cmd.args(&[
        "prefetch",
        "SRR5150787", // Small variable-length SRA (~1.7MB)
        "SRR1574235", // Small fixed-length SRA (~17MB)
        "--output",
        output_path.to_str().unwrap(),
        "--provider",
        "https",
        "--retry-limit",
        "3",
    ]);

    let assert = cmd.assert();
    assert.success();

    // Verify both files were downloaded
    let files: Vec<_> = fs::read_dir(output_path)
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    assert_eq!(
        files.len(),
        2,
        "Expected two files to be downloaded for two accessions"
    );

    // Verify both files are not empty
    for file in files {
        assert!(
            file.metadata().unwrap().len() > 0,
            "Downloaded file {:?} is empty",
            file.file_name()
        );
    }
}

#[test]
#[ignore = "requires network access"]
fn test_prefetch_invalid_accession_https() {
    let temp_dir = TempDir::new().unwrap();
    let output_path = temp_dir.path();

    let mut cmd = cargo::cargo_bin_cmd!("xsra");
    cmd.args(&[
        "prefetch",
        "INVALID_ACCESSION_12345",
        "--output",
        output_path.to_str().unwrap(),
        "--provider",
        "https",
        "--retry-limit",
        "1",
    ]);

    let assert = cmd.assert();
    assert.failure().stderr(
        predicate::str::contains("Unable to identify a download URL")
            .or(predicate::str::contains("API rate limit")),
    );
}

#[test]
#[ignore = "requires network access"]
fn test_prefetch_lite_vs_full_quality() {
    let temp_dir_lite = TempDir::new().unwrap();
    let output_path_lite = temp_dir_lite.path();

    // First, download lite version (prefer lite, allow fallback)
    let mut cmd_lite = cargo::cargo_bin_cmd!("xsra");
    cmd_lite.args(&[
        "prefetch",
        "SRR5150787", // Small variable SRA that should have lite version
        "--output",
        output_path_lite.to_str().unwrap(),
        "--provider",
        "https",
        "--retry-limit",
        "3",
        // Default is full_quality=false, lite_only=false (prefer lite with fallback)
    ]);

    let assert_lite = cmd_lite.assert();
    assert_lite.success();

    let lite_file_path = fs::read_dir(output_path_lite)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let lite_size = fs::metadata(&lite_file_path).unwrap().len();

    // Then, download full version
    let temp_dir_full = TempDir::new().unwrap();
    let output_path_full = temp_dir_full.path();

    let mut cmd_full = cargo::cargo_bin_cmd!("xsra");
    cmd_full.args(&[
        "prefetch",
        "SRR5150787",
        "--output",
        output_path_full.to_str().unwrap(),
        "--provider",
        "https",
        "--retry-limit",
        "3",
        "--full-quality", // Request full quality version
    ]);

    let assert_full = cmd_full.assert();
    assert_full.success();

    let full_file_path = fs::read_dir(output_path_full)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let full_size = fs::metadata(&full_file_path).unwrap().len();

    // Full should be larger than lite (if lite was actually downloaded)
    assert!(
        full_size >= lite_size,
        "Expected full quality file ({}) to be >= lite quality file ({})",
        full_size,
        lite_size
    );
}
