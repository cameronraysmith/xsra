use crate::cli::{AccessionOptions, MultiInputOptions, Provider};
use anyhow::{anyhow, bail, Result};
use futures::{future::join_all, stream::FuturesUnordered, StreamExt};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use log::{debug, error, info, trace, warn};
use std::{
    fs::File,
    io::{BufWriter, Write},
    sync::{Arc, LazyLock},
    time::Duration,
};
use tokio::{sync::Semaphore, time::sleep};

/// Shared reqwest client for all requests
static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(reqwest::Client::new);

/// Semaphore for rate limiting (NCBI limits to 3 requests per second)
pub const RATE_LIMIT_SEMAPHORE: usize = 3;

/// Checks if the response from NCBI indicates rate limiting
fn is_rate_limited(response: &str) -> bool {
    // Check for the specific JSON rate limit response
    if response.contains("API rate limit exceeded") {
        return true;
    }

    // Check if response is a JSON error with rate limit indicators
    if response.starts_with("{")
        && (response.contains("rate limit") || response.contains("limit exceeded"))
    {
        return true;
    }

    false
}

#[cfg(not(test))]
pub async fn query_entrez(accession: &str) -> Result<String> {
    let query_url = format!(
        "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/efetch.fcgi?db=sra&id={}&rettype=full",
        accession
    );
    trace!(accession = accession, url = query_url.as_str(); "Querying NCBI Entrez");
    let response = CLIENT.get(&query_url).send().await?.text().await?;
    trace!(accession = accession, response_len = response.len(); "Received Entrez response");
    Ok(response)
}

