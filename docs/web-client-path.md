# Web Client Path

This note records the preferred direction for adding a Flutter Web client that
uses the shared Rust client code compiled to WebAssembly.

The goal is not native parity. The web client should reuse the same auth,
encryption, object encoding, and app-state behavior where that is valuable, but
browser-only I/O should stay behind explicit adapters.

## Current Shape

The codebase currently has one shared Rust client crate:

- `crates/client` owns `ApiClient`, `SyncEngine`, encryption helpers, and local
  client state.
- `app/rust` exposes Flutter Rust Bridge functions to Dart.
- macOS uses `app/rust` -> daemon IPC -> `clipper-client::SyncEngine`.
- Android uses `app/rust` -> in-process `clipper-client::SyncEngine`.
- `crates/api-types` owns the server/client payload types.
- `crates/app-types` owns decrypted display state exposed to the UI.
- `crates/daemon-types` owns app/daemon commands and events.

This is the right broad architecture for web: keep one shared Rust client logic
surface and place platform differences around it.

## Web Product Scope

The web client does not need realtime clipboard support.

Target web behavior:

- login and register;
- derive the same client-side encryption key in WASM;
- list synced clipboard items and file metadata;
- explicitly refresh or poll on a conservative interval;
- copy selected text through browser clipboard APIs from Dart;
- upload selected files through browser file picker bytes;
- download selected files through browser download APIs;
- avoid background clipboard watching.

Browser limitations are product constraints, not implementation bugs:

- arbitrary background clipboard watching is not available;
- clipboard writes generally require a user gesture;
- files are selected or downloaded through browser APIs, not paths;
- browser WebSockets cannot set arbitrary `Authorization` headers;
- durable local storage is IndexedDB/localStorage, not a filesystem path.

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
        browser fetch transport
        web local store or memory store
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

`ApiClient` currently owns a `reqwest::Client`. That is fine for native, but it
should become an implementation detail of a native transport adapter.

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

- calls a JS/browser fetch function from WASM, or uses a thin web-sys wrapper;
- lets browser fetch own CORS, credentials mode, and TLS;
- returns raw status and body bytes to Rust;
- keeps postcard serialization/deserialization in Rust.

Postcard is a good fit for this boundary because the Rust client can construct
and parse binary request/response bodies without involving Dart-specific model
code.

## Storage Boundary

The current `LocalStore` is filesystem based. That should remain the native
implementation, not the universal client storage model.

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

- start with an in-memory store if that is enough for the first web client;
- move to IndexedDB once offline reload or durable web history matters;
- avoid persisting the passphrase or derived encryption key;
- consider not persisting decrypted clipboard history by default.

The storage decision should not leak into crypto or server API code.

## Runtime Boundary

`app/rust/src/runtime.rs` should gain a web branch that mirrors Android more
than macOS:

- one in-process engine;
- no daemon process;
- no Unix socket transport;
- web-specific default server URL handling;
- no platform clipboard watcher.

The existing Flutter Rust Bridge surface can remain mostly stable for login,
register, refresh, copy, and state. File APIs need web-specific byte-oriented
entry points because browser files are not local paths.

Recommended bridge split:

- keep native `upload_file(file_path)` and `download_file(file_id, target_path)`;
- add web-friendly `upload_file_bytes(filename, mime_type, bytes)`;
- add web-friendly `download_file_bytes(file_id) -> bytes + metadata`;
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

Finish the postcard/object endpoint migration before treating web as a serious
target.

Avoid making the web client support both the legacy clipboard/file endpoint
model and the new object model as first-class paths. The sustainable endpoint
contract should be:

- shared request/response structs in `crates/api-types`;
- postcard for Rust-only binary object endpoints;
- raw bytes for object payload uploads/downloads;
- JSON only where browser/native compatibility or human debugging is more
  valuable than binary efficiency.

Once the object model is stable, the web client can consume the same object API
as native with only the HTTP transport swapped.

## Build Integration

Flutter Rust Bridge already has generated web glue in `app/lib/src/rust`, and
the Rust crate enables FRB's `wasm-start` feature. The app itself is not yet
configured as a Flutter Web app.

Expected build direction:

- add Flutter web platform files using Flutter's project generator;
- build the Rust bridge with `flutter_rust_bridge_codegen build-web`;
- keep generated WASM artifacts under the expected `pkg/` path for FRB;
- configure Rust WASM entropy support for browser crypto randomness;
- keep native cargokit build flow intact for Android/macOS.

The WASM build should be treated as another platform target, not as a separate
application architecture.

## Dependency Cleanup Needed

The shared client currently pulls in native assumptions that should be isolated:

- `tokio::fs` belongs behind native local storage;
- `tokio-tungstenite` belongs behind native realtime transport;
- `reqwest` belongs behind native HTTP transport;
- random number generation needs browser-compatible WASM configuration;
- file path APIs need byte-oriented web alternatives.

These are separable cleanups. They should be done as boundary extractions, not
as a web fork.

## Recommended Order

1. Stabilize the object/postcard server contract.
2. Move `ApiClient` onto an HTTP transport abstraction.
3. Move local cache operations behind a store abstraction.
4. Add byte-oriented file operations to the shared engine.
5. Add a web runtime branch in `app/rust`.
6. Add Flutter Web platform files and FRB WASM build integration.
7. Start web with manual refresh and no realtime transport.
8. Add IndexedDB or browser WebSocket only after the basic web client is useful.

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
