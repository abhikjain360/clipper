# Clipper

Encrypted clipboard and file sync.

> **Status: early and experimental.** Clipper is pre-1.0, has not been security
> audited, and has known unfixed issues tracked in
> [`docs/rust-code-review.md`](docs/rust-code-review.md). Don't trust it with
> secrets you can't afford to lose yet. See [SECURITY.md](SECURITY.md).

> **Contributions:** upstream pull requests are closed for now. Clipper is
> experimental both as a product and as a development-process experiment: it is
> being used to explore how far high-quality software can be written with heavy
> agentic coding while keeping design judgment, review responsibility, and
> maintenance ownership with the maintainer. You are still welcome to read the
> code, run it locally, fork it, experiment, and send feedback. See
> [CONTRIBUTING.md](CONTRIBUTING.md).

The server stores ciphertext and sync metadata. Clients do the crypto locally.
The raw passphrase is not sent to the server.

Auth uses OPAQUE, an augmented PAKE. The server stores OPAQUE verifier material,
not a password hash usable as a login secret. Clipboard text, file metadata, and
file bytes are encrypted client-side with XChaCha20-Poly1305. The 256-bit
data-encryption key is derived from the OPAQUE login's `export_key` with
HKDF-SHA256; it is computed on the client and never sent to the server. OPAQUE
itself uses Argon2id to stretch the passphrase.

Strong passphrases still matter. TLS still matters too; OPAQUE does not protect
bearer tokens or sync metadata from plain HTTP.

## Repo

`crates/server` is the Axum API and SQLite storage.
`crates/client` is the shared Rust sync client.
`crates/web-wasm` is the wasm-bindgen adapter for the browser client.
`crates/mobile-uniffi` is the UniFFI adapter for React Native mobile.
`crates/daemon` is the local macOS/Linux background process.
`web` is the Vite/React client shared by the browser build and Tauri shell.
`web/src-tauri` is the native Tauri desktop shell.
`mobile` is the React Native/Expo Android client.
`packages/shared` contains frontend contracts shared by web, Tauri, and mobile.
`packages/mobile-bridge` contains the React Native UniFFI package glue.
`docs` has the longer notes.

## Platforms

The browser client is a Vite/React app that uses the shared Rust sync client
through `crates/web-wasm`.

The native desktop client is a Tauri shell around the same React UI. Its Rust
backend runs `clipper-client` in-process, stores client data under the app data
directory, and uses Tauri plugins for native file dialogs and clipboard access.
On macOS and Linux, `clipper-client` starts its platform clipboard watcher after
login.

The mobile client is React Native/Expo for Android. It has native RN components
under `mobile/src`, shares frontend contracts through `packages/shared`, and
talks to `clipper-client` through `crates/mobile-uniffi` plus
`packages/mobile-bridge`. iOS is not wired into the app yet, but the bridge
layout keeps it mostly additive.

The web client does not watch the clipboard in the background. It can display
synced clipboard history and can add the current text clipboard entry through
the Clipboard tab when the browser grants access.

```sh
nix run .#tauri-dev
nix run .#tauri-build
```

## Development

Use the flake shell:

```sh
direnv allow
```

One-off checks:

```sh
nix run .#fmt
nix run .#audit
nix run .#udeps
cargo test --workspace
nix run .#wasm-check
nix run .#web-check
nix run .#mobile-check
cargo check -p clipper-desktop
```

Web and native builds:

```sh
nix run .#web-build
nix run .#web-serve
nix run .#tauri-dev
nix run .#tauri-build
nix run .#mobile-start
nix run .#mobile-uniffi-android
nix run .#mobile-android
```

`nix run .#web-build` builds `crates/web-wasm` with `wasm-pack`, installs the
pnpm workspace from `pnpm-lock.yaml`, and emits the Vite production build under
`web/dist`. `nix run .#web-serve` regenerates the wasm package and starts the
Vite dev server. `nix run .#tauri-dev` and `nix run .#tauri-build` install the
same workspace and invoke Tauri against `web/src-tauri`.

The browser client receives live sync events over a ticketed WebSocket path:
it mints a short-lived ticket over authenticated HTTP and then connects to the
public WebSocket upgrade route with that ticket as a WebSocket subprotocol.
Native clients continue to use the bearer-authenticated `/api/ws` route.

## Local Server

```sh
mkdir -p data
test -f data/clipper-server.secret || \
  cargo run -p clipper-server -- generate-secret > data/clipper-server.secret
chmod 600 data/clipper-server.secret

export CLIPPER_SERVER_SECRET_FILE="$PWD/data/clipper-server.secret"
cargo run -p clipper-server -- init --data-dir data/clipper-server
cargo run -p clipper-server -- serve --data-dir data/clipper-server
```

The app defaults to `http://127.0.0.1:8787`.
Keep the same server secret for `init`, `add-access-key`, and `serve`; the
database cannot be opened with a different secret.

## Access

