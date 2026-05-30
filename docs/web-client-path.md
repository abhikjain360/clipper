# Web Client Path

This note records the current shape and remaining design constraints for the
Flutter Web client, which uses the shared Rust client code compiled to
WebAssembly.

The goal is not native parity. The web client should reuse the same auth,
encryption, object encoding, and app-state behavior where that is valuable, but
browser-only I/O should stay behind explicit adapters.

## Current Shape

The codebase currently has one shared Rust client crate:

- `crates/client` owns `ApiClient`, `SyncEngine`, encryption helpers, and local
  client state.
- `app/rust` exposes Flutter Rust Bridge functions to Dart.
- macOS and Linux use `app/rust` -> daemon IPC -> `clipper-client::SyncEngine`.
- Android and web use `app/rust` -> in-process `clipper-client::SyncEngine`.
- `crates/api-types` owns the server/client payload types.
- `crates/app-types` owns decrypted display state exposed to the UI.
- `crates/daemon-types` owns app/daemon commands and events.
- Web builds use the same bridge surface with byte-oriented file
  upload/download helpers and browser `localStorage` for the current local
  clipboard cache.

The broad architecture is: keep one shared Rust client logic surface and place
platform differences around it.

## Web Product Scope

The web client does not need realtime clipboard support or background clipboard
watching.

Current web behavior:

- login and register;
- derive the same client-side encryption key in WASM;
- list synced clipboard items and file metadata;
- explicitly refresh after user action;
- copy selected text through browser clipboard APIs from Dart;
- upload selected files through browser file picker bytes;
- download selected files through browser download APIs;
- avoid background clipboard watching.

Browser limitations are product constraints, not implementation bugs:

- arbitrary background clipboard watching is not available;
- clipboard writes generally require a user gesture;
- files are selected or downloaded through browser APIs, not paths;
- browser WebSockets cannot set arbitrary `Authorization` headers;
- durable local storage is browser storage, not a filesystem path.

## Web Hosting Requirement

The Flutter web build must be served from a cross-origin isolated page. Flutter
Rust Bridge starts a Rust wasm worker and shares wasm memory with that worker;
browsers require both a shared-memory wasm build and these response headers for
that path:

- `Cross-Origin-Opener-Policy: same-origin`
- `Cross-Origin-Embedder-Policy: require-corp`

For local development, use `nix run .#web-build` and then
`nix run .#web-serve`. The build wrapper passes the shared-memory wasm linker
flags, and the serve wrapper sends the isolation headers. Generic static file
servers, including `python -m http.server`, do not send these headers and will
prevent the Rust wasm worker from starting.

## Preferred Architecture

Do not create a separate web client crate that duplicates auth, crypto, and sync
logic. Keep `clipper-client` as the shared client crate, but split native I/O
away from client behavior.

Target layering:

```text
Flutter UI
  app/rust Flutter Rust Bridge API
    platform runtime adapter
      native runtime
        reqwest HTTP
        filesystem local store
        native WebSocket
      web runtime
        reqwest/browser fetch HTTP
        browser localStorage store
        no realtime transport initially
    shared client logic
      auth flow
      OPAQUE client messages
      key derivation
      object encryption/decryption
      postcard encode/decode
      app-state reducer
```

The important boundary is: Rust client logic should depend on small transport
and storage traits, not directly on `reqwest`, `tokio::fs`, or
`tokio-tungstenite` everywhere.

## Transport Boundary

`ApiClient` currently owns a `reqwest::Client`. That works for native and web
today, but an explicit transport boundary may still be useful if web size,
browser behavior, or testability becomes painful.

Introduce a small HTTP transport shape:

```text
request:
  method
  path or absolute URL
  headers
  body bytes

response:
  status
  headers
  body bytes
```

Native implementation:

- uses `reqwest`;
- keeps existing bearer-token header behavior;
- handles JSON and postcard bodies as today.

Web implementation:

- currently relies on the browser-backed WASM path used by `reqwest`;
- lets browser fetch own CORS, credentials mode, and TLS;
- returns raw status and body bytes to Rust;
- keeps postcard serialization/deserialization in Rust.

Postcard is a good fit for this boundary because the Rust client can construct
and parse binary request/response bodies without involving Dart-specific model
code.

## Storage Boundary

`LocalStore` has platform-specific implementations. Native builds use the
filesystem; web builds use browser `localStorage` with a bounded clipboard
index. The network boundary remains encrypted, while the local cache stores
decrypted clipboard payloads for UI convenience.

