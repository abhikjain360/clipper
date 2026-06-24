# Collab Docs Plan

Three-phase plan to add a collaborative document layer to Clipper. The end
state replaces the standalone `webb/collab` project entirely.

## Background

Clipper currently stores all objects encrypted end-to-end; the server sees only
ciphertext. Collab docs are a deliberate exception: content must be
server-visible so the server can apply and relay Y-CRDT updates. This is
opt-in and explicit per object — it does not weaken the encryption model for
clipboard or file objects.

## Schema Design

Two new / modified constructs, introduced in a single migration:

### New table: `collab_docs`

```
collab_docs
  id            UUID        PK
  owner_user_id UUID        NOT NULL  FK → users(id)  ON DELETE CASCADE
  share_token   TEXT UNIQUE NOT NULL  — random token for "anyone with link" access
  yjs_state     BLOB                  — Y.Doc binary; null until first edit
  created_at    TEXT        NOT NULL
  updated_at    TEXT        NOT NULL
```

`share_token` is the sole auth credential for unauthenticated access.
No title column — the document title lives inside the Y.Doc state.

### Modified table: `objects`

New column:

```
collab_doc_id   UUID        — nullable FK → collab_docs(id)  ON DELETE CASCADE
```

Three existing columns become nullable:

```
meta_ciphertext BLOB        — was NOT NULL
meta_nonce      BLOB        — was NOT NULL
envelope        BLOB        — was NOT NULL
```

New XOR check constraint:

```sql
CHECK (
  (meta_ciphertext IS NOT NULL AND meta_nonce IS NOT NULL
   AND envelope IS NOT NULL AND collab_doc_id IS NULL)
  OR
  (meta_ciphertext IS NULL AND meta_nonce IS NULL
   AND envelope IS NULL AND collab_doc_id IS NOT NULL)
)
```

`kind` check gains `'collab'`. `status` for collab objects is always
`'complete'` (no upload phase); `created_seq` is assigned at row insert, so
the existing `complete → created_seq NOT NULL` constraint is satisfied without
change.

### Modified table: `event_log`

`object_kind` check gains `'collab'`. The cross-column deleted constraint
becomes:

```sql
CHECK (
  event_type = 'created'
  OR (event_type = 'deleted' AND object_kind IN ('file', 'collab'))
)
```

Clipboard items still expire passively and never emit `deleted` events.

---

## Phase 1 — CodeMirror for existing file objects

**Goal:** text-file objects in the Files section open in a CodeMirror 6 editor
instead of a plain text dump or download prompt.

### What changes

**`web/src` (shared web + Tauri desktop)**

- Add `@codemirror/view`, `@codemirror/state`, `@codemirror/commands`,
  `@codemirror/lang-*` (detect from MIME type / filename extension), and
  `@codemirror/vim` to `web/package.json`.
- New `<CodeEditor>` component wrapping a CodeMirror `EditorView`. Read-only by
  default for file objects (they are immutable). Vim key bindings opt-in from
  user settings (persisted locally).
- File viewer route detects text MIME types, decrypts payload, renders in
  `<CodeEditor>`. Binary files keep their existing download / preview path.

**`mobile/src`**

- Text file viewer gets a scrollable `<Text>` block with a monospace font for
  now. Vim bindings and a full editor are out of scope on mobile — the touch
  editing model is too different.

### What does NOT change

Server, API types, sync engine, encryption — untouched.

---

## Phase 2 — Collab object API + basic UI

**Goal:** users can create and list collab docs. No live sync yet — the editor
is static (same `<CodeEditor>` from Phase 1, read-only placeholder content).
This phase also lands the schema migration so Phase 3 has a clean base.

### Server

- Migration: add `collab_docs` table + `objects` modifications above.
- `POST /api/collab-docs` — authenticated; creates a `collab_docs` row and a
  corresponding `objects` row with `kind='collab'`, `status='complete'`,
  `collab_doc_id` set, ciphertext columns NULL. Returns the new object id and
  `share_token`. Emits a `created` event into the event log.
- `DELETE /api/collab-docs/:id` — authenticated, owner only; deletes the
  `collab_docs` row (cascades to `objects`). Emits `deleted` event.
- `GET /api/collab-docs/:id/meta` — authenticated; returns `share_token` and
  `updated_at`. Does not return `yjs_state` (that flows over the Y-sync WS in
  Phase 3).

No `yjs_state` read/write in this phase — those columns exist but are unused.

### `api-types`

- New request/response types for the three endpoints above.
- Add `Collab` variant to `ObjectKind`.

### `crates/client`

- Sync engine and local store handle `kind = 'collab'` in event log and object
  listings; collab objects surface in the app state alongside files.

### Web + desktop UI

- New "Collab Docs" section in the nav, separate from Files.
- Create dialog: one action, no required input (title is set inside the doc
  itself in Phase 3). On create, navigate to the new doc's editor view.
- Doc list and per-doc view reuse the same layout patterns as the Files section.
- Per-doc view renders `<CodeEditor>` in read-only mode with empty / placeholder
  content until Phase 3 wires up live state.
- Share-token copy button in the doc detail view (displayed but non-functional
  until Phase 3 adds the public page).

### Mobile

- Collab section appears in the nav; lists docs. Tapping a doc shows its title
  (from metadata) and a "Open in browser" deep-link for now. Full editing stays
  web-only.

---

## Phase 3 — Y-sync, live editing, and public share links

