// SPDX-License-Identifier: MIT

//! End-to-end integration tests that spin up a mock HTTP server with
//! `wiremock` and shell out to the release binary so the assertions
//! cover the whole code path including argv parsing, the HEAD probe,
//! the per-worker range fan-out, and the final summary line.
//!
//! Scenarios covered:
//!
//! * `serves_file_with_content_length` — happy path, exact byte count.
//! * `serves_file_without_content_length` — B1 regression: chunked /
//!   streaming fallback, *not* a fan-out that downloads
//!   `N × file_size` bytes.
//! * `non_success_status_exits_non_zero` — B2 regression: a 404 must
//!   fail the binary.
//! * `accept_ranges_none_still_succeeds` — B5 regression: a server
//!   that advertises `Accept-Ranges: none` must fall back to a single
//!   worker and report exactly `file_size` bytes — never
//!   `cpu_count × file_size`.
//! * `slow_server_completes` — sanity: a server that trickles bytes
//!   still finishes correctly.

use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::Command;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Respond, ResponseTemplate};

/// Path to the binary under test. Cargo sets `CARGO_BIN_EXE_<name>` when
/// running integration tests, so this is rebuilt and located automatically.
const fn bin_path() -> &'static str {
    env!("CARGO_BIN_EXE_httpbandwidthspeedtester")
}

/// Run the binary against `url` with a generous wall-clock timeout and return
/// (`exit_code`, `stdout`, `stderr`).
async fn run_binary(url: &str) -> (i32, String, String) {
    let mut child = Command::new(bin_path())
        .arg(url)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn binary");

    let mut stdout = child.stdout.take().expect("stdout");
    let mut stderr = child.stderr.take().expect("stderr");

    // Read both streams concurrently with a hard timeout so a hung test
    // doesn't wedge CI.
    let read_streams = async {
        let mut out = String::new();
        let mut err = String::new();
        let read_out = stdout.read_to_string(&mut out);
        let read_err = stderr.read_to_string(&mut err);
        let _ = tokio::join!(read_out, read_err);
        (out, err)
    };

    let (out, err) = tokio::time::timeout(Duration::from_secs(60), read_streams)
        .await
        .expect("binary timed out");

    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("wait timed out")
        .expect("wait failed");

    (status.code().unwrap_or(-1), out, err)
}

/// Extract the `<bytes>` value from the binary's final summary line:
///   "Download completed: <bytes> bytes downloaded at an average speed …"
fn parse_total_bytes(stdout: &str) -> Option<u64> {
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("Download completed: ") {
            if let Some((num, _)) = rest.split_once(" bytes downloaded") {
                return num.trim().parse().ok();
            }
        }
    }
    None
}

