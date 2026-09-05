# Signed Object Envelopes

Encrypted clipboard, file, and schedule objects use envelope version 1. An
object has a stable id and an append-only chain of immutable revisions. The
wire types are `ObjectEnvelopeBody`, `ObjectEnvelopePayload`, and
`ObjectEnvelope` in `crates/api-types`; cryptographic construction lives in
`crates/core/src/crypto.rs`; the client creates and verifies envelopes; the
server validates placement and stores the chain.

Version 1 is the initial supported format, including revision chains. Unsupported
versions are rejected. Abandoned development formats are not supported; existing
QA objects from those formats must be regenerated. Future incompatible format
changes must increment the version. Object revision numbers are independent of
this format version.

## Keys and primitives

- `K` is the per-user object key derived from the stable OPAQUE export key with
  HKDF-SHA256 and the label `clipper:opaque-export:data-key:v1`.
- `sk_D` / `pk_D` are a device's Ed25519 signing keypair.
- Object metadata and payloads use XChaCha20-Poly1305.
- `H` is SHA-256. `Canon` is postcard serialization, whose field order is part
  of the canonical bytes.

The server never learns the OPAQUE export key or `K`.

## Version 1 body and chain

In serialization order, each signed body contains:

```text
body = (
  object_id,
  object_type,
  envelope_version = 1,
  revision,                    // 1, 2, 3, ...
  parent_hash,                 // None at 1; H(Canon(parent body)) afterward
  source_device_id,
  created_at,
  operation,                   // create | revise | delete
  meta_nonce,
  H(meta_ciphertext),
  [(payload_id, payload_nonce, ciphertext_size, H(payload_ciphertext)), ...]
)

signature = Sign(sk_D, Canon(body))
```

Revision 1 has no parent and must use `create`. Every later revision has a
32-byte parent hash and uses `revise` or `delete`. A delete revision is a signed
tombstone with no payloads. The server keeps earlier revisions, so restoring an
object means appending a new `revise` revision after the tombstone. Purging is a
separate irreversible operation.

The parent hash is over the canonical signed body, excluding the signature. A
client writing revision `n + 1` names the exact head body it accepted at `n`.
The server rejects a wrong revision number, a wrong parent hash, or a concurrent
writer that lost the `(object_id, revision)` uniqueness race with
`ObjectRevisionConflict`.

## AEAD associated data

The AAD projection binds ciphertext to the full revision identity:

```text
A_meta = Canon((
  "clipper:object-meta-aad:v1",
  object_id, object_type, envelope_version,
  revision, parent_hash,
  source_device_id, created_at, operation,
  [payload_id_1, ...],
  None
))

A_payload_i = the same projection with payload_id_i in the final field
```

Nonces, sizes, and ciphertext hashes are excluded from the AAD because the
ciphertext does not exist when its AAD is constructed. They are included in the
signed body instead. The payload-id set is present in both projections, so a
payload cannot be moved between objects or revisions without AEAD rejection.

## Server validation and storage

For both genesis and later revisions the server cross-checks the body against
the authenticated request: object id and kind, envelope version, source device,
operation, metadata nonce and hash, and the complete payload descriptor set. It
then verifies the Ed25519 signature with the authenticated device's stored
public key.

For a later revision the server loads the current completed head, computes its
body hash, and requires the submitted `(revision, parent_hash)` to be the next
link. Publication happens in one database transaction: payload rows become
complete, the revision receives its committed sequence, the object's
`head_revision` / `published_seq` / `deleted_at` projection advances, and the
matching created, updated, or deleted event is inserted. Sequence allocation
happens only after the transaction holds SQLite's write lock.

Payload rows are keyed by `(object_id, revision, payload_id)`. Streamed upload
claim, completion, and status changes are scoped to that exact revision. Each
revision's payload bytes count toward the user's storage-byte quota; additional
revisions do not consume additional object-count quota. Orphan cleanup may
remove incomplete revisions and releases only their reserved bytes. Completed
history remains until the whole tombstoned object is purged.

## Client verification and rollback limits

For a listed or fetched live head, the client checks that the clear response and
signed body agree on id, kind, revision, source device, timestamp, metadata, and
payload descriptors. It verifies the signature when the source device public
key is still available, checks downloaded payload hashes, and then decrypts
with the version 1 AAD.

