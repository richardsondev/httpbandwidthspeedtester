// SPDX-License-Identifier: MIT

//! Binary entrypoint for the HTTP bandwidth speed-tester.
//!
//! Wires up the helpers in the sibling library crate
//! (`httpbandwidthspeedtester::lib`) to a hyper-based HTTPS client, a
//! per-second `print_loop`, and a fan-out of range-aware workers. Run
//! with `httpbandwidthspeedtester <URL>`; see `--help` for details.

use bytes::Bytes;
use chrono::Local;
use clap::Parser;
use futures_util::StreamExt;
use http_body_util::{BodyStream, Empty};
use httpbandwidthspeedtester::{compute_ranges, Speed};
use hyper::{
    header::{ACCEPT_RANGES, CONTENT_LENGTH, RANGE},
    http::HeaderValue,
    Request, Uri,
};
use hyper_tls::HttpsConnector;
use hyper_util::client::legacy::{connect::HttpConnector, Client};
use hyper_util::rt::TokioExecutor;
use std::cmp::max;
use std::collections::VecDeque;
use std::error::Error;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Per-second and lifetime byte counters shared between workers and the
/// `print_loop`. Lock-free: every operation is a relaxed atomic.
struct Counters {
    /// Bytes accumulated during the in-progress one-second window.
    /// Reset by `print_loop` via [`Counters::take_bucket`].
    bytes_current_bucket: AtomicU64,
    /// Total bytes downloaded across all workers since startup.
    total_bytes_downloaded: AtomicU64,
}

impl Counters {
    /// Construct a fresh set of zeroed counters.
    const fn new() -> Self {
        Self {
            bytes_current_bucket: AtomicU64::new(0),
            total_bytes_downloaded: AtomicU64::new(0),
        }
    }

    /// Add `bytes` to both the current-second bucket and the lifetime
    /// total. Called once per body chunk from each worker.
    fn record(&self, bytes: u64) {
        self.bytes_current_bucket
            .fetch_add(bytes, Ordering::Relaxed);
        self.total_bytes_downloaded
            .fetch_add(bytes, Ordering::Relaxed);
    }

    /// Atomically read the current-second bucket and zero it. Called
    /// once per second by `print_loop`.
    fn take_bucket(&self) -> u64 {
        self.bytes_current_bucket.swap(0, Ordering::Relaxed)
    }

    /// Snapshot of the lifetime total bytes downloaded.
    fn total(&self) -> u64 {
        self.total_bytes_downloaded.load(Ordering::Relaxed)
    }
}

/*
Download a range of bytes from the file
*/
async fn start_download(
    client: Arc<Client<HttpsConnector<HttpConnector>, Empty<Bytes>>>,
    url: Uri,
    range: Option<String>,
    counters: Arc<Counters>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Prepare the request
    let mut request = Request::new(Empty::<Bytes>::new());
    *request.method_mut() = hyper::Method::GET;
    *request.uri_mut() = url.clone();
    if let Some(r) = range {
        request
            .headers_mut()
            .insert(RANGE, HeaderValue::from_str(&r)?);
    }

    // Send the request
    let res = client.request(request).await?;
    let body = res.into_body();
    let mut body_stream = BodyStream::new(body);

    // Process each chunk of data as it arrives
    while let Some(frame_result) = body_stream.next().await {
        let frame = frame_result?;
        if let Some(chunk) = frame.data_ref() {
            counters.record(chunk.len() as u64);
        }
    }

    Ok(())
}

/*
Print the download speed every second
*/
async fn print_loop(counters: Arc<Counters>) {
    let mut past_seconds: VecDeque<u64> = VecDeque::with_capacity(10);

    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;

        let bytes_this_second = counters.take_bucket();
        past_seconds.push_back(bytes_this_second);
        if past_seconds.len() > 10 {
            past_seconds.pop_front();
        }

        // Calculate the average download speed over the last 10 seconds
        let total_past_bytes: u64 = past_seconds.iter().sum();
        let avg_speed: u64 = total_past_bytes / max(past_seconds.len() as u64, 1);

        // Print the average speed
        let speed = Speed::from_bps(avg_speed);

        println!(
            "[{}] Average speed: {} B/s, {} KiB/s, {} MiB/s",
            Local::now().format("%Y-%m-%d %H:%M:%S"),
            speed.bps,
            speed.kib_s,
            speed.mib_s
        );
    }
}

/// Command-line arguments for the speedtester binary.
///
/// One positional argument is accepted today — the URL to download. Using
/// `clap` here instead of `std::env::args().nth(1).expect(…)` gives us:
///   * a friendly `error: the following required arguments were not provided`
///     message when the URL is missing, instead of a panic backtrace,
///   * `--help` and `--version` for free, and
///   * up-front validation of the URL scheme so callers see a clear error
///     instead of a hyper connector failure deep in the stack.
#[derive(Parser, Debug)]
#[command(
    name = "httpbandwidthspeedtester",
    version,
    about = "Measure HTTP/HTTPS download bandwidth with parallel range requests",
    long_about = None,
)]
struct Cli {
    /// URL of the file to download (must use http:// or https://).
    #[arg(value_name = "URL", value_parser = parse_http_uri)]
    url: Uri,
}

