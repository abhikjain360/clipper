# Security Policy

## Status

Clipper is **early, experimental, and pre-1.0**. It has **not** had an external
security audit, and there are known, unfixed security and correctness issues
tracked in [`docs/rust-code-review.md`](docs/rust-code-review.md). Until that
list is cleared and the project reaches a tagged release, **do not rely on
Clipper to protect secrets you cannot afford to lose.**

## Threat model

The intended model (see [`docs/backend-review-flow.md`](docs/backend-review-flow.md)
and [`docs/rust-code-review.md`](docs/rust-code-review.md) for the full version):

- Every client is untrusted; clients encrypt content locally before upload.
- The server is honest-but-curious storage and coordination — it holds
  ciphertext and sync metadata and is not trusted with plaintext.
- A relevant attacker may hold a database dump plus on-disk blobs, be a
  malicious authenticated client, be a malicious or buggy server/relay tampering
  with ciphertext, or (for daemon IPC) be same-user local software.

Transport security (TLS) is assumed to be terminated by a reverse proxy in front
of the server for any non-loopback deployment; OPAQUE does not protect bearer
tokens or sync metadata over plain HTTP.

## Supported versions

There are no released versions yet. Only the current `main` branch is supported,
and fixes land on `main` without backports.

| Version         | Supported |
| --------------- | --------- |
| `main`          | ✅        |
| tagged releases | none yet  |

## Reporting a vulnerability

Please report security issues **privately** — not in public issues or pull
requests.

- Preferred: use GitHub's private vulnerability reporting. Open the repository's
  **Security** tab and choose **"Report a vulnerability"**.
  (Maintainers: enable this under _Settings → Code security and analysis →
  Private vulnerability reporting_.)
- Before reporting, please skim [`docs/rust-code-review.md`](docs/rust-code-review.md):
  many issues are already known and tracked there. Confirming that a tracked
  issue is exploitable in practice is still useful, but a brand-new finding is
  the most valuable.

Please include enough detail to reproduce: the affected component
(`crates/server`, `crates/client`, `crates/daemon`, `app`), the version/commit,
and a proof of concept if you have one.

### What to expect

As a small pre-release project, response is best-effort. A rough target:

- Acknowledgement within 7 days.
- Initial assessment (severity, whether it is already tracked) within 14 days.
- Fixes for confirmed issues land on `main`; an advisory is published once a fix
  is available.

## Scope notes

Some residual risks are intentional tradeoffs for now and are listed in the
"Accepted / Intentional Tradeoffs" section of
[`docs/rust-code-review.md`](docs/rust-code-review.md) (for example, the
same-user local IPC trust boundary and the plaintext local clipboard cache).
Reporting that these _documented_ tradeoffs exist is not a vulnerability — but
reporting that one is materially worse than documented is welcome.
