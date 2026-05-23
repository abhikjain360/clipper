# Server Visible MCP

Goal: let MCP/ChatGPT search and read only things user marks visible.

Default stays private.

Client boundary:

- macOS app transitions go through the local daemon and shared Rust client engine.
- Android app transitions go through the same Rust client engine in-process.
- Platform UI code may request a visibility change, but only client-side code may decrypt private data before uploading a server-readable form.

## Visibility

One column:

```text
visibility = private | server_visible
```

No second storage flag.

Invariant:

- `private`: encrypted at rest. Server cannot read content or metadata.
- `server_visible`: server-readable at rest. Server and MCP may read it.
- never store both forms for same item.

## Wire

- HTTPS required.
- "Encrypted on wire" means TLS.
- `server_visible` is not end-to-end encrypted.

## Transitions

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

Review note: both transitions must be implemented consistently across macOS and
Android. The server must not be given private-mode plaintext as part of normal
sync, bootstrap, list, download, or WebSocket flows.

## Files

- visible files may be read by server and MCP tools
- search index only filename and metadata for now
- file body fetch allowed by explicit MCP tool
- add size limits and MIME checks before returning file bytes/text

## Clipboard

- visible clipboard text may be indexed and fetched
- private clipboard text stays encrypted only

## MCP

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

## Delete Semantics

Unmarking visible deletes active readable storage and active index rows.

It does not promise removal from:

- logs
- backups
- snapshots
- old MCP/tool responses