Access is invite-style. The operator creates a high-entropy access key, stores
only an Argon2id verifier in `access_keys`, and gives the raw key to the user
out of band. The key is only for first registration. It is not the passphrase
and it is not used for encryption. Add keys after `init`, because the server
generates the access-key hashing salt during initialization.
Without `--access-key`, the `add-access-key` command prompts for the raw key;
keep the generated key and give it to the user out of band.

```sh
export CLIPPER_SERVER_SECRET_FILE="$PWD/data/clipper-server.secret"
ACCESS_KEY="$(openssl rand -base64 32)"
echo "Access key: $ACCESS_KEY"
cargo run -p clipper-server -- add-access-key \
  --data-dir data/clipper-server \
  --access-key "$ACCESS_KEY"
```

There is no admin UI here yet.

Users register from the app's Register mode with the server URL, access key,
and their chosen passphrase. Returning users use Login mode with the same
passphrase; the app stores the last user ID in the local profile where the
platform supports profile storage.

## Deploy

Server deploy is the Rust binary plus a durable data directory. There are no
service files in this repo yet.

```sh
cargo build --release -p clipper-server
test -f /path/outside/db-backups/clipper.secret || \
  target/release/clipper-server generate-secret > /path/outside/db-backups/clipper.secret
chmod 600 /path/outside/db-backups/clipper.secret
export CLIPPER_SERVER_SECRET_FILE=/path/outside/db-backups/clipper.secret
target/release/clipper-server init --data-dir /var/lib/clipper
target/release/clipper-server serve --data-dir /var/lib/clipper --addr 127.0.0.1:8787
```

`CLIPPER_SERVER_SECRET` can hold the base64 secret directly, but
`CLIPPER_SERVER_SECRET_FILE` is usually safer for services. Set exactly one of
those variables before `init`, `add-access-key`, or `serve`.

Server configuration can also live in a TOML file. Built-in defaults are used
when no config file is supplied; config file values override those defaults,
`CLIPPER_TRUSTED_PROXIES` can override `server.trusted_proxies`, and CLI flags
override the config file.

```toml
[server]
data_dir = "/var/lib/clipper"
addr = "127.0.0.1:8787"
trusted_proxies = ["127.0.0.1", "::1"]

[rate_limit]
auth_per_client_per_minute = 10
auth_per_username_per_minute = 30
auth_global_per_minute = 3000
api_per_client_per_minute = 2400
api_per_user_per_minute = 1200
ws_tickets_per_user_per_minute = 30
prune_interval_secs = 60

[auth]
challenge_ttl_secs = 300
max_pending_challenges = 4096
max_pending_ws_tickets = 4096

[limits]
max_file_blob_bytes = 536870912
max_file_meta_ciphertext_bytes = 65536
max_object_meta_ciphertext_bytes = 65536
max_user_storage_bytes = 10737418240
max_user_objects = 10000
max_user_devices = 32
max_user_ws_connections = 32

[clipboard]
ttl_days = 7
max_items = 100

[crypto]
access_key_hash_m_cost = 19456
access_key_hash_t_cost = 2
access_key_hash_p_cost = 1
access_key_hash_salt_bytes = 16
session_token_bytes = 32

[list]
default_limit = 100
max_limit = 500

[cleanup]
interval_secs = 3600
event_log_retention_days = 3
orphan_upload_ttl_secs = 3600
created_at_future_skew_secs = 300
```

```sh
export CLIPPER_SERVER_SECRET_FILE=/path/outside/db-backups/clipper.secret
target/release/clipper-server --config /etc/clipper/server.toml init
target/release/clipper-server --config /etc/clipper/server.toml serve
```

Put it behind a TLS reverse proxy for anything outside local development. Use
`--addr 0.0.0.0:8787` only when that is really what you want.

If a reverse proxy connects to `clipper-server`, configure that proxy as trusted
so auth rate limiting uses the real client IP from `X-Forwarded-For`,
`X-Real-IP`, or `Forwarded`. This is startup configuration and requires a server
restart when changed.

```sh
CLIPPER_SERVER_SECRET_FILE=/path/outside/db-backups/clipper.secret \
CLIPPER_TRUSTED_PROXIES=127.0.0.1,::1 \
  target/release/clipper-server serve --data-dir /var/lib/clipper --addr 127.0.0.1:8787
```

The equivalent CLI form is `--trusted-proxy 127.0.0.1 --trusted-proxy ::1`.
Do not trust forwarded headers from arbitrary peers; clients can spoof them.

## License

Clipper is licensed under the GNU Affero General Public License, Version 3
(AGPL-3.0), **with an additional linking/combination permission**: using,
linking against, or embedding Clipper in another project does not place that
other project under the AGPL. Only modifications to Clipper's own source code
carry the AGPL's copyleft — including its network-use (SaaS) disclosure
requirement — and must be released in source form. See [LICENSE](LICENSE) for
the exact terms.
