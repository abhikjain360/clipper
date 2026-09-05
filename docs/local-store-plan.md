# Local Store Plan

Design notes for replacing the client's on-disk local store on native
platforms, and for saying honestly what the browser's store does and does not
guarantee.

**Status: built.** S1 through S7 are implemented on `schedule-module`; the
[Build order](#build-order) records what each step turned into and the
[Open questions](#open-questions) section says which are now answered and which
are still open. The [Decision Log](#decision-log) is the reasoning, kept as
written so a later change has to argue with it rather than rediscover it. This
came out of reviewing the D6 revision work on `schedule-module` (see
[`schedule-plan.md`](schedule-plan.md) D6 and
[`object-envelopes.md`](object-envelopes.md)) and is a consequence of it, not a
criticism of it: D6 closed a real rollback hole, and the cost of closing it is
what this plan pays down.

Deliberately no file or line references below. They rot, and an agent doing
this work has to read the surrounding code properly anyway. Concepts and type
names are enough to find everything.

## Why this exists

D6 gave encrypted objects a revision chain and made the client remember where
in that chain it is. That memory — the _revision anchor_ — has to outlive the
object it describes, or a server can delete an object, wait for the client to
forget, and replay an earlier revision. Retaining anchors across deletes and
reconciliation sweeps is what makes rollback detectable.

The store was not built for state that has to outlive its object. It is a
directory of JSON files, one per object, designed for content that can always
be thrown away and fetched again. Bolting a durable security control onto a
disposable cache produced the problems below.

## Findings

Verified against the code during the review of the D6 follow-up work. These are
the observations the decisions rest on; re-derive them if the code has moved on
substantially.

### The anchor is the only local state that is not a cache

Everything else the client stores — encrypted records, cached payload
ciphertext, sync markers, generation counters — is a local copy of something
the server has. Lose it and you resync; nothing is worse than slow.

The revision anchor is different in kind. Its entire purpose is to be memory
the server cannot influence, so "fetch it again" is definitionally unavailable.
It is the one thing in the store where durability is a correctness property
rather than a performance one.

Most of this plan follows from taking that distinction seriously and giving the
two classes different storage, rather than treating the whole store as one
thing with one policy.

### Deleted markers accumulate, and skipping them is not free

When an object is deleted, its record becomes a small marker holding the
anchor. The reconciliation pass used to sweep markers it no longer recognised;
that sweep was removed when anchors landed, necessarily — a swept marker is a
forgotten anchor, which is exactly the hole the anchor closes. So nothing
removes them now.

Two consequences, and the second is the one that bites:

- The count grows with _lifetime deletions_, not with data held. Clipboard
  dominates, because the server trims it to a fixed item count and a TTL, so
  every entry past the cap becomes a permanent marker on every native client.
  Logout does not help: it clears the in-memory state and deliberately leaves
  the encrypted on-disk cache. Only deleting the data directory clears them.
- A tombstone cannot be distinguished from a live object without opening and
  parsing it. The record's state is a field _inside_ the JSON and the filename
  is a bare object id. Both places that enumerate the store — hydration at
  unlock, and the per-kind reconciliation sweep — therefore pay an open and a
  parse per dead object, forever.

Disk is the milder half. The read amplification at unlock is what a person
would actually notice, and it grows linearly and permanently.

### Anchor durability is inverted

The anchor lives inside the record, and the record's usability depends on the
cached payload — the most disposable thing in the store. Hydration deletes any
record it fails to decrypt, and the clipboard and schedule previews need the
cached payload ciphertext to succeed. So a payload file that goes missing for
any reason destroys the anchor instead of triggering a refetch.

The crash window during a delete is a live example rather than the whole
problem: payload removal and record rewrite are separate, unsequenced
filesystem operations. A crash between them does not itself lose the anchor —
the record is still present and still carries its envelope — but the next
hydration cannot decrypt it and deletes it, anchor included.

The general statement is worse than any single window: **the most
security-relevant state in the store currently has the weakest durability**,
because it is welded to the least durable thing. No amount of transactionality
fixes that while the two share a lifetime.

### The browser is a different problem, not a smaller one

The browser store is bounded and evicts oldest-first, so anchors there are
already unreliable, and `object-envelopes.md` says so.

The structural reason matters more than the cap. In the browser, the code that
enforces the anchor check is delivered over the network by an origin. An
operator who controls both the API and the web origin does not need to defeat
the check — they ship a bundle without it. On native the app is installed
out-of-band, so the check is something a hostile server has to actually get
past.

This does not make the browser check worthless, and the earlier framing of
"absent by design" oversold it. The web client takes the API base URL as
user-entered input, so an honestly-served web client pointed at a
separately-operated API is a supported configuration, and there the remaining
checks do real work. The accurate description is that the browser checks
against whatever local history happens to remain, with no durability guarantee.

### Refetch-on-demand is already the model

Only clipboard caches payload bytes locally. File payloads are downloaded when
asked for, on both platforms. Nothing here proposes a new relationship with the
server; it makes the existing one explicit.

## Decision Log

### S1: SQLite on native; the browser store is left alone

Native platforms (desktop daemon, mobile) move to SQLite. The browser keeps its
current bounded store, unchanged.

The justification is atomicity and query shape, not tidiness. Transactions
remove a class of half-applied state that currently has no defence. Indexed
queries mean displaying a calendar reads the schedule rows it needs instead of
every record ever written, and the newest-N-clipboard query stops needing the
object id's embedded timestamp as a stand-in for creation order. Rows also do
not each consume a filesystem block.

Parity with the browser is explicitly _not_ a goal. Arguing for it was a
mistake: it would mean an OPFS-backed wasm SQLite or an IndexedDB port to give
a platform capabilities it does not need, when a bounded cache that refetches
on demand is a perfectly good bounded cache.

### S2: Anchors are their own table, separate from cached content

Two kinds of row, with different lifetimes and different guarantees:

- **Cached objects** — encrypted content that may be discarded and downloaded
  again at any time.
- **Revision anchors** — small rows recording what this device has already
  accepted, retained after the cached content is gone.

A deleted object stops producing a marker record entirely; it leaves an anchor
row. That is what makes hydration cheap: it queries live content and never
touches the anchors of long-dead clipboard entries.

It also fixes the durability inversion directly. When the two disagree the rule
is unambiguous — the anchor is authoritative and durable, the cached content is
disposable, and a missing or undecryptable cache entry means refetch rather
than forget.

### S3: Retain native anchors indefinitely; measure before expiring

Do not ship an expiry policy. Anchor rows are small, indexed, and never scanned
by ordinary queries, so the cost of keeping them is very different from the
cost of keeping the JSON files that prompted this.

Expiring an anchor is a deliberate reduction in rollback protection, not
housekeeping. If it turns out to be needed, it should be argued for on measured
cost, and the likely shape is per-kind — clipboard benefits least from the
protection, files and schedule blocks most — but that is a later decision with
evidence, not a starting assumption.

### S4: The browser gets clearer documentation, not new storage

No code change to the browser store. Change what is claimed about it.

`object-envelopes.md` currently reads as "the same protection, degraded by
eviction". Replace that with the honest version: the browser checks incoming
revisions against whatever local history remains and offers no durable history
guarantee, and note the structural reason — a page cannot defend itself against
the origin that serves it — while acknowledging that a separately-operated API
is a real configuration in which the remaining checks still matter.

### S5: No intermediate step

Two smaller fixes were considered and rejected, both of which would be thrown
away by S2:

- Encoding the record state in the filename so tombstones can be skipped
  without opening them.
- A packed anchor blob stored beside the record files.

Each solves part of the read cost and neither addresses the durability
inversion. Do the real change.

### S6: Full-text search is out of scope

SQLite makes clipboard search _possible_, and that is not the same as making it
safe. The local store today holds only ciphertext. An FTS index over decrypted
clipboard text would be the first plaintext at rest in the client, which is a
real break in the boundary that `local-at-rest-encryption.md` describes.

It needs its own design — an encrypted index, an in-memory-only search over the
already-decrypted working set, or an explicit and argued decision to accept
plaintext. Not a feature that rides along with a storage migration.

### S7: Hydration must stop destroying anchors, whatever else happens

Independent of the migration and worth doing first if the migration slips: a
record that fails to decrypt should not take its anchor with it. Failing to
read cached content is a reason to discard the content and refetch, never a
reason to forget what revision this device accepted.

Under S2 this is automatic, because the anchor is no longer inside the record.
Until then it is a live way to lose one.

## Out of scope

- Browser storage changes of any kind (S1, S4).
- Full-text or any other index over decrypted content (S6).
- Server-side changes. Nothing here alters the wire protocol, the envelope
  format, or the server schema.
- Backwards compatibility. Project policy is not to preserve local state, and
  the owner has confirmed data loss is acceptable, so this is a cutover: create
  the new store, delete the old directory, resync from the server. No migration
  code, no dual-read path.

## Build order

What each step became. Kept in dependency order, since a later change is
likely to want the same sequence.

1. **S7, standalone.** An unreadable cache entry is downgraded to an anchor
   instead of being deleted. Committed on its own, before any of the rest, so
   it would have landed even if the migration had not.
2. **Schema.** Three tables: `objects`, `object_payloads` hanging off it by
   foreign key, and `object_anchors` standing alone. Objects are indexed by
   kind and creation order, which is what reconciliation and the lists ask for.
3. **Storage layer behind the existing interface.** The native half of the
   store's existing native/browser split became SQLite; the browser half was
   left alone. `StoredObjectRecord` stayed the boundary type, so nothing above
   the store changed.
4. **Transactions.** A record and its payload are written together, and a
   delete is one statement pair. The states in between — a record whose payload
   never landed, a payload with no record — stopped being reachable, so the
   orphan sweep that used to look for them was deleted rather than ported.
5. **Sweeps became queries.** Reconciliation asks for the ids of one kind that
   a pass did not account for, and fetches only those. It no longer reads and
   parses every object to find out.
6. **Cutover.** Opening the database deletes the old `objects/` and
   `clipboard/` directories. Data loss by design, and better than leaving
   ciphertext behind that nothing will ever read or reclaim.
7. **Docs.** This file, plus `object-envelopes.md` (the browser paragraph) and
   `local-at-rest-encryption.md` (the storage medium, the file modes, and what
   a failed decrypt now costs).

Two things changed shape while being built, both worth knowing about:

- **Held objects get an anchor row too**, not just departed ones. The plan
  assumed a live object's envelope was anchor enough, since it carries the
  revision and hashes to the parent. But that puts the anchor back inside the
  cache entry it is supposed to outlive, which is the exact inversion S2
  exists to correct — and it would have meant a cache wipe silently forfeiting
  the accepted revision of everything still held. Writing the anchor out as
  the content lands also makes reading a head an indexed lookup rather than a
  parse and a hash.
- **A delete used to leak the payload of every kind but clipboard.** Payload
  removal was guarded by an object-kind check that predated schedule objects
  caching a payload, so deleting one left its ciphertext behind with nothing
  to reclaim it. The payload row is tied to the object row now, so there is no
  kind-specific step left to get wrong.

## Open questions

### Answered while building

- **What an anchor row holds.** The object id, the accepted revision, the
  parent hash, and which of the three anchor kinds it is — plus the object
  kind and the sequence bookkeeping, which turned out to be required rather
  than speculative: an anchor row is also the ordering guard that stops a late
  create event resurrecting a deleted object, and that guard needs the
  `event_seq` the delete carried. So the kind is there, and a future per-kind
  retention policy has what it needs for free.
- **Mobile.** The bundled SQLite cross-compiles for `aarch64-linux-android`
  through the existing NDK setup, with no cargo configuration beyond the
  dependency. Work, not risk, as expected.
- **The reconciliation generation counter.** Still per-record, and now more
  clearly right than before: the sweep's query filters on it directly, against
  the index.

### Still open

- Whether the schedule and clipboard previews should degrade instead of
  failing when cached payload bytes are missing, so a missing cache entry
  surfaces as "not loaded" rather than as an absent row. S7 made this
  survivable — the object drops out of the visible list and is refetched — but
  it is still not visible to the person looking at the list.
- Whether to encrypt the database itself (SQLCipher or equivalent), or keep the
  current boundary where content is application-encrypted and the database's
  own metadata is plaintext in a permission-restricted file. The current
  boundary is the default; changing it is a separate decision with its own
  threat model.
- **Bounded reads for display.** Hydration still loads every held object into
  memory and every list reads from there, so the indexes the schema now has
  are used by reconciliation but not yet by the lists. This is much less
  pressing than it was — what made the old store's reads grow without limit
  was the deleted markers, and those are gone — but the newest-N clipboard
  query and the calendar's date range are both things the database could
  answer directly.
- **Schema changes recreate the database**, anchors included. Acceptable while
  this is a development-time event; a change that ships to someone who has
  been running the client needs to carry `object_anchors` across instead.
- **The rusqlite version is pinned** to the line that shares
  `libsqlite3-sys` with sqlx, because only one package in the workspace may
  claim the `sqlite3` links key and the server resolves sqlx through sea-orm.
  Bumping either means bumping both.

## Related open findings not covered here

From the same review, unresolved and unrelated to storage:

- The Android alarm path logs and drops a failed foreground-service start. On
  Android 15 a `mediaPlayback` foreground service can be refused when the start
  is attributed to the post-boot window, which would make an alarm fail
  silently in exactly the reboot-then-ring case. The project's own
  [`rn-alarm-absorption.md`](rn-alarm-absorption.md) documents the restriction
  and recommends a full-screen-intent fallback; it has not been built, and
  whether `LOCKED_BOOT_COMPLETED` inherits the attribution is untested.
- The TypeScript mirror of `Recurrence` in `packages/shared` is missing the
  `Raw` variant that Rust can produce. Not reachable through the edit UI today,
  but any exhaustive switch over the union in TypeScript is unsound.

## Provenance

Written 2026-09-09, after a review of the twelve commits that followed D6 on
`schedule-module`, and implemented the same day once the decisions were agreed.
The findings were verified against the code at that point. Two unrelated fixes
from the same review are committed on the same branch: raw-rule injection
hardening in the schedule crate, and the recurrence-flattening bug in the web
composer.