/// Parse the URL argument into a `Uri` and require an http/https scheme.
///
/// `hyper-tls` only supports those two schemes; anything else (e.g. `ftp://`,
/// `file://`, a bare hostname with no scheme at all) produces a connector
/// error several stack frames deep. Rejecting it here gives the user a much
/// clearer message.
fn parse_http_uri(s: &str) -> Result<Uri, String> {
    let uri: Uri = s.parse().map_err(|e| format!("invalid URL: {e}"))?;
    match uri.scheme_str() {
        Some("http" | "https") => Ok(uri),
        Some(other) => Err(format!(
            "unsupported URL scheme `{other}://`; expected http:// or https://"
        )),
        None => Err("URL is missing a scheme; expected http:// or https://".to_string()),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let Cli { url } = Cli::parse();

    // Create the HTTP client
    let https: HttpsConnector<HttpConnector> = HttpsConnector::new();
    let client: Client<HttpsConnector<HttpConnector>, Empty<Bytes>> =
        Client::builder(TokioExecutor::new()).build::<_, Empty<Bytes>>(https);
    let client: Arc<Client<HttpsConnector<HttpConnector>, Empty<Bytes>>> = Arc::new(client);

    // Send a HEAD request to get the content length
    let req = Request::builder()
        .method(hyper::Method::HEAD)
        .uri(url.clone())
        .body(Empty::<Bytes>::new())?;
    let res = client.request(req).await?;
    if !res.status().is_success() {
        return Err(format!("Server returned HTTP {}", res.status()).into());
    }
    let headers = res.headers();
    let content_length: Option<u64> = headers
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());

    // Determine if the server advertises range support. The HTTP spec says the
    // valid values are `bytes` (ranges supported) or `none` (explicitly not
    // supported); a missing header is also treated as "no support" because we
    // cannot rely on it. When ranges aren't supported, fanning out N range
    // requests would either fail or — worse — cause each worker to receive the
    // entire body (some servers silently ignore the `Range` header and answer
    // with `200 OK`), inflating the reported byte count by `cpu_count`.
    let accepts_ranges: bool = headers
        .get(ACCEPT_RANGES)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',')
                .any(|tok| tok.trim().eq_ignore_ascii_case("bytes"))
        });

    // Calculate the number of bytes to download in each thread
    let cpu_count: u64 = std::thread::available_parallelism().map_or(1, NonZeroUsize::get) as u64;

    let ranges: Vec<Option<String>> = match content_length {
        Some(cl) if cl > 0 && accepts_ranges => compute_ranges(Some(cl), cpu_count),
        Some(_) if !accepts_ranges => {
            eprintln!(
                "Warning: Server does not advertise `Accept-Ranges: bytes`, falling back to single-worker sequential download"
            );
            compute_ranges(None, cpu_count)
        }
        _ => {
            eprintln!(
                "Warning: Server did not provide Content-Length, falling back to single-worker streaming download"
            );
            compute_ranges(None, cpu_count)
        }
    };

    // Create the shared download counters
    let counters: Arc<Counters> = Arc::new(Counters::new());

    // Start the print loop
    let print_handle = tokio::spawn(print_loop(counters.clone()));
    let start_time = Instant::now();

    // Start the downloads
    let mut handles: Vec<tokio::task::JoinHandle<Result<(), Box<dyn Error + Send + Sync>>>> =
        Vec::new();
    for range in ranges {
        let client: Arc<Client<HttpsConnector<HttpConnector>, Empty<Bytes>>> = Arc::clone(&client);
        let counters: Arc<Counters> = counters.clone();
        let handle: tokio::task::JoinHandle<Result<(), Box<dyn Error + Send + Sync>>> =
            tokio::spawn(start_download(client, url.clone(), range, counters));
        handles.push(handle);
    }

    // Wait for the downloads to finish
    for handle in handles {
        handle.await??;
    }

    // Stop the print loop
    print_handle.abort();

    // Print out the total bytes downloaded and the average speed
    let total_bytes = counters.total();
    let elapsed_secs = start_time.elapsed().as_secs_f64().max(1e-9);
    // The cast triplet (u64 → f64 → u64) is a pragmatic choice for a
    // human-readable B/s figure: for byte counts up to ~9 PB the f64
    // mantissa carries enough precision, and the final value is
    // guaranteed non-negative and bounded by `total_bytes`, so neither
    // sign loss nor truncation is observable in practice.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let avg_speed: u64 = (total_bytes as f64 / elapsed_secs) as u64;
    let speed = Speed::from_bps(avg_speed);
    println!(
        "Download completed: {} bytes downloaded at an average speed of {} B/s, {} KiB/s, {} MiB/s",
        total_bytes, speed.bps, speed.kib_s, speed.mib_s
    );

    Ok(())
}
