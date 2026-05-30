# Clipper

Encrypted clipboard and file sync.

The server stores ciphertext and sync metadata. Clients do the crypto locally.
The raw passphrase is not sent to the server.

Auth uses OPAQUE, an augmented PAKE. The server stores OPAQUE verifier material,
not a password hash usable as a login secret. Clipboard text, file metadata, and
file bytes are encrypted client-side with XChaCha20-Poly1305. The encryption key
is derived from the passphrase with Argon2id and a per-user salt.

Strong passphrases still matter. TLS still matters too; OPAQUE does not protect
bearer tokens or sync metadata from plain HTTP.

## Repo

`crates/server` is the Axum API and SQLite storage.
`crates/client` is the shared Rust sync client.
`crates/daemon` is the local macOS/Linux background process.
`app` is the Flutter app and Flutter Rust Bridge adapter.
`docs` has the longer notes.

## Platforms

The Flutter bridge is wired for macOS, Linux, Android, and web.

macOS runs the Flutter app against a local `clipper-daemon` over a Unix socket.
The daemon owns sync, clipboard watching, and Keychain-backed profile storage.

Linux also runs the Flutter app against `clipper-daemon` over a Unix socket. The
daemon is a per-user process, started through `systemd --user` when available
and falling back to a direct spawn. Wayland clipboard sync uses data-control via
`wl-clipboard-rs`; X11 clipboard support is intentionally not implemented.

Android runs the same Rust sync engine in-process behind the Flutter bridge.
The emulator default server URL is `http://10.0.2.2:8787`.

Android can keep the sync engine alive in a foreground service after the app has
been opened and the user is logged in. The service shows a persistent
notification with a Stop action and is for network sync only. Android does not
allow a normal background app or foreground service to continuously read the
system clipboard; clipboard reads require the app to have input focus unless it
is the default input method or a privileged system app. For Android clipboard
push, open Clipper and use **Add Current Clipboard** from the Clipboard tab.

The web client does not watch the clipboard in the background. It can display
synced clipboard history and can add the current text clipboard entry through
the Clipboard tab when the browser grants access.

```sh
cd app && flutter run -d android
```

macOS app packaging currently uses host Flutter and Xcode, while Rust artifacts
still use the Nix-pinned Rust toolchain through `CARGOKIT_CARGO` /
`CARGOKIT_RUSTC`. Use the wrapper so the Xcode environment is sanitized and the
daemon build uses the same pinned Rust toolchain:

```sh
nix run .#macos-build
```

By default the wrapper looks for Homebrew Flutter at `/opt/homebrew/bin/flutter`
or `/usr/local/bin/flutter`, builds `--debug`, and disables Flutter's Swift
Package Manager integration so the current CocoaPods-based macOS project is not
rewritten by newer host Flutter versions. Override with
`CLIPPER_HOST_FLUTTER=/path/to/flutter` and pass normal Flutter build flags
after `--`, for example `nix run .#macos-build -- --release`.

## Development

Use the flake shell:

```sh
direnv allow
```

One-off checks:

```sh
nix run .#fmt
nix run .#audit
cargo test --workspace
cd app && flutter analyze && flutter test
nix run .#wasm-check
```

After Rust bridge API changes:

```sh
nix run .#frb-generate
```

Web build:

```sh
nix run .#web-build
nix run .#web-serve
```

The web build must use the flake wrapper and be served with cross-origin
isolation headers because Flutter Rust Bridge starts a shared-memory Rust wasm
worker. `nix run .#web-serve` serves `app/build/web` with those headers; a
generic static file server such as `python -m http.server` will show a blank
page or startup error.

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
Android emulator loopback is `http://10.0.2.2:8787`.
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
auth_global_per_minute = 600
prune_interval_secs = 60

[auth]
challenge_ttl_secs = 300
max_pending_challenges = 4096

[limits]
max_file_blob_bytes = 536870912
max_file_meta_ciphertext_bytes = 65536
max_object_meta_ciphertext_bytes = 65536

[clipboard]
ttl_days = 7

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
