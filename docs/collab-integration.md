# Collab Integration

> Status: **PROPOSAL / NOT IMPLEMENTED.** This is a forward-looking design for
> folding the standalone `collab` project (live document editing) into clipper
> as a `server_visible` feature. None of it ships today: there is no document
> store, no CRDT room manager, no share-link route, and no anonymous access path
> in the codebase. Do not read the invariants below as guarantees the running
> server currently enforces. The guarantee that *does* hold today is the one in
> [`server-visible-mcp.md`](server-visible-mcp.md): all synced objects are
> end-to-end encrypted and opaque to the server.

## Goal

Bring `collab`'s feature — real-time collaborative document editing with
anonymous share links — into clipper, reusing clipper's server, OPAQUE auth, and
the planned `server_visible` storage mode instead of running a second product.

The owner is a normal authenticated clipper user. They create a document and
hand out a secret link that anyone can open and edit live, with no account. The
document content is **not** end-to-end encrypted: a shared-with-strangers,
real-time-merged document is fundamentally server-readable (see "The core
tension").

## Source project (`collab`, today)

For reference, the standalone project at `/Users/abhik/coding/webb/collab`:

- **Client** — Vite + TypeScript, CodeMirror 6, Yjs CRDT over a WebSocket
  provider.
- **Server** — Bun + Elysia, Yjs document rooms over WebSockets, Drizzle ORM on
  SQLite for document metadata and persisted CRDT state.
- **Auth** — single owner via `ADMIN_PASSPHRASE` + JWT sessions. Share links are
  `/d/<slug>?token=<token>`, openable by anyone with no account.

Integration keeps the Yjs/CodeMirror client model and the share-link UX, but
replaces collab's runtime and auth: the CRDT server moves to Rust (`yrs` /
`y-crdt`), and owner auth becomes clipper's existing OPAQUE flow. The only
genuinely new HTTP surface is the public share-link path.

## The core tension

Clipper's identity is zero-knowledge: the server stores only ciphertext, the
data key `K` is derived client-side from the OPAQUE `export_key`, and raw content
never reaches the server (see [`object-envelopes.md`](object-envelopes.md)).

Collab is the opposite on the axis that matters: anonymous participants edit a
shared document in real time, and the server merges and persists CRDT state. You
cannot have both "anyone with the link edits" and "server can't read it" without
an out-of-band key exchange — which would destroy the no-signin UX that makes
collab worth having.

Resolution: collab documents live in the **`server_visible`** class. Their
content is plaintext-at-rest on the server, exactly the opt-in mode proposed in
[`server-visible-mcp.md`](server-visible-mcp.md). This is a deliberate,
explicit, per-document departure from end-to-end encryption — never a silent
default, and always surfaced loudly in the UI.

`server_visible` is therefore a **shared primitive with two consumers**:

| Consumer | Actors | Storage shape | Mutability |
| --- | --- | --- | --- |
| MCP / search (`server-visible-mcp.md`) | owner only, authenticated | snapshot, content-addressed | one-shot replace |
| Collab (this doc) | owner + anonymous editors | streaming CRDT updates + snapshots | continuous |

They share the *philosophy* (explicit opt-in, plaintext-at-rest, default stays
private) but **not the storage model**. Do not shoehorn collab documents into
the `visibility`-flagged `objects` table. See "Storage model".

## Two data planes

The central architectural decision: collab is a second data plane, fully
separate from object sync.

```text
Plane A — object sync (existing)
  E2E-encrypted objects (clipboard, file)
  init -> upload payload -> complete, immutable, content-addressed
  envelope-signed, AEAD-sealed under K
  ordered by event_log.seq, replayed via snapshot sync
  every row scoped by authenticated user_id

Plane B — collab (new)
  server_visible documents, plaintext-at-rest
  long-lived rooms taking a continuous stream of CRDT updates
  no envelope, no K, no signature
  realtime fan-out per room, NOT through event_log.seq
  owner is a user_id; editors are anonymous share-token holders
```