The client persists the newest accepted revision body hash as a local anchor.
It rejects a served revision below that anchor, rejects a different body at the
same revision, and checks the parent hash for an immediate successor. Locally
created tombstones are retained as exact signed anchors. A delete learned only
from the event stream has no tombstone body, so the client retains the preceding
signed head and requires any later visible revision to be at least two steps
newer. Snapshot absence retains the accepted head but permits that same head to
reappear.

How durable those anchors are differs by platform, and the difference is not
one of degree. On native the anchors are rows in the client's database, kept
apart from the cached content they outlive: they survive deletes,
reconciliation sweeps, process restarts, and a cache that has been discarded or
could not be decrypted. In the browser they live in the same bounded store as
everything else, which evicts oldest-first, so the check is made against
whatever local history happens to remain and no durability is promised. The
structural reason matters more than the cap: a page cannot defend itself
against the origin that serves it, and an operator who controls both the API
and the web origin can ship a bundle without the check rather than defeat it.
That is not the same as the check being pointless there — the web client takes
the API base URL as user-entered input, so an honestly served client pointed at
a separately operated API is a supported configuration, and the remaining
checks do real work in it.

These checks do not provide global transparency. A new installation has no
anchor. If the server jumps forward by more than one revision, the client does
not possess the intermediate bodies and cannot verify each missing link. The
server can still omit objects or revisions and deny service. The local anchor
prevents rollback of history this device has already accepted; it does not
prove that the server showed the device every revision.

## Exact historical references

Schedule history uses `ObjectRevisionRef`: storage `ObjectId`, revision number,
and `H(Canon(body))`. The body hash identifies the exact accepted content, rather
than trusting that a server-supplied revision number names the same definition.
It is distinct from both the envelope format version and the schedule's domain
series ID.

Standalone occurrence overrides retain the schedule reference they were authored
against. Actual records pin the effective schedule and, where applicable, a
standalone override revision. Provider exceptions embedded in an imported event
share its revision; there is no separately stored provider exception to pin.
These references are encrypted schedule content, not server-managed foreign keys.

Revision-specific descriptor and payload reads are authenticated and scoped to
the requesting user's object and completed revision. They also work for retained
history behind a tombstoned current head. The client verifies the response/body
agreement, pinned identity and body hash, signature when the source key is
available, ciphertext bounds and hashes, and AEAD before using the definition.
A memory-only historical cache holds at most 64 entries, is scoped to the
authenticated session epoch, and is separate from current-head storage. Logout
or account changes invalidate that session's cached reads. An explicit
historical read must not pass through head acceptance, advance or lower a local
anchor, replace a live record, or change a reconciliation cursor. Reading an
older pinned revision is therefore not an exception to current-head rollback
protection.

History is immutable, not immortal. Permanent purge removes it; a server can
also withhold it. The client reports historical context as unavailable rather
than silently substituting the latest definition. Resolved planned bounds and
observer timezone on the actual remain usable even when its source history is
unavailable. References do not currently prevent purge or implement a retention
policy. A previously verified definition can remain available in memory after
server purge until eviction or session invalidation; server deletion cannot
retroactively erase client-held content. A process restart discards this cache.

Validation of this revision-aware historical-read change is pending integration
completion; the earlier verification guarantees above remain the baseline.

## Trust model

End-to-end content authenticity rests on AEAD under `K` and its AAD. The server
supplies the device public key alongside a listed object, and clients do not pin
peer device keys independently. A malicious server can therefore substitute a
public key and re-sign a matching body, but it still cannot create ciphertext
that authenticates under `K`.

Deleting a device sets revision source-device foreign keys to null. The signed
body still carries the original device id, but the server can no longer return
that device's public key. The client then skips the provenance signature check
while retaining all response/body checks and the load-bearing AEAD verification.

## Device key storage

The device signing secret is stored wrapped with a separate key derived from
the OPAQUE export key using
`clipper:opaque-export:device-identity-wrap-key:v1`. The local record uses
XChaCha20-Poly1305 with the label
`clipper:wrap:device-signing-secret:v1`. Native files and directories are
permission-restricted and written atomically. Plaintext or forged legacy
identity records are rejected rather than migrated.