#[tokio::test]
async fn serves_file_with_content_length() {
    let mock = MockServer::start().await;
    let body = vec![0x42u8; 64 * 1024]; // 64 KiB

    // Respond to both HEAD (the probe) and GET (the range workers).
    Mock::given(method("HEAD"))
        .and(path("/file"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Length", body.len().to_string().as_str())
                .insert_header("Accept-Ranges", "bytes"),
        )
        .mount(&mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/file"))
        .respond_with(RangeResponder { body: body.clone() })
        .mount(&mock)
        .await;

    let url = format!("{}/file", mock.uri());
    let (code, stdout, stderr) = run_binary(&url).await;

    assert_eq!(
        code, 0,
        "binary exited non-zero\nstdout: {stdout}\nstderr: {stderr}"
    );
    let total = parse_total_bytes(&stdout).expect("no summary line");
    assert_eq!(total, body.len() as u64, "expected exactly file_size bytes");
}

#[tokio::test]
async fn serves_file_without_content_length() {
    // B1 regression: server omits Content-Length on HEAD, returns whole body
    // on GET with `Transfer-Encoding: chunked`. The binary must fall back to
    // a single sequential worker and download exactly file_size bytes, *not*
    // cpu_count * file_size bytes.
    let mock = MockServer::start().await;
    let body = vec![0xABu8; 32 * 1024];

    Mock::given(method("HEAD"))
        .and(path("/file"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&mock)
        .await;

    // No Range matching here: the fallback path issues an un-ranged GET, and
    // even if it did issue a range request, the server is allowed to ignore
    // it. Return the whole body either way.
    Mock::given(method("GET"))
        .and(path("/file"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .mount(&mock)
        .await;

    let url = format!("{}/file", mock.uri());
    let (code, stdout, stderr) = run_binary(&url).await;

    assert_eq!(
        code, 0,
        "binary exited non-zero\nstdout: {stdout}\nstderr: {stderr}"
    );
    let total = parse_total_bytes(&stdout).expect("no summary line");
    assert_eq!(
        total,
        body.len() as u64,
        "expected exactly file_size bytes from the streaming fallback, got {total}"
    );
}

#[tokio::test]
async fn non_success_status_exits_non_zero() {
    // B2 regression: a 404 page used to be accepted as a successful download.
    let mock = MockServer::start().await;

    Mock::given(method("HEAD"))
        .and(path("/missing"))
        .respond_with(ResponseTemplate::new(404).set_body_string("not here"))
        .mount(&mock)
        .await;

    let url = format!("{}/missing", mock.uri());
    let (code, stdout, stderr) = run_binary(&url).await;

    assert_ne!(code, 0, "404 must fail\nstdout: {stdout}\nstderr: {stderr}");
    assert!(
        stderr.contains("404") || stdout.contains("404") || stderr.to_lowercase().contains("error"),
        "expected an error mentioning the HTTP status; stderr: {stderr}"
    );
}

#[tokio::test]
async fn accept_ranges_none_still_succeeds() {
    // B5: server advertises `Accept-Ranges: none` (or omits the header). The
    // binary must NOT fan out, must fall back to a single-worker download,
    // and must report exactly `file_size` bytes.
    let mock = MockServer::start().await;
    let body = vec![0u8; 8 * 1024];

    Mock::given(method("HEAD"))
        .and(path("/no-ranges"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Length", body.len().to_string().as_str())
                .insert_header("Accept-Ranges", "none"),
        )
        .mount(&mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/no-ranges"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .mount(&mock)
        .await;

    let url = format!("{}/no-ranges", mock.uri());
    let (code, stdout, stderr) = run_binary(&url).await;

    assert_eq!(
        code, 0,
        "binary exited non-zero\nstdout: {stdout}\nstderr: {stderr}"
    );
    let total = parse_total_bytes(&stdout).expect("no summary line");
    assert_eq!(
        total,
        body.len() as u64,
        "Accept-Ranges: none must fall back to a single worker (file_size bytes), got {total}"
    );
}

#[tokio::test]
async fn slow_server_completes() {
    let mock = MockServer::start().await;
    let body = vec![0x7Fu8; 4 * 1024];

    Mock::given(method("HEAD"))
        .and(path("/slow"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Length", body.len().to_string().as_str())
                .insert_header("Accept-Ranges", "bytes"),
        )
        .mount(&mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/slow"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(body.clone())
                .set_delay(Duration::from_millis(250)),
        )
        .mount(&mock)
        .await;

    let url = format!("{}/slow", mock.uri());
    let (code, stdout, stderr) = run_binary(&url).await;

    assert_eq!(
        code, 0,
        "binary exited non-zero\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        parse_total_bytes(&stdout).is_some(),
        "expected a summary line; stdout: {stdout}"
    );
}

/// Honors `Range: bytes=A-B?` requests against an in-memory body so the four
/// parallel workers each get the slice they asked for.
struct RangeResponder {
    body: Vec<u8>,
}

impl Respond for RangeResponder {
    fn respond(&self, req: &wiremock::Request) -> ResponseTemplate {
        let Some(range_hdr) = req.headers.get("range") else {
            return ResponseTemplate::new(200).set_body_bytes(self.body.clone());
        };
        let s = range_hdr.to_str().unwrap_or("");
        let Some(spec) = s.strip_prefix("bytes=") else {
            return ResponseTemplate::new(200).set_body_bytes(self.body.clone());
        };
        let (start_s, end_s) = spec.split_once('-').unwrap_or((spec, ""));
        let start: usize = start_s.parse().unwrap_or(0);
        let end: usize = if end_s.is_empty() {
            self.body.len().saturating_sub(1)
        } else {
            end_s
                .parse()
                .unwrap_or_else(|_| self.body.len().saturating_sub(1))
        };
        let end = end.min(self.body.len().saturating_sub(1));
        let slice = if start > end {
            Vec::new()
        } else {
            self.body[start..=end].to_vec()
        };
        ResponseTemplate::new(206)
            .insert_header(
                "Content-Range",
                format!("bytes {}-{}/{}", start, end, self.body.len()).as_str(),
            )
            .set_body_bytes(slice)
    }
}
