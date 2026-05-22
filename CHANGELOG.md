# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Maintenance and dependency updates; see git log for details.

### Decisions

- **TLS implementation: continues to use OpenSSL (vendored).** The post-audit
  review evaluated migrating to `rustls` to remove the OpenSSL build-time
  dependency. That migration was intentionally deferred: vendoring OpenSSL
  keeps the released binary completely free of system runtime dependencies
  (no `libssl.so.X` requirement), which is the project's explicit deployment
  contract. `cargo audit` and Dependabot remain the controls for the
  vendored copy. Re-evaluate if OpenSSL becomes harder to cross-build or if
  a critical advisory ships without a same-day OpenSSL patch.

## Released

This project has been published as a series of GitHub releases. See the
[Releases page](https://github.com/richardsondev/httpbandwidthspeedtester/releases)
for the binaries and the auto-generated release notes for each tag.

Notable historical changes:

- Migrate to hyper 1.x API.
- Add `.deb` package generation for Linux releases.
- Various `openssl` security updates via Dependabot.
- Response parsing improvements.

[Unreleased]: https://github.com/richardsondev/httpbandwidthspeedtester/compare/HEAD
