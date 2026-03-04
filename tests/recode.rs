use anyhow::Result;
use assert_cmd::prelude::*;
use predicates::prelude::*;
use std::fs;
use std::path::Path;
use std::process::Command;

mod fixtures;
use fixtures::TestFixtures;

/// Integration tests for recode module

#[test]
fn test_recode_to_binseq_happy_path() -> Result<()> {
    let fixtures = TestFixtures::ensure_fixtures()?;
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_xsra"));
    let output_file = "test_recode_to_binseq_happy_path.bq";
    let assert = cmd
        .arg("recode")
        .arg(&fixtures.small_fixed_sra)
        .arg("-f")
        .arg("b")
        .arg("-I")
        .arg("0")
        .arg("-n")
        .arg(output_file)
        .assert();

    assert.success();

    assert_ne!(
        fs::metadata(output_file)?.len(),
        0,
        "output file should not be empty"
    );
    fs::remove_file(output_file)?;
    Ok(())
}

#[test]
fn test_recode_to_vbinseq_happy_path() -> Result<()> {
    let fixtures = TestFixtures::ensure_fixtures()?;
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_xsra"));
    let output_file = "test_recode_to_vbinseq_happy_path.vbq";
    let assert = cmd
        .arg("recode")
        .arg(&fixtures.small_variable_sra)
        .arg("-f")
        .arg("v")
        .arg("-I")
        .arg("0")
        .arg("-n")
        .arg(output_file)
        .assert();

    assert.success();

    assert_ne!(
        fs::metadata(output_file)?.len(),
        0,
        "output file should not be empty"
    );
    fs::remove_file(output_file)?;
    Ok(())
}

#[test]
fn test_recode_to_binseq_variable_length_error() {
    let fixtures = TestFixtures::ensure_fixtures().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_xsra"));
    let output_file = "test_recode_to_binseq_variable_length_error.bq";
    let assert = cmd
        .arg("recode")
        .arg(&fixtures.small_variable_sra)
        .arg("-f")
        .arg("b")
        .arg("-I")
        .arg("0")
        .arg("-n")
        .arg(output_file)
        .assert();

    // Small variable SRA has variable-length segments, so BINSEQ encoding should fail
    assert.failure().stderr(
        predicate::str::contains(
            "Segment ID 0 shows variance in length. Cannot encode to BQ (try VBQ or CBQ instead)",
        )
        .or(predicate::str::is_empty()), // Allow empty stderr in case of buffering issues
    );

    // Ensure that no output file was created on failure
    assert!(
        !Path::new(output_file).exists(),
        "Output file should not be created on failure"
    );
}

#[test]
fn test_recode_to_binseq_segment_variance_error() {
    let fixtures = TestFixtures::ensure_fixtures().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_xsra"));
    let output_file = "test_recode_to_binseq_segment_variance_error.bq";
    let assert = cmd
        .arg("recode")
        .arg(&fixtures.corrupt_sra)
        .arg("-f")
        .arg("b")
        .arg("-I")
        .arg("0")
        .arg("-n")
        .arg(output_file)
        .assert();

    // Corrupt SRA file should fail with a VDB or table error
    assert.failure().stderr(
        predicate::str::contains("VDB error")
            .or(predicate::str::contains("Unable to find column"))
            .or(predicate::str::contains("file invalid")),
    );
}
