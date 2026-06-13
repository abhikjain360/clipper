# Server Visible MCP

> Status: **PROPOSAL / NOT IMPLEMENTED.** This is a forward-looking design for a
> future opt-in "server-visible" mode and an MCP surface over it. As of this
> writing **none of it ships**: there is no `visibility` column, no
> server-readable storage form, no search index, no MCP server, and no MCP
> tools or scopes in the codebase. Everything below describes intended future
> behavior unless it appears under "Current behavior". Do not read the
> invariants in this document as guarantees the running server currently
> enforces — the relevant guarantee that *does* hold today is described under
> "Current behavior": all synced objects are end-to-end encrypted and opaque to
> the server.

Goal: let MCP/ChatGPT search and read only things the user explicitly marks
visible.

Default stays private.

## Current behavior (what the server actually does today)

There is **one** storage mode, and it is end-to-end encrypted. The server never
receives or stores plaintext clipboard or file content.

- Every synced item is an "object" (`ObjectKind::Clipboard` or
  `ObjectKind::File`) stored in the `objects` / `object_payloads` tables. The
  `objects` table has no `visibility` column; the only mode-like field is
  `status` (`pending` / `complete`).
- Object metadata is uploaded as `meta_ciphertext` + `meta_nonce` and stored
  verbatim. The server cannot decrypt it. The plaintext (`ClipboardMeta` /
  `FileMeta`, e.g. `mime_type`, `filename`, `size`) is sealed client-side with
  XChaCha20-Poly1305 before upload.
- Object payload bytes (clipboard text, file body) are uploaded as ciphertext
  and stored as opaque files on disk
  (`object_payloads.ciphertext_path`). The server records only ciphertext
  metadata: `nonce`, `ciphertext_size`, and `sha256_ciphertext`.
- `init_object`, `upload_payload`, `complete_object`, `list_objects`,
  `get_object`, and `download_payload` all move ciphertext only. There is no
  endpoint that accepts or returns a server-readable plaintext form of an
  object, and no code path that decrypts object content server-side. See
  `crates/server/src/routes/objects.rs`.

What the server *can* see today (unavoidable metadata, not content):

- `kind` (clipboard vs file), `created_at`, `expires_at`, `status`,
  `created_seq` (the sync cursor), and `source_device_id`.
- The signed provenance `envelope` (`ObjectEnvelopeV1`) and the source device's
  Ed25519 signing public key, used to verify the upload came from that device.
- Ciphertext sizes and SHA-256 of ciphertext.

What the server cannot see today: decrypted clipboard text, file contents,
filenames, MIME types, or any other object metadata — all of that lives inside
`meta_ciphertext` / payload ciphertext.

The full HTTP surface today is auth (`/api/auth/*`, `/api/auth/logout`),
WebSocket sync (`/api/ws`, `/api/ws-ticket`, `/api/ws-ticket/connect`), object
endpoints under `/api/objects`, and `/api/health`. There is **no** MCP route,
no search route, and no "server-visible" route. (`crates/server/src/main.rs`.)

> Note: the `LocalVisibleState` / `visible_state` names in the client crate
> (`crates/client`) refer to *locally decrypted state shown in the app UI*, not
> to this "server-visible" concept. They are unrelated to this proposal.

---

The remainder of this document is the design for the future feature. It is not
implemented.

Client boundary (planned):

- Browser transitions go through the wasm adapter and shared Rust client engine.
- Native desktop transitions go through Tauri commands and the same Rust client
  engine in-process.
- Platform UI code may request a visibility change, but only client-side code
  may decrypt private data before uploading a server-readable form.

## Visibility (planned)

One column:

```text
visibility = private | server_visible
```

> Not implemented: the `objects` table has no `visibility` column today.

No second storage flag.

Intended invariant (future):

- `private`: encrypted at rest. Server cannot read content or metadata. (This is
  how **all** objects behave today.)
- `server_visible`: server-readable at rest. Server and MCP may read it. (No such
  mode exists yet; nothing is server-readable today.)
- never store both forms for the same item.

## Wire

- HTTPS required.
- "Encrypted on wire" means TLS.
- `server_visible` (future) would not be end-to-end encrypted.

> Today, because no `server_visible` mode exists, all object content is
> end-to-end encrypted in addition to being sent over TLS.

## Transitions (planned)

`private -> server_visible`

- client decrypts locally
- uploads plaintext/server-readable form over HTTPS
- server replaces encrypted storage
- server indexes allowed fields

`server_visible -> private`

- client fetches readable form
- client encrypts locally
- uploads encrypted form
- server deletes readable storage and index rows

Review note: both transitions must be implemented consistently across browser
and Tauri desktop clients. The server must not be given private-mode plaintext
as part of normal sync, bootstrap, list, download, or WebSocket flows.

> Status: neither transition exists. There is no server-readable storage form
> to transition into, and no MCP/search index to populate or tear down.

## Files (planned)

- visible files may be read by server and MCP tools
- search index only filename and metadata for now
- file body fetch allowed by explicit MCP tool
- add size limits and MIME checks before returning file bytes/text

> Today, file objects are stored as ciphertext only; filenames and MIME types
> live inside `meta_ciphertext` and are not server-readable or indexed.

## Clipboard (planned)

- visible clipboard text may be indexed and fetched
- private clipboard text stays encrypted only

> Today, all clipboard text stays encrypted only; there is no indexable or
> fetchable plaintext form.

## MCP (planned)

Expose only visible items.

Initial tools:

- `search_files`
- `fetch_file`
- `search_clipboard`
- `fetch_clipboard_item`

Use separate MCP auth scopes.

Suggested scope:

```text
mcp:visible:read
```

> Status: no MCP server, tools, or scopes exist in the codebase yet.

## Delete Semantics (planned)

Unmarking visible deletes active readable storage and active index rows.

It does not promise removal from:

- logs
- backups
- snapshots
- old MCP/tool responses

> For reference, the delete path that *does* exist today (`delete_object`) only
> supports `File` objects, removes the object row, its ciphertext payload files,
> and the storage-quota reservation, and emits a `Deleted` sync event. It has no
> readable-storage or index rows to remove because none are created.
