# HTTP Bandwidth Speed Tester

This application is a very simple bandwidth testing tool written in Rust. It is designed to test the bandwidth between your local computer and a remote server over HTTP or HTTPS. The application uses HTTP range requests to download different parts of a file concurrently across multiple threads. By doing so, it can typically fully utilize the network bandwidth and provide a measure of the maximum download speed achievable.

## Build Requirements

To run this application, you will need:

- Rust 1.74.0 or later (as declared by `rust-version` in `Cargo.toml`)
- Cargo (usually comes with Rust)

## How to Use

### Pre-built binaries
See the release page on GitHub for Windows, Linux, and macOS binaries.

### Self-build
To run the application, first build it with Cargo:

```bash
cargo build --release
```

You can then run the application with a URL as the argument:

```bash
cargo run --release "http://yourserver.example.com/testfile.bin"
```

The application will print out the average download speed over the last 10 seconds every second. It measures the bandwidth by downloading a file and tracking the amount of data received over time. The application will exit when all parts of the file have been downloaded.

### HTTPS

The same command form works for HTTPS URLs — the binary is built against
`hyper-tls` so it uses the system OpenSSL stack and the OS trust store.
On minimal container images, install `ca-certificates` (or copy in the CA
bundle) so TLS verification succeeds:

```bash
cargo run --release "https://yourserver.example.com/testfile.bin"
```

## Creating Your Own Test File

If you want to create your own test file on a remote server, you can do so using the following bash command:

```bash
dd if=/dev/zero of=testfile.bin bs=1M count=10240
```

This command will create a 10GB file named `testfile.bin` filled with zeroes. After running this command, make sure that the file is accessible via HTTP or HTTPS by placing it in the appropriate directory of your web server. You can then use the URL of this file as the argument to the bandwidth testing tool.

## Range request support is required

This tool deliberately uses HTTP byte-range requests so that it can fan a
single download out across as many TCP connections as the host has CPU cores
— that is what lets it saturate links that a single connection cannot. As a
direct consequence the server **must** support range requests:

- It must advertise `Accept-Ranges: bytes` on the initial probe.
- It must return `206 Partial Content` for ranged `GET` requests rather than
  silently returning the whole body with `200 OK`.

A server that ignores range requests will either fail outright or, worse,
each worker will download the full file and the reported bandwidth will be a
multiple of the real value. Static origins like nginx, Apache, S3, GCS, and
most CDNs satisfy these requirements out of the box; dynamically generated
endpoints often do not.

If the probe shows the server does not support ranges, the tool will fall
back to a single-worker streaming download (see also the *No Content-Length*
note below). In that mode it still produces a correct byte count and speed
reading, but it cannot use more than one connection.

## Note

Not all servers support HTTP range requests (see the section above for the
full requirement). If the server doesn't support them, the download may fall
back to a single connection or fail entirely. Always make sure that the
server is capable of handling range requests and multiple connections before
running this test.

## No Content-Length

If the server's response to the initial probe does not carry a
`Content-Length` header (for example, dynamically generated `chunked`
responses), the tool cannot pre-compute byte ranges. In that case it falls
back to a single sequential worker that streams the body to EOF. The
reported byte count and speed are still correct; only the parallelism is
disabled.