The planes meet only at: (1) the owner's identity (a collab document is owned by
a `user_id`), and (2) the `server_visible` opt-in concept. They must not share
storage, the sync cursor, the broadcast machinery, or the client decrypt path.

## Storage model

Collab documents get their own tables, not a flag on `objects`.

Why `objects` is the wrong carrier:

- The object model is immutable and content-addressed: a payload is pinned by
  `ciphertext_size` + `sha256_ciphertext`, and `operation = create` is the only
  operation that exists (see `object-envelopes.md`). A CRDT document mutates
  continuously; there is no stable content hash to pin.
- The `server-visible-mcp.md` transition model (decrypt locally → upload one
  readable form → server replaces storage) is a one-shot snapshot, not a stream.

Proposed schema (illustrative):

```text
documents
  id            uuid primary key
  owner_user_id fk -> users(id)
  title         text                 // server-readable (server_visible)
  created_at, updated_at
  expires_at    nullable
  state         active | revoked

document_state
  document_id   fk -> documents(id)
  yjs_snapshot  blob                 // periodic compacted Yjs state
  snapshot_seq  integer              // doc-local, NOT event_log.seq

document_updates                     // optional append log between snapshots
  document_id   fk -> documents(id)
  update_blob   blob                 // Yjs update
  created_at

share_links
  id            uuid primary key
  document_id   fk -> documents(id)
  token_hash    blob                 // hash of the high-entropy token; never store raw
  capability    view | edit
  created_at
  expires_at    nullable
  state         active | revoked
```

Storage accounting: collab document bytes are charged to a **per-document** cap,
independent of the owner's object storage quota, because anonymous editors write
into them (see "Anonymous surface").

## Realtime plane (yrs rooms)

CRDT fan-out is a per-document room, served over a dedicated WebSocket path,
implemented on `yrs` / `y-crdt` (optionally `yrs-warp`-style room glue adapted to
Axum). Each room:

- holds the live Yjs document in memory, applies incoming updates, and broadcasts
  them to the other room members;
- persists periodically: compact to `document_state.yjs_snapshot`, optionally
  append to `document_updates` between snapshots;
- carries Yjs **awareness** (presence/cursors) as ephemeral, non-persisted state.

Keep this plane **off `event_log.seq`.** `event_log.seq` is the monotonic
microsecond sync cursor allocated under the write lock (`AppState::next_event_seq`,
see `AGENTS.md`); it is the object-sync ordering primitive. A busy document
produces a high-frequency stream of small updates — routing those through
`event_log` would hammer seq allocation and bloat the log. Document ordering uses
a doc-local `snapshot_seq`, never the global cursor. The per-user
`tokio::sync::broadcast` channel used for object sync is likewise not reused;
each room has its own fan-out.

## Auth model

- **Owner** authenticates through the existing OPAQUE flow
  (`crates/server/src/routes/auth.rs`) and bearer session. Creating, renaming,
  revoking, and listing documents and share links are owner-only,
  `user_id`-scoped operations — same discipline as object routes.
- **Anonymous editors** present a share-link token. The token is a high-entropy
  capability (treat like the access-key generation path: 256-bit, base64). The
  server stores only `token_hash`; the raw token lives only in the URL the owner
  distributes. A token grants access to exactly one document at one capability
  (`view` / `edit`), and nothing else — no object access, no other documents, no
  account.

The anonymous WebSocket connection authorizes off the share token, not a bearer
ticket. It must resolve to `(document_id, capability)` and be confined to that
room.

## Anonymous surface hardening

This is the highest-risk part of the integration and the part with no precedent
in the current codebase. Clipper's entire safety model is keyed on authenticated
`user_id`: rate limits (`auth_by_client`, `api_by_user`, `ws_tickets_by_user`),
per-user storage quotas, per-user broadcast channels, and pending-ticket caps
(see [`server-resource-limits.md`](server-resource-limits.md)) all assume a
`user_id`. Anonymous editors have none, so a parallel abuse-control layer keyed
by `(document, token, IP)` is required:

