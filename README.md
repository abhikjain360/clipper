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
`crates/daemon` is the local macOS background process.
`app` is the Flutter app and Flutter Rust Bridge adapter.
`docs` has the longer notes.

## Platforms

The Flutter bridge is wired for macOS and Android.

macOS runs the Flutter app against a local `clipper-daemon` over a Unix socket.
The daemon owns sync, clipboard watching, and Keychain-backed profile storage.

Android runs the same Rust sync engine in-process behind the Flutter bridge.
The emulator default server URL is `http://10.0.2.2:8787`.

```sh
nix develop --command bash -c 'cd app && flutter run -d macos'
nix develop --command bash -c 'cd app && flutter run -d android'
```

## Development

Use the flake shell:

```sh
direnv allow
```

One-off checks:

```sh
nix develop --command bash -c 'cargo test --workspace'
nix develop --command bash -c 'cd app && flutter analyze && flutter test'
```

After Rust bridge API changes:

```sh
nix develop --command bash -c 'cd app && flutter_rust_bridge_codegen generate'
```

## Local Server

```sh
nix develop --command bash -c 'cargo run -p clipper-server -- init --data-dir .clipper-server'
nix develop --command bash -c 'cargo run -p clipper-server -- serve --data-dir .clipper-server'
```

The app defaults to `http://127.0.0.1:8787`.
Android emulator loopback is `http://10.0.2.2:8787`.

## Access

Access is invite-style. The operator creates a high-entropy access key, stores
only `base64(SHA-256(key))` in `access_keys`, and gives the raw key to the user
out of band. The key is only for first registration. It is not the passphrase
and it is not used for encryption.

```sh
ACCESS_KEY='replace-with-a-long-random-invite'
KEY_HASH=$(printf %s "$ACCESS_KEY" | openssl dgst -sha256 -binary | base64)
sqlite3 .clipper-server/clipper.db \
  "insert into access_keys (key_hash, created_at) values ('$KEY_HASH', '$(date -u +%Y-%m-%dT%H:%M:%SZ)');"
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
nix develop --command bash -c 'cargo build --release -p clipper-server'
target/release/clipper-server init --data-dir /var/lib/clipper
target/release/clipper-server serve --data-dir /var/lib/clipper --addr 127.0.0.1:8787
```

Put it behind a TLS reverse proxy for anything outside local development. Use
`--addr 0.0.0.0:8787` only when that is really what you want.
