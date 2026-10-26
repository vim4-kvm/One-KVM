# SRTP authentication backend

This directory contains the crates.io `webrtc-srtp` 0.17.2 release, from upstream
revision `1da3fe8ee84db8d266322cbcfcb357c745959a8e` (`srtp/`). Original authors,
MIT/Apache-2.0 licenses, protocol implementation and tests are preserved.

The `openssl-hmac` feature selects precomputed OpenSSL SHA-1 inner/outer states
for AES-CM's HMAC authentication. It avoids per-packet EVP allocation and uses
the existing OpenSSL dependency. Key derivation, SHA-1 HMAC, tag truncation,
constant-time verification, ROC, replay protection and profile negotiation are
unchanged. Without this feature, upstream RustCrypto HMAC remains in use,
including builds that enable OpenSSL only for AES.

One-KVM enables this feature only for its `vim4` build. The shipped vendored
OpenSSL exposes the low-level SHA-1 interface used here; OpenSSL builds with
`OPENSSL_NO_DEPRECATED_3_0` do not support this optional backend. Keep this feature
disabled for those builds. These APIs require review when updating OpenSSL.

Run the complete library tests with `--features vendored-openssl,openssl-hmac`,
then with `--features vendored-openssl` and `--no-default-features` separately.
The additional tests compare RFC2202 vectors and full RTP/SRTCP HMAC tags against
RustCrypto across key/block/ROC boundaries and repeated use. The application's
ordinary `cargo test --lib` does not run a dependency's own tests.

Use `test/rtp-bench` for production-track replay. Its default features enable
this backend; `--no-default-features --features vim4` retains the same VIM4 track
behavior with upstream HMAC for a controlled comparison. Preserve both binaries
and compare encrypted transcript hashes using identical fixtures and counts.