Define a client store boundary with only the operations the sync engine needs:

- persist decrypted clipboard item;
- list recent decrypted clipboard items;
- fetch clipboard text by id;
- persist/list decrypted file metadata;
- optionally persist/fetch downloaded file bytes.

Native implementation:

- keep using the filesystem-backed `LocalStore`;
- preserve the current local plaintext cache model.

Web implementation:

- currently uses `localStorage`;
- should move to IndexedDB if durable web history needs larger binary payloads;
- must not persist the passphrase or derived encryption key;
- should keep decrypted clipboard persistence an explicit product decision.

The storage decision should not leak into crypto or server API code.

## Runtime Boundary

`app/rust/src/runtime.rs` has a web branch that mirrors Android more than
macOS/Linux:

- one in-process engine;
- no daemon process;
- no Unix socket transport;
- web-specific default server URL handling;
- no platform clipboard watcher.

The Flutter Rust Bridge surface stays shared for login, register, refresh,
copy, and state. File APIs include web-specific byte-oriented entry points
because browser files are not local paths.

Current bridge split:

- keep native `upload_file(file_path)` and `download_file(file_id, target_path)`;
- add web-friendly `upload_file_bytes(filename, mime_type, bytes)`;
- add web-friendly `download_file_bytes(file_id) -> bytes`;
- let Dart handle browser file picker and browser download.

## Realtime Sync

Do not make WebSocket support a requirement for the first web client.

Initial web sync can be:

- refresh after login/register;
- explicit refresh button;
- refresh after upload/delete;
- optional light polling while the tab is active.

If web realtime is added later, avoid reusing the native `tokio-tungstenite`
path. Browser realtime should use browser `WebSocket`, and server auth should be
adapted accordingly.

Because browsers cannot attach arbitrary `Authorization` headers to WebSocket
handshakes, one of these server-side auth shapes would be needed:

- cookie-backed browser sessions;
- short-lived WebSocket ticket acquired over authenticated HTTP;
- token in query string with strict short lifetime and logging hygiene;
- subprotocol-based token, if accepted by the client/server stack.

The short-lived ticket is the cleanest fit if the server stays bearer-token
oriented for HTTP.

## Server API Direction

The web client consumes the same object API as native. Avoid adding any
browser-only legacy clipboard/file endpoint path. The sustainable endpoint
contract is:

- shared request/response structs in `crates/api-types`;
- postcard for Rust-only binary object endpoints;
- raw bytes for object payload uploads/downloads;
- JSON only where browser/native compatibility or human debugging is more
  valuable than binary efficiency.

## Build Integration

Flutter Rust Bridge has generated web glue in `app/lib/src/rust`, and the Rust
crate enables FRB's `wasm-start` feature. Use the flake wrappers:

- `nix run .#frb-build-web` builds the Rust bridge WASM package.
- `nix run .#web-build` builds the bridge package and Flutter web app.
- `nix run .#web-serve` serves `app/build/web` with the required
  cross-origin-isolation headers.

The WASM build should be treated as another platform target, not as a separate
application architecture.

## Dependency Cleanup Needed

Remaining cleanup:

- keep `tokio-tungstenite` behind native realtime transport;
- consider a smaller explicit HTTP transport boundary if `reqwest` becomes too
  heavy for web;
- keep filesystem-only behavior out of WASM paths;
- move web local storage to IndexedDB if `localStorage` size limits become a
  product issue.

These are separable cleanups. They should be done as boundary extractions, not
as a web fork.

## Remaining Order

1. Keep manual refresh as the baseline web sync model.
2. Add IndexedDB only if `localStorage` is too small or too awkward.
3. Add browser WebSocket only after the basic web client remains useful without
   realtime transport.
4. If browser WebSocket is added, use browser-native WebSocket plus a dedicated
   server auth shape; do not reuse the native `tokio-tungstenite` path.

## Non-Goals

- Do not create per-platform client crates for shared auth and crypto behavior.
- Do not duplicate server payload types in Dart for endpoints Rust already owns.
- Do not require browser background clipboard watching.
- Do not require browser WebSocket support for the first web client.
- Do not make native platforms depend on browser-specific transport code.

## Decision Summary

The most sustainable path is one shared Rust client logic layer with explicit
transport, storage, runtime, and file I/O adapters. Web should be another
platform adapter around the shared client, not a separate client implementation
and not a forced reuse of native filesystem/WebSocket assumptions.
