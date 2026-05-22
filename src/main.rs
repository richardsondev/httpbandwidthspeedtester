// SPDX-License-Identifier: MIT

use bytes::Bytes;
use chrono::Local;
use futures_util::StreamExt;
use http_body_util::{BodyStream, Empty};
use httpbandwidthspeedtester::{compute_ranges, Speed};
use hyper::{
    header::{CONTENT_LENGTH, RANGE},
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
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

struct DownloadState {
    bytes_last_second: u64,
    past_seconds: VecDeque<u64>,
    last_second: Instant,
    total_bytes_downloaded: u64,
}

/*
Update the state with a new chunk of data
*/
async fn update_state(chunk: Bytes, download_state: &Arc<Mutex<DownloadState>>) {
    let mut state = download_state.lock().await;
    let bytes = chunk.len() as u64;

    // Add the bytes to the total of the last second
    state.bytes_last_second += bytes;

    // Check if a second has passed
    if state.last_second.elapsed() >= Duration::from_secs(1) {
        // Push the number of bytes of the last second into past_seconds
        // and remove old seconds if necessary
        let bytes_sec = state.bytes_last_second;
        state.past_seconds.push_back(bytes_sec);
        if state.past_seconds.len() > 10 {
            state.past_seconds.pop_front();
        }

        // Reset bytes_last_second and last_second
        state.bytes_last_second = 0;
        state.last_second = Instant::now();
    }

    // Add the bytes to the total_bytes_downloaded
    state.total_bytes_downloaded += bytes;
}

/*
Download a range of bytes from the file
*/
async fn start_download(
    client: Arc<Client<HttpsConnector<HttpConnector>, Empty<Bytes>>>,
    url: Uri,
    range: Option<String>,
    download_state: Arc<Mutex<DownloadState>>,
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
            update_state(chunk.clone(), &download_state).await;
        }
    }

    Ok(())
}

/*
Print the download speed every second
*/
async fn print_loop(download_state: Arc<Mutex<DownloadState>>) {
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;

        let state = download_state.lock().await;

        // Calculate the average download speed over the last 10 seconds
        let total_past_bytes: u64 = state.past_seconds.iter().sum();
        let avg_speed: u64 = total_past_bytes / max(state.past_seconds.len() as u64, 1);

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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Parse the URL from the command line arguments
    let url: String = std::env::args().nth(1).expect("URL is required");
    let url: Uri = url.parse::<Uri>()?;

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

    // Calculate the number of bytes to download in each thread
    let cpu_count: u64 = std::thread::available_parallelism()
        .map(NonZeroUsize::get)
        .unwrap_or(1) as u64;

    let ranges: Vec<Option<String>> = match content_length {
        Some(cl) if cl > 0 => compute_ranges(Some(cl), cpu_count),
        _ => {
            eprintln!(
                "Warning: Server did not provide Content-Length, falling back to single-worker streaming download"
            );
            compute_ranges(None, cpu_count)
        }
    };

    // Create the shared download state
    let download_state: Arc<Mutex<DownloadState>> = Arc::new(Mutex::new(DownloadState {
        bytes_last_second: 0,
        past_seconds: VecDeque::with_capacity(10),
        last_second: Instant::now(),
        total_bytes_downloaded: 0,
    }));

    // Start the print loop
    let print_handle = tokio::spawn(print_loop(download_state.clone()));
    let start_time = Instant::now();

    // Start the downloads
    let mut handles: Vec<tokio::task::JoinHandle<Result<(), Box<dyn Error + Send + Sync>>>> =
        Vec::new();
    for range in ranges {
        let client: Arc<Client<HttpsConnector<HttpConnector>, Empty<Bytes>>> = Arc::clone(&client);
        let download_state: Arc<Mutex<DownloadState>> = download_state.clone();
        let handle: tokio::task::JoinHandle<Result<(), Box<dyn Error + Send + Sync>>> =
            tokio::spawn(start_download(client, url.clone(), range, download_state));
        handles.push(handle);
    }

    // Wait for the downloads to finish
    for handle in handles {
        handle.await??;
    }

    // Stop the print loop
    print_handle.abort();

    // Print out the total bytes downloaded and the average speed
    let state: tokio::sync::MutexGuard<'_, DownloadState> = download_state.lock().await;
    let elapsed_secs = start_time.elapsed().as_secs_f64().max(1e-9);
    let avg_speed: u64 = (state.total_bytes_downloaded as f64 / elapsed_secs) as u64;
    let speed = Speed::from_bps(avg_speed);
    println!(
        "Download completed: {} bytes downloaded at an average speed of {} B/s, {} KiB/s, {} MiB/s",
        state.total_bytes_downloaded, speed.bps, speed.kib_s, speed.mib_s
    );

    Ok(())
}