**Goal:** collab docs are live CRDT-synced documents accessible to owners
across all their devices and to anyone holding a share link.

This phase is large but splits cleanly into two sub-phases:

### Phase 3a — Server Y-sync infrastructure (no client UI change)

Dependencies: `yrs`, `y-sync` from the y-crdt crate family.

**Room manager** (new struct in `AppState`)

```rust
yjs_rooms: Mutex<HashMap<Uuid, YjsRoom>>

struct YjsRoom {
    doc: yrs::Doc,
    awareness: y_sync::awareness::Awareness,
    conns: HashMap<ConnId, HashSet<u32>>,  // conn → owned awareness client ids
    persist_handle: Option<JoinHandle<()>>,
}
```

Rooms are created on first WS connection and unloaded (after a grace period)
when the last connection closes. On load, `yjs_state` is read from `collab_docs`
and applied to a fresh `yrs::Doc`. On every doc update, a 2-second debounced
task writes `Y.encodeStateAsUpdate()` back to `collab_docs.yjs_state`.

**New WS endpoint: `GET /api/collab-docs/:id/ws`**

Accepts either:

- A session Bearer token (authenticated owner or any device of the owner), or
- `?token=<share_token>` (anyone with the link, no user account required).

The endpoint is separate from `/api/ws` — it speaks the binary y-sync protocol
(lib0-encoded frames: `MSG_SYNC = 0`, `MSG_AWARENESS = 1`), not the JSON event
protocol. The existing ticket mechanism, per-connection ping/idle timeouts, and
send-timeout logic are reused structurally.

On `open`: send sync-step-1 + current awareness states to the new client.
On `message`: route to `y_sync::sync::handle_msg` or awareness update handler.
On `close`: remove connection, clean up awareness states, schedule room unload.

This sub-phase ships as a pure backend change. It is testable with a simple
HTML file or wscat before any UI work.

### Phase 3b — Authenticated live editing + public share page

**`web/src` (web + Tauri desktop)**

- Upgrade `<CodeEditor>` in the collab doc view to a live `yjs` + `y-websocket`
  document:
  ```ts
  const ydoc = new Y.Doc();
  const ytext = ydoc.getText("content");
  const provider = new WebsocketProvider(wsBase, docId, ydoc, {
    params: authenticated
      ? {
          /* session via header */
        }
      : { token: shareToken },
  });
  yCollab(ytext, provider.awareness); // @codemirror/collab binding
  ```
- Awareness (live cursors and user badges): set `provider.awareness` local
  state with the user's display name and a stable colour derived from their
  user id. For anonymous share-link users, a random ephemeral colour.
- The "Copy share link" button in the doc detail view now copies the public URL
  `<origin>/s/:share_token`.

**New public route: `/s/:share_token`**

- Web-only. No authentication required.
- Served by the same Vite app but under a public-accessible URL prefix.
- Loads the collab doc metadata via a new unauthenticated endpoint
  `GET /api/s/:share_token/meta` (returns doc id, title from yjs_state if
  derivable, or just the token).
- Connects to `GET /api/collab-docs/:id/ws?token=<share_token>`.
- Renders `<CodeEditor>` in live-sync mode with vim bindings opt-in.
- No nav bar, no account UI — just the editor and a subtle "Made with Clipper"
  footer.

**`crates/server`**

- `GET /api/s/:share_token/meta` — unauthenticated; looks up `collab_docs` by
  `share_token`, returns `{ id, updated_at }`. Does not return `yjs_state` in
  full (client syncs via WS).

### Can Phase 3 be done all at once?

No, and splitting it is cleaner:

- **3a is independently shippable** — pure server, no user-visible change.
  Testable with a standalone HTML test page or curl.
- **3b requires 3a** but can ship as one client + server PR once 3a is merged.
  Live cursors are part of this (awareness is a single message type, not a
  separate system).
- The public share page (`/s/:share_token`) is part of 3b — it is a new Vite
  route, not a separate deployment, and it reuses the same WS endpoint and
  `<CodeEditor>` component. It ships in the same PR as authenticated editing.

---

## Packages to add

| Package                                                         | Phase | Where                      |
| --------------------------------------------------------------- | ----- | -------------------------- |
| `@codemirror/view`, `@codemirror/state`, `@codemirror/commands` | 1     | `web/package.json`         |
| `@codemirror/lang-*` (markdown, js, rust, …)                    | 1     | `web/package.json`         |
| `@codemirror/vim`                                               | 1     | `web/package.json`         |
| `yjs`                                                           | 3b    | `web/package.json`         |
| `y-websocket`                                                   | 3b    | `web/package.json`         |
| `@codemirror/collab`                                            | 3b    | `web/package.json`         |
| `yrs`                                                           | 3a    | `crates/server/Cargo.toml` |
| `y-sync`                                                        | 3a    | `crates/server/Cargo.toml` |

---

## What this plan does not cover

- **Mobile editing:** collab docs on mobile stay list-only in Phase 2; Phase 3b
  adds an "Open in browser" link that deep-links to the share URL. Full mobile
  editing (CodeMirror in a WebView) is left for later.
- **Access tiers:** "anyone with link can edit" is the only share mode. Read-only
  share links are a future addition.
- **Collab clipboard items:** only the `'collab'` kind (document) is introduced.
  Extending collab to clipboard entries is out of scope.
- **Server storage quotas:** `yjs_state` growth is not tracked in
  `users.storage_bytes` for now.
