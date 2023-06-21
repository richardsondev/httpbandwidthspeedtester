# HTTP Bandwidth Speed Tester

This application is a very simple bandwidth testing tool written in Rust. It is designed to test the bandwidth between your local computer and a remote server over HTTP or HTTPS. The application uses HTTP range requests to download different parts of a file concurrently across multiple threads. By doing so, it can typically fully utilize the network bandwidth and provide a measure of the maximum download speed achievable.

## Build Requirements

To run this application, you will need:

- Rust 1.41.0 or later
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

## Creating Your Own Test File

If you want to create your own test file on a remote server, you can do so using the following bash command:

```bash
dd if=/dev/zero of=testfile.bin bs=1M count=10240
```

This command will create a 10GB file named `testfile.bin` filled with zeroes. After running this command, make sure that the file is accessible via HTTP or HTTPS by placing it in the appropriate directory of your web server. You can then use the URL of this file as the argument to the bandwidth testing tool.

## Note

Not all servers support HTTP range requests. If the server doesn't support them, the download may fail or not be as fast as it could be. Always make sure that the server is capable of handling range requests and multiple connections before running this test.
