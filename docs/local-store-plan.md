# Local Store Plan

Design notes for replacing the client's on-disk local store on native
platforms, and for saying honestly what the browser's store does and does not
guarantee.

**Status: decided, not started.** No code has been written against this plan.
Everything under [Decision Log](#decision-log) is agreed and can be executed
without re-litigating it; everything under [Open questions](#open-questions)
still needs an answer, but none of it blocks starting. This came out of
reviewing the D6 revision work on `schedule-module` (see
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

Roughly dependency order; each step should be independently verifiable.

1. **S7 first, standalone.** Stop hydration from deleting anchors on decrypt
   failure. Small, safe, and valuable whether or not the rest lands.
2. **Schema.** Two tables, cached content and anchors, with the indexes the
   real queries need — at minimum by kind, and by whatever ordering the
   clipboard list and the schedule expander actually ask for.
3. **Storage layer behind the existing interface.** The local store already has
   a native/browser split; the native side becomes SQLite while the browser
   side is untouched. Keep the public surface stable so the engine does not
   change in this step.
4. **Fold the writes into transactions.** Delete, revise and reconcile each
   become one transaction rather than a sequence of filesystem operations.
5. **Make the sweeps queries.** Reconciliation should be a statement over an
   index, not a read-and-parse of everything.
6. **Cutover.** Delete the old directory on first run of the new store.
7. **Docs.** S4's rewording, plus a note in `object-envelopes.md` that anchors
   are durable on native and best-effort in the browser.

## Open questions

None of these block starting; they need answering along the way.

- What exactly an anchor row holds. At minimum the object id, the accepted
  revision, the parent hash, and which of the three anchor kinds it is
  (locally-written tombstone, delete observed without a body, absent from a
  snapshot) — since those carry different minimum-next-revision rules. Whether
  it also needs the kind, for a future per-kind retention policy, is undecided.
- Whether the schedule and clipboard previews should degrade instead of
  failing when cached payload bytes are missing, so a missing cache entry
  surfaces as "not loaded" rather than an error. Related to S7 but separable.
- Whether to encrypt the database itself (SQLCipher or equivalent), or keep the
  current boundary where content is application-encrypted and metadata is
  plaintext in a permission-restricted file. The current boundary is the
  default; changing it is a separate decision with its own threat model.
- Mobile build implications of bundling SQLite through the UniFFI layer for
  Android. The workspace already builds C dependencies, so this is expected to
  be work rather than risk, but it has not been checked.
- Whether the reconciliation generation counter still needs to be per-record
  once sweeps are queries.

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
`schedule-module`. The findings were verified against the code at that point.
Two fixes from the same review are already committed on that branch: raw-rule
injection hardening in the schedule crate, and the recurrence-flattening bug in
the web composer. The storage work described here was deliberately not started,
so that it could be agreed first.