// Note: This version of query_entrez is used in tests
#[cfg(test)]
pub async fn query_entrez(accession: &str) -> Result<String> {
    match accession {
        // For testing empty or invalid accessions
        "" => Ok("no urls found".to_string()),
        accession if accession.contains("INVALID") => Ok("no urls found".to_string()),

        // For testing multiple URL formats
        "SRR123456" => Ok(r#"
                url="https://localhost:12345/sra/SRR123456/SRR123456.sra"
                url="gs://test-bucket/sra/SRR123456/SRR123456.sra"
                url="s3://test-bucket/sra/SRR123456/SRR123456.sra"
            "#
        .to_string()),

        // For testing with both lite and full versions
        "SRR999999" => Ok(r#"
                url="https://localhost:12345/SRR999999.sra"
                url="https://localhost:12345/SRR999999.lite.sra"
            "#
        .to_string()),

        // For testing lite only version
        "SRR_LITE_ONLY" => {
            Ok(r#"url="https://localhost:12345/SRR_LITE_ONLY.lite.sra""#.to_string())
        }

        // For testing default case
        _ => Ok(r#"url="https://localhost:12345/default.sra""#.to_string()),
    }
}

/// Checks if a line in the response is a URL and passes all the filters
fn pass_url_filter(line: &str, accession: &str, provider: Provider) -> bool {
    line.contains("url=")
        && line.contains(accession)
        && !line.contains(".fastq")
        && !line.contains(".vcf")
        && !line.contains(".bam")
        && !line.contains(".gz")
        && line.contains(provider.url_prefix())
}

pub fn parse_url(
    accession: &str,
    response: &str,
    full_quality: bool,
    provider: Provider,
) -> Option<String> {
    for line in response.replace(" ", "\n").split("\n") {
        if pass_url_filter(line, accession, provider) {
            if full_quality && line.contains(".lite") {
                continue;
            }
            if !full_quality && !line.contains(".lite") {
                continue;
            }
            let url = line.replace("url=", "").replace('"', "");
            return Some(url);
        }
    }
    None
}

pub fn parse_url_with_fallback(
    accession: &str,
    response: &str,
    full_quality: bool,
    lite_only: bool,
    provider: Provider,
) -> Option<String> {
    // Try preferred quality type
    if let Some(url) = parse_url(accession, response, full_quality, provider) {
        return Some(url);
    }

    // Fallback from SRA lite to full if needed
    if !lite_only {
        if let Some(url) = parse_url(accession, response, true, provider) {
            warn!(
                accession = accession;
                "Lite quality not available, falling back to full quality"
            );
            return Some(url);
        }
    } else {
        warn!(
            accession = accession;
            "No lite quality found - not performing fallback because `--lite-only` flag in use"
        );
    }

    None
}

pub async fn identify_url(accession: &str, options: &AccessionOptions) -> Result<String> {
    let mut retry_count = 0;

    loop {
        // Break the loop if we've reached max retries
        if retry_count >= options.retry_limit {
            break;
        }

        let entrez_response = query_entrez(accession).await?;

        // Check if we're being rate limited
        if is_rate_limited(&entrez_response) {
            let delay = options.retry_delay + (retry_count * options.retry_delay);
            warn!(
                accession = accession,
                delay_ms = delay,
                attempt = retry_count,
                max_retries = options.retry_limit;
                "Rate limit detected, retrying after delay"
            );

            // Use tokio::time::sleep for asynchronous sleep
            tokio::time::sleep(Duration::from_millis(delay as u64)).await;
            retry_count += 1;
            continue;
        }

        // If we have a valid response, try to parse the URL
        if let Some(url) = parse_url_with_fallback(
            accession,
            &entrez_response,
            options.full_quality,
            options.lite_only,
            options.provider,
        ) {
            match options.provider {
                Provider::Https | Provider::Gcp => return Ok(url),
                _ => {
                    bail!(
                        "Identified the {}-URL, but cannot currently proceed: {url}",
                        options.provider,
                    );
                }
            }
        } else {
            // If we can't parse a URL, break out of the loop to return the error
            break;
        }
    }

    // If we've exhausted retries or couldn't parse a URL, return an error
    bail!("Unable to identify a download URL for accession: <{accession}> with full_quality={} and provider={}",
        options.full_quality,
        options.provider,
    )
}

// Rate-limited version that processes multiple accessions by calling identify_url
pub async fn identify_urls(
    accessions: &[String],
    options: &AccessionOptions,
) -> Result<Vec<(String, Result<String>)>> {
    let total = accessions.len();
    info!(count = total; "Identifying URLs for accessions");

    // Use a semaphore to limit concurrent requests to 3
    let semaphore = Arc::new(Semaphore::new(RATE_LIMIT_SEMAPHORE));
    let mut tasks = Vec::new();

    for accession in accessions {
        let accession_clone = accession.clone();
        let options_clone = options.clone();
        let sem_clone = Arc::clone(&semaphore);

        // Create a task for each accession that respects the semaphore
        let task = tokio::spawn(async move {
            // Acquire permit from semaphore (blocks when 3 permits are already taken)
            let _permit = sem_clone
                .acquire()
                .await
                .expect("Semaphore should not be closed");
            debug!(accession = accession_clone.as_str(); "Identifying URL for accession");

            // Execute the request
            let result = identify_url(&accession_clone, &options_clone).await;

            // The permit is automatically released when it goes out of scope
            // Small delay to ensure we don't exceed rate limits when permits are released in bursts
            sleep(Duration::from_millis(50)).await;

            (accession_clone, result)
        });

        tasks.push(task);
    }

    // Wait for all tasks to complete
    let results = join_all(tasks).await;

    // Process results, handling any JoinError from the spawned tasks.
    // `join_all` preserves task order, which matches the input `accessions`
    // order, so a JoinError can be attributed to its accession rather than
    // being silently dropped.
    let mut processed_results = Vec::with_capacity(accessions.len());
    for (accession, result) in accessions.iter().zip(results) {
        match result {
            Ok(res) => processed_results.push(res),
            Err(e) => {
                error!(accession = accession.as_str(), error:% = e; "Task join error");
                processed_results
                    .push((accession.clone(), Err(anyhow!("task failed to complete: {e}"))));
            }
        }
    }

    Ok(processed_results)
}

/// Download a file from a URL asynchronously
async fn download_url(url: String, path: String, pb: ProgressBar) -> Result<()> {
    let filename = url.split('/').next_back().unwrap_or("");
    trace!(url = url.as_str(), path = path.as_str(); "Starting HTTPS download");
    let client = CLIENT.get(&url).send().await?.error_for_status()?;

    let size = client.content_length().unwrap_or(0);
    pb.set_style(ProgressStyle::default_bar()
        .template(
            "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta}) {msg}")?
        .progress_chars("#>-"));
    pb.set_length(size);
    pb.set_message(filename.to_string());

    let mut file = File::create(&path).map(BufWriter::new)?;
    let mut stream = client.bytes_stream();
    while let Some(item) = stream.next().await {
        let chunk = item?;
        pb.inc(chunk.len() as u64);
        file.write_all(&chunk)?;
    }
    file.flush()?;
    pb.finish();
    debug!(path = path.as_str(), filename = filename; "HTTPS download completed");
    Ok(())
}

/// Download a file from a GCP URL using gsutil
async fn download_url_gcp(
    url: String,
    path: String,
    project_id: String,
    pb: ProgressBar,
) -> Result<()> {
    let filename = url.split('/').next_back().unwrap_or("");
    trace!(
        url = url.as_str(),
        path = path.as_str(),
        project_id = project_id.as_str();
        "Starting GCP download via gsutil"
    );
    pb.set_message(format!("GCP: {}", filename));

    // Set indeterminate progress style - we'll let gsutil show its own progress
    pb.set_style(ProgressStyle::default_spinner().template("{spinner:.green} {msg}")?);

    // Prepare the gsutil command
    let mut cmd = std::process::Command::new("gsutil");
    cmd.arg("-u")
        .arg(project_id)
        .arg("cp")
        .arg(&url)
        .arg(&path)
        // Use inherit to show gsutil's own progress bar in the terminal
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit());

    // Execute the command and wait for it to complete
    let status = cmd.spawn()?.wait()?;

    if !status.success() {
        pb.finish_with_message(format!("Failed to download {}", filename));
        error!(
            filename = filename,
            exit_code:? = status.code();
            "gsutil command failed"
        );
        bail!("gsutil command failed with exit code: {}", status);
    }

    pb.finish_with_message(format!("Downloaded {} successfully", filename));
    debug!(path = path.as_str(), filename = filename; "GCP download completed");
    Ok(())
}

pub async fn prefetch(input: &MultiInputOptions, output_dir: Option<&str>) -> Result<()> {
    let accessions = input.accession_set();

    if accessions.is_empty() {
        bail!("No accessions provided");
    }

    // For a single accession
    if accessions.len() == 1 {
        let url = identify_url(&accessions[0], &input.options).await?;
        let path = match output_dir {
            Some(dir) => format!("{}/{}.sra", dir, &accessions[0]),
            None => format!("{}.sra", &accessions[0]),
        };

        let pb = ProgressBar::new(0);

        return match input.options.provider {
            Provider::Https => download_url(url, path, pb).await,
            Provider::Gcp => {
                let project_id = match &input.options.gcp_project_id {
                    Some(id) => id.to_string(),
                    None => bail!("GCP project ID is required for GCP downloads"),
                };
                download_url_gcp(url, path, project_id, pb).await
            }
            _ => bail!("Unsupported provider: {:?}", input.options.provider),
        };
    }

    // For multiple accessions
    // Step 1: Identify URLs with rate limiting
    let url_results = identify_urls(accessions, &input.options).await?;

    // Step 2: Download files concurrently
    let mp = MultiProgress::new();

    // For HTTPS downloads, we can use FuturesUnordered for full concurrency
    let mut https_downloads = FuturesUnordered::new();

    // For GCP downloads, we'll use a separate Vec since gsutil has its own concurrency management
    let mut gcp_downloads = Vec::new();

    // Accumulate per-accession failures so the command can fail loudly at the
    // end while still attempting every accession (best-effort behavior).
    let mut failures: Vec<String> = Vec::new();

    for (accession, url_result) in url_results {
        match url_result {
            Ok(url) => {
                let path = match output_dir {
                    Some(dir) => format!("{}/{}.sra", dir, accession),
                    None => format!("{}.sra", accession),
                };

                let pb = mp.add(ProgressBar::new(0));
                pb.set_message(format!("Downloading {}", accession));

                match input.options.provider {
                    Provider::Https => {
                        // Carry the accession into the future so download
                        // failures can be attributed back to it.
                        https_downloads.push(async move {
                            let result = download_url(url, path, pb).await;
                            (accession, result)
                        });
                    }
                    Provider::Gcp => {
                        let project_id = match &input.options.gcp_project_id {
                            Some(id) => id.to_string(),
                            None => {
                                error!(
                                    accession = accession.as_str();
                                    "GCP project ID is required for GCP downloads"
                                );
                                failures.push(format!(
                                    "{accession}: GCP project ID is required for GCP downloads"
                                ));
                                continue;
                            }
                        };
                        // We'll collect GCP downloads and process them separately
                        gcp_downloads.push((accession, url, path, project_id, pb));
                    }
                    _ => {
                        error!(
                            accession = accession.as_str(),
                            provider:? = input.options.provider;
                            "Unsupported provider"
                        );
                        failures.push(format!(
                            "{accession}: unsupported provider: {:?}",
                            input.options.provider
                        ));
                        continue;
                    }
                }
            }
            Err(e) => {
                error!(accession = accession.as_str(), error:% = e; "Failed to identify URL");
                failures.push(format!("{accession}: URL resolution failed: {e}"));
            }
        }
    }

    // Process HTTPS downloads concurrently
    while let Some((accession, result)) = https_downloads.next().await {
        if let Err(e) = result {
            error!(accession = accession.as_str(), error:% = e; "HTTPS download failed");
            failures.push(format!("{accession}: download failed: {e}"));
        }
    }

    // Process GCP downloads - since gsutil has its own concurrency management,
    // we'll run them sequentially to avoid overwhelming the terminal output
    for (accession, url, path, project_id, pb) in gcp_downloads {
        if let Err(e) = download_url_gcp(url, path, project_id, pb).await {
            error!(accession = accession.as_str(), error:% = e; "GCP download failed");
            failures.push(format!("{accession}: download failed: {e}"));
        }
    }

    if !failures.is_empty() {
        bail!(
            "prefetch failed for {} accession(s):\n{}",
            failures.len(),
            failures.join("\n")
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    // Helper functions for test setup
    fn create_test_accession_options() -> AccessionOptions {
        AccessionOptions {
            full_quality: true,
            lite_only: false,
            provider: Provider::Https,
            retry_limit: 1,
            retry_delay: 50,
            gcp_project_id: None,
        }
    }

    async fn create_mock_server(path: &str, status: usize, content: &str) -> mockito::ServerGuard {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("GET", path)
            .with_status(status)
            .with_header("content-length", &content.len().to_string())
            .with_body(content)
            .create_async()
            .await;
        server
    }

    fn download_url_test_setup() -> (NamedTempFile, String, ProgressBar) {
        let temp_file = NamedTempFile::new().unwrap();
        let temp_path = temp_file.path().to_string_lossy().to_string();
        let progress_bar = ProgressBar::new(0);
        (temp_file, temp_path, progress_bar)
    }

    // is_rate_limited tests
    #[test]
    fn is_rate_limited_happy_path() {
        assert!(!is_rate_limited(r#"{"status": "success"}"#));
    }

    #[test]
    fn is_rate_limited_detects_rate_limit_errors() {
        assert!(is_rate_limited("API rate limit exceeded"));
        assert!(is_rate_limited(r#"{"error": "API rate limit exceeded"}"#));
        assert!(is_rate_limited(r#"{"message": "rate limit exceeded"}"#));
        assert!(is_rate_limited(r#"{"error": "limit exceeded"}"#));
    }

    // parse_url tests
    #[test]
    fn parse_url_prefers_lite_when_full_quality_false() {
        let response = r#"
            url="https://example.com/SRR123456.sra"
            url="https://example.com/SRR123456.lite.sra"
        "#;
        let result = parse_url("SRR123456", response, false, Provider::Https);
        assert_eq!(
            result,
            Some("https://example.com/SRR123456.lite.sra".to_string())
        );
    }

    #[test]
    fn parse_url_prefers_full_when_full_quality_true() {
        let response = r#"
            url="https://example.com/SRR123456.sra"
            url="https://example.com/SRR123456.lite.sra"
        "#;
        let result = parse_url("SRR123456", response, true, Provider::Https);
        assert_eq!(
            result,
            Some("https://example.com/SRR123456.sra".to_string())
        );
    }

    #[test]
    fn parse_url_filters_unwanted_formats() {
        let response = r#"
            url="https://example.com/SRR999999.sra"
            url="https://example.com/SRR123456.fastq"
            url="https://example.com/SRR123456.sra.gz"
            url="https://example.com/SRR123456.sra"
        "#;
        let result = parse_url("SRR123456", response, true, Provider::Https);
        assert_eq!(
            result,
            Some("https://example.com/SRR123456.sra".to_string())
        );
    }

    #[test]
    fn parse_url_returns_none_when_no_match() {
        assert_eq!(
            parse_url("SRR123456", "no urls here", true, Provider::Https),
            None
        );
    }

    // parse_url_with_fallback tests
    #[test]
    fn fallback_prefers_requested_quality_when_both_available() {
        let response = r#"
            url="https://example.com/SRR123456.sra"
            url="https://example.com/SRR123456.lite.sra"
        "#;

        // Should prefer lite when full_quality=false
        let lite_result =
            parse_url_with_fallback("SRR123456", response, false, false, Provider::Https);
        assert_eq!(
            lite_result,
            Some("https://example.com/SRR123456.lite.sra".to_string())
        );

        // Should prefer full when full_quality=true
        let full_result =
            parse_url_with_fallback("SRR123456", response, true, false, Provider::Https);
        assert_eq!(
            full_result,
            Some("https://example.com/SRR123456.sra".to_string())
        );
    }

    #[test]
    fn fallback_falls_back_when_lite_unavailable() {
        let response = r#"url="https://example.com/SRR123456.sra""#;
        let result = parse_url_with_fallback("SRR123456", response, false, false, Provider::Https);
        assert_eq!(
            result,
            Some("https://example.com/SRR123456.sra".to_string())
        );
    }

    #[test]
    fn fallback_prevents_when_lite_only_true() {
        let response = r#"url="https://example.com/SRR123456.sra""#;
        let result = parse_url_with_fallback("SRR123456", response, false, true, Provider::Https);
        assert_eq!(result, None);
    }

    #[test]
    fn fallback_works_with_lite_only_when_available() {
        let response = r#"url="https://example.com/SRR123456.lite.sra""#;
        let result = parse_url_with_fallback("SRR123456", response, false, true, Provider::Https);
        assert_eq!(
            result,
            Some("https://example.com/SRR123456.lite.sra".to_string())
        );
    }

    #[test]
    fn fallback_handles_conflicting_flags() {
        let response = r#"url="https://example.com/SRR123456.lite.sra""#;
        // full_quality=true + lite_only=true should return None
        let result = parse_url_with_fallback("SRR123456", response, true, true, Provider::Https);
        assert_eq!(result, None);
    }

    #[test]
    fn fallback_returns_none_when_no_urls() {
        let result =
            parse_url_with_fallback("SRR123456", "no urls here", false, false, Provider::Https);
        assert_eq!(result, None);
    }

    // identify_url tests
    #[tokio::test]
    async fn identify_url_succeeds_with_proper_provider() {
        let options = AccessionOptions {
            full_quality: true,
            lite_only: false,
            provider: Provider::Gcp,
            retry_limit: 3,
            retry_delay: 100,
            gcp_project_id: Some("test-project".to_string()),
        };

        // Test with SRR123456 which returns GCP URLs in our mock
        let result = identify_url("SRR123456", &options).await;
        assert!(
            result.is_ok(),
            "Failed to identify GCP URL: {:?}",
            result.err()
        );

        let url = result.unwrap();
        assert_eq!(url, "gs://test-bucket/sra/SRR123456/SRR123456.sra");
    }

    #[tokio::test]
    async fn identify_url_fails_with_unsupported_provider() {
        let options = AccessionOptions {
            full_quality: true,
            lite_only: false,
            provider: Provider::Aws,
            retry_limit: 1,
            retry_delay: 100,
            gcp_project_id: None,
        };

        let result = identify_url("SRR123456", &options).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn identify_url_fails_with_zero_retries() {
        let mut options = create_test_accession_options();
        options.retry_limit = 0;

        let result = identify_url("INVALID", &options).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Unable to identify a download URL"));
    }

    // identify_urls tests
    #[tokio::test]
    async fn identify_urls_succeeds_with_multiple_accessions() {
        let mut options = create_test_accession_options();
        options.full_quality = false;

        let accessions = vec![
            "SRR123456".to_string(),
            "SRR999999".to_string(),
            "SRR_LITE_ONLY".to_string(),
        ];

        let results_wrapper = identify_urls(&accessions, &options).await;
        assert!(
            results_wrapper.is_ok(),
            "Failed to identify URLs: {:?}",
            results_wrapper.err()
        );

        let actual_results_vec: Vec<(String, Result<String, anyhow::Error>)> =
            results_wrapper.unwrap();

        let actual_results: Vec<(String, String)> = actual_results_vec
            .iter()
            .map(|(acc, res)| {
                assert!(
                    res.is_ok(),
                    "Expected Ok for accession {}, got {:?}",
                    acc,
                    res
                );
                (acc.clone(), res.as_ref().unwrap().clone())
            })
            .collect();

        let expected_results: Vec<(String, String)> = vec![
            (
                "SRR123456".to_string(),
                "https://localhost:12345/sra/SRR123456/SRR123456.sra".to_string(),
            ), // Fallback
            (
                "SRR999999".to_string(),
                "https://localhost:12345/SRR999999.lite.sra".to_string(),
            ),
            (
                "SRR_LITE_ONLY".to_string(),
                "https://localhost:12345/SRR_LITE_ONLY.lite.sra".to_string(),
            ),
        ];

        assert_eq!(actual_results, expected_results);
    }

    #[tokio::test]
    async fn identify_urls_handles_mixed_success_failure() {
        let options = create_test_accession_options(); // full_quality is true by default here
        let accessions = vec![
            "SRR123456".to_string(),
            "INVALID_ACCESSION".to_string(),
            "SRR999999".to_string(),
        ];

        let results_wrapper = identify_urls(&accessions, &options).await;
        assert!(
            results_wrapper.is_ok(),
            "identify_urls should not fail even with some invalid accessions"
        );

        let actual_results_vec: Vec<(String, Result<String, anyhow::Error>)> =
            results_wrapper.unwrap();

        let actual_results: Vec<(String, Result<String, ()>)> = actual_results_vec
            .into_iter()
            .map(|(acc, res)| {
                let mapped_res = match res {
                    Ok(url_str) => Ok(url_str),
                    Err(_) => Err(()),
                };
                (acc, mapped_res)
            })
            .collect();

        let expected_results: Vec<(String, Result<String, ()>)> = vec![
            (
                "SRR123456".to_string(),
                Ok("https://localhost:12345/sra/SRR123456/SRR123456.sra".to_string()),
            ),
            ("INVALID_ACCESSION".to_string(), Err(())),
            (
                "SRR999999".to_string(),
                Ok("https://localhost:12345/SRR999999.sra".to_string()),
            ),
        ];

        assert_eq!(actual_results, expected_results);
    }

    // download_url tests
    #[tokio::test]
    async fn download_url_succeeds_with_valid_content() {
        let test_content = "test file data";
        let server = create_mock_server("/test.sra", 200, test_content).await;
        let (_temp_file, temp_path, pb) = download_url_test_setup();

        let url = format!("{}/test.sra", server.url());

        let result = download_url(url, temp_path.clone(), pb).await;
        assert!(result.is_ok(), "Download failed: {:?}", result.err());

        // Verify file contents
        let contents = std::fs::read_to_string(&temp_path).unwrap();
        assert_eq!(contents, test_content);
    }

    #[tokio::test]
    async fn download_url_fails_with_404_error() {
        let error_server = create_mock_server("/error.sra", 404, "").await;
        let (_temp_file, temp_path, pb) = download_url_test_setup();

        let error_url = format!("{}/error.sra", error_server.url());

        let result = download_url(error_url, temp_path, pb).await;
        assert!(result.is_err());
    }

    // prefetch tests
    #[tokio::test]
    async fn prefetch_fails_with_empty_accessions() {
        let input = MultiInputOptions {
            accessions: vec![],
            options: AccessionOptions {
                full_quality: true,
                lite_only: false,
                provider: Provider::Https,
                retry_limit: 1,
                retry_delay: 100,
                gcp_project_id: None,
            },
        };

        let result = prefetch(&input, None).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("No accessions provided"));
    }

    #[tokio::test]
    async fn prefetch_fails_with_unsupported_aws_provider() {
        let input = MultiInputOptions {
            accessions: vec!["SRR123456".to_string()],
            options: AccessionOptions {
                full_quality: true,
                lite_only: false,
                provider: Provider::Aws,
                retry_limit: 1,
                retry_delay: 100,
                gcp_project_id: None,
            },
        };

        let result = prefetch(&input, None).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert_eq!(
            err_msg,
            "Identified the aws-URL, but cannot currently proceed: s3://test-bucket/sra/SRR123456/SRR123456.sra"
        );
    }

    #[tokio::test]
    async fn prefetch_fails_with_gcp_provider_missing_project_id() {
        let input = MultiInputOptions {
            accessions: vec!["SRR123456".to_string()],
            options: AccessionOptions {
                full_quality: true,
                lite_only: false,
                provider: Provider::Gcp,
                retry_limit: 1,
                retry_delay: 100,
                gcp_project_id: None,
            },
        };

        let result = prefetch(&input, None).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("GCP project ID is required for GCP downloads"));
    }

    #[tokio::test]
    async fn prefetch_multi_fails_when_any_url_resolution_fails() {
        // Both accessions contain "INVALID", so the test `query_entrez` returns
        // "no urls found" for each and resolution fails with no network access.
        let input = MultiInputOptions {
            accessions: vec!["INVALID_A".to_string(), "INVALID_B".to_string()],
            options: create_test_accession_options(),
        };

        let result = prefetch(&input, None).await;
        assert!(
            result.is_err(),
            "multi-accession prefetch must fail when an accession cannot be resolved"
        );

        // The summary error must identify every failed accession, not just the first.
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("INVALID_A"), "missing INVALID_A: {err_msg}");
        assert!(err_msg.contains("INVALID_B"), "missing INVALID_B: {err_msg}");
    }
}
