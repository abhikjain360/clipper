# Security Policy

## Status

Clipper is **early, experimental, and pre-1.0**. It has **not** had an external
security audit, and there are known, unfixed security and correctness issues
tracked in [`docs/issues.md`](docs/issues.md). Until that list is cleared and
the project reaches a tagged release, **do not rely on Clipper to protect
secrets you cannot afford to lose.**

## Threat model

- The server is not trusted with plaintext. It is storage and coordination: it
  holds ciphertext and sync metadata.
- Clients derive keys, encrypt, decrypt, sign and verify locally. This work, the
  local cache and the sync state live in the shared Rust client. The browser,
  Tauri and React Native frontends are thin adapters over it.
- A relevant attacker may hold a server database dump plus the on-disk payload
  files, be a malicious authenticated client, be a malicious or buggy server or
  relay that tampers with ciphertext, or be another OS user on the same machine.
- Processes running as the same OS user are trusted. The daemon accepts only
  same-user peers, but it cannot tell Clipper's own UI from other same-user
  software (see [`docs/issues.md`](docs/issues.md), entry 53).

The design relies on two rules in the frontends. A break of either one is a
vulnerability:

- No Tauri command returns the bearer token, the keys or the IPC secret to the
  webview.
- Decrypted text reaches the DOM only as text nodes.

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
- Before reporting, please skim [`docs/issues.md`](docs/issues.md): many issues
  are already known and tracked there. Confirming that a tracked issue is
  exploitable in practice is still useful, but a brand-new finding is the most
  valuable.

Please include enough detail to reproduce: the affected component
(`crates/server`, `crates/client`, `crates/daemon`, `web`, `mobile`), the
version/commit, and a proof of concept if you have one.

### What to expect

As a small pre-release project, response is best-effort. A rough target:

- Acknowledgement within 7 days.
- Initial assessment (severity, whether it is already tracked) within 14 days.
- Fixes for confirmed issues land on `main`; an advisory is published once a fix
  is available.

## Scope notes

Some residual risks are intentional tradeoffs for now and are recorded in
[`docs/issues.md`](docs/issues.md): the same-user trust of local IPC (entry 53),
the web client's dependence on the host that serves it (entry 60), collab docs
being readable by the server and open to anyone with the link (entry 63), and
the limits of rollback and provenance checks (entry 64). Reporting that these
_documented_ tradeoffs exist is not a vulnerability — but reporting that one is
materially worse than documented is welcome.