1. **Rate limiting on the public surface.** The share-resolve endpoint and the
   anonymous WebSocket need their own buckets keyed by client IP and by document.
   Note that `/api/health` and `/api/ws-ticket/connect` are already unthrottled
   today; do not extend that pattern — every new anonymous route is throttled
   from the start.
2. **Per-document caps.** Anonymous writes are charged to a per-document size and
   update-rate cap, independent of the owner's object quota, so a hostile editor
   cannot balloon the owner's storage or exhaust the server.
3. **Token entropy, expiry, and revocation.** Share tokens are long-lived bearer
   capabilities held by third parties — strictly more dangerous than session
   tokens. They must support expiry and immediate revocation on day one.
   (Clipper has no session/device revocation today; that gap, tracked separately,
   must close before this ships — see "Sequencing".)
4. **Origin / cross-site WebSocket hijacking.** The object-sync WebSocket
   deliberately skips an `Origin` check because tickets are non-cookie bearers
   minted by an authenticated user, so a cross-origin page cannot obtain one
   (`crates/server/src/ws.rs`). That reasoning breaks for a URL-reachable
   anonymous endpoint: the capability is in the URL, so a malicious page can
   drive the socket. The collab WebSocket needs an `Origin` allowlist and a
   deliberate token-placement decision (subprotocol vs query) with CSWSH in mind.
5. **Abuse of content.** Server-readable, anonymously-writable documents are a
   spam/abuse host. At minimum: document count caps per owner, and an owner kill
   switch (revoke link / freeze document).

## Client path divergence

Collab documents must not flow through the encrypted-object client path. The
existing client verifies envelopes, checks payload SHA-256, decrypts under `K`,
and **purges local items absent from the server snapshot**
(`crates/client/src/local_store.rs`). A `server_visible` document has no
envelope, no `K`, and no signature. The client branches cleanly:

- collab documents are fetched/edited through the CRDT plane, never decrypted or
  envelope-verified;
- the "purge if absent from snapshot" logic operates only on Plane A and never
  touches collab state;
- the editor UI (CodeMirror + Yjs binding) is a distinct surface from the
  clipboard/file views.

## Trust model and narrative

- Adding `server_visible` changes the one-line pitch from "the server stores only
  ciphertext" to "the server stores only ciphertext, except documents you
  explicitly share." This is defensible but must be stated plainly in `README.md`
  and `SECURITY.md`.
- **Blast radius:** a server compromise previously yielded only ciphertext. With
  collab, it also yields every shared document in plaintext. Owners must
  understand that a shared document is server-readable by design.
- **Default stays private.** Consistent with `server-visible-mcp.md`: nothing is
  server-visible unless the user explicitly makes it so, and the UI marks a
  collaborative document as not-end-to-end-encrypted unmistakably — never a
  silent flag.

## Sequencing

This is a v2 subsystem. It adds unauthenticated, mutating attack surface to a
server whose safety model assumes authenticated `user_id`, so it should not debut
alongside the initial security-focused release, and ideally not before a security
review of the anonymous surface.

1. **Ship clipper's private sync product first.** Collab is strictly additive and
   blocks nothing in the core release.
2. **Land session/device revocation** (already needed independently). Share links
   are non-viable without revoke + expiry, and the same primitive serves both.
3. **Build the `server_visible` primitive once**, for the simpler MCP/search case:
   authenticated, snapshot-based, owner-only. Get "default private / explicit
   opt-in / loud UI" right there.
4. **Add collab as its own document type** reusing the `server_visible`
   philosophy but with its own streaming storage, its own room manager, and its
   own anonymous abuse-control layer.

## Open questions

- Does an anonymous editor's identity need to persist at all (named cursors,
  per-editor attribution), or is fully anonymous awareness sufficient?
- Snapshot/compaction cadence and `document_updates` retention vs. snapshot-only.
- Should `view` vs `edit` capability be enforced server-side on every CRDT update
  (reject writes on a `view` token) — yes, since the token is the only authority.
- Per-document storage cap value and behavior on overflow (reject vs. freeze).
- Whether collab documents are ever convertible to/from private E2E objects, or
  are a permanently distinct, server-visible-only type (leaning: distinct type).
</content>
</invoke>
