# Local At-Rest Encryption

Clipper is end-to-end encrypted: clipboard and file payloads are encrypted on
the client before they reach the server, and the server only ever stores
ciphertext (see `docs/object-envelopes.md`, `docs/opaque.md`). This document
covers the _client_ side of at-rest protection: what the local cache and the
local device identity contain on disk (or in browser `localStorage`), which keys
protect them, where those keys come from, and the filesystem-level safeguards
(ownership checks, `0700`/`0600` modes, atomic writes).

The relevant code is:

- `crates/client/src/local_store.rs` — the on-disk/`localStorage` cache and the
  persisted device signing identity.
- `crates/client/src/engine.rs` — how keys are loaded into the running engine
  and which profile directory the cache lives under.
- `crates/client/src/api_client.rs` — derivation of the data key and the
  device-identity wrapping key from the OPAQUE export key.
- `crates/core/src/crypto.rs` — the AEAD, KDF, and wrap/unwrap primitives.
- `crates/fs-txn/src/lib.rs` — a separate rollback guard for staged file writes,
  used by the server rather than the client (see the last section).

## Key material and where it comes from

All client-side keys derive from a single root: the **OPAQUE export key**
returned by `opaque_client_login_finish` / `opaque_client_register_finish`. The
export key is stable for a given `(passphrase, server registration)` pair across
logins, and it never leaves the client. From it the client derives two
independent 32-byte keys with HKDF-SHA256, using distinct domain-separation
labels (`crates/core/src/crypto.rs`):

- **Data key**: `derive_data_key_from_opaque_export_key`, label
  `clipper:opaque-export:data-key:v1`. It encrypts and decrypts all object
  material (clipboard meta, clipboard payloads, file meta, file blobs). This is
  the end-to-end key behind the ciphertext the server stores.
- **Device-identity wrapping key**:
  `derive_device_identity_wrapping_key_from_opaque_export_key`, label
  `clipper:opaque-export:device-identity-wrap-key:v1`. It wraps the persisted
  device signing secret at rest.

The passphrase and OPAQUE export key are not persisted. Both derived keys can
be retained as session-resume material by clients with a suitable credential
store. The resume record contains the server-revocable bearer token, the data
key, the device-identity wrapping key, and the last confirmed session time.
These derived keys can decrypt the cache and unwrap the existing device
identity; they cannot re-run OPAQUE login or enroll a new device. In-memory
derived keys use `Zeroizing` wrappers, but not every copy is wiped on drop:
[`docs/issues.md`](issues.md), entry 42.

### What the OS keychain stores (and does not)

For the macOS daemon, `crates/daemon/src/keychain.rs` stores:

- The 32-byte **IPC secret** (`ipc-secret-v1`) used for the local daemon/UI HMAC
  handshake — unrelated to data encryption. See `docs/local-ipc-security.md`.
- A `Credentials` record under service `com.clipper.daemon`, account
  `credentials`: device name, server URL, username, a random non-secret session
  ID, and session-resume material.
  The daemon first tries the macOS data protection keychain with
  `AccessibleWhenUnlockedThisDeviceOnly`, no iCloud synchronization, and no
  user-presence constraint. An entitled build can resume unattended while the
  device is unlocked, with a record that is not transferable to another device.
  If that store rejects the build for a missing entitlement or code-signing
  error, the daemon saves the record in the regular login keychain instead.
  This lets local ad-hoc builds resume without an Apple team or access-group
  entitlement.

The default daemon data directory keeps service `com.clipper.daemon` and the
existing account names so existing items still work. Every other data directory
uses service `com.clipper.daemon.<hash>`, where `<hash>` is the lowercase SHA-256
hex digest of its canonical path. Both session stores and the IPC secret use
that service. An independent QA directory cannot read, overwrite or delete the
default directory's items. Path aliases use the same items as their target;
moving a directory changes its names and requires a new login. The daemon,
desktop app and CLI use the shared `clipper-daemon-client` derivation. Select a
directory with `CLIPPER_DATA_DIR`, or `--data-dir` for the daemon and CLI.

The login keychain is local to this Mac and normally unlocked while the user
is logged in. Its default access rule trusts the creating binary; other
programs get a macOS prompt. The daemon uses the same default rule as its IPC
secret, without an allow-all ACL or a user-presence requirement. This fallback
uses the login keychain's locking and backup behaviour, rather than the data
protection keychain's explicit when-unlocked, this-device-only class. Access
to either resume item is enough to decrypt the cache and unwrap the device
identity. This is the accepted trade-off for unattended local builds in issue 53.

macOS applies the accessibility class to the data protection store rather than
the regular login keychain. See
[Apple's data protection keychain documentation](https://developer.apple.com/documentation/security/ksecusedataprotectionkeychain).
See [Apple's default access-rule documentation](https://developer.apple.com/documentation/security/secaccesscreate%28_%3A_%3A_%3A%29)
for trusted applications and prompts.

The non-secret profile records the current session ID and which store holds
it. Reads inspect both stores but return only the item matching that record,
username, server URL, and device name. Ignore and try to delete other copies,
including copies left behind after failed deletion. An absent session record
or no matching item requires login with the remembered profile prefilled.
A successful save records the chosen store and attempts to remove the
superseded copy from the other store. Logout attempts deletion in both stores,
even if either fails. Logs name the store
used for every credential read or save. Locked stores, denied access, and
other failures do not cause a storage downgrade. If the selected item's read
is denied after an ad-hoc signature change, startup keeps sign-in prefilled
with the remembered server URL and username. The IPC secret remains separate.

The daemon keeps non-secret profile metadata in `Clipper/profile.json`, with
`0700` directory and `0600` file permissions. This includes the username,
device name, server URL, the current session record, and a `signed_out` marker.
Only explicit logout or a server rejection sets `signed_out`. Logout atomically
saves and syncs that marker before reporting success. Startup checks it before reading
session credentials, refuses resume, and retries credential deletion. A
profile that cannot be read also prevents resume. The server URL and username
remain available when a keychain read fails.

Linux has no configured session secret store. It saves only the non-secret
profile and requires login at every daemon start. It never writes a bearer token
or either derived key to `credentials.json`, and startup removes that file if
an earlier build left it behind. Its separate IPC secret remains a
private file. Tests inject an explicit in-memory credential store; they do not
use the Linux credential path or the owner's macOS keychain.

The server session is validated on resume. The existing offline unlock rule
allows a session confirmed within three days when the server cannot be reached.
Unchanged session credentials are not rewritten for each minute's confirmation:
the daemon persists a newer confirmation at most once every six hours. Token,
key, or profile changes are saved immediately with a new session ID.
Confirmation updates keep the same ID. A failed credential write keeps the previously saved
copy and its profile record valid, logs the failure, and retries later. It does
not mark the session signed out. If a different account cannot be saved, remember
its login profile without a resume record, so the old account cannot return.
Normally the persisted confirmation shortens offline availability by up to six
hours. Repeated write failures can shorten it further. It never extends the
offline window.

The browser retains resume material in tab-scoped `sessionStorage`, rather than
the durable object cache in `localStorage`. Android retains resume material in
the platform secure store with authentication required. Native signing secrets
remain wrapped on disk. Persistent derived keys improve restart behaviour but
make access to the resume credential store sufficient to open the local cache;
see issue 53 in `docs/issues.md` for the desktop trust decision.

## What is encrypted at rest

### Cached objects (clipboard and files)

On native the store is a SQLite database (`store.sqlite3`) in the profile
directory; in the browser it is `localStorage`. Either way it stores, per
object, a `StoredObjectRecord` and (for clipboard and schedule objects) a
separate payload ciphertext. The persisted record never contains plaintext.
Specifically, a `Present` record holds an `EncryptedObject`:

- `meta_nonce` + `meta_ciphertext` — the object metadata (clipboard MIME type and
  size, or filename/MIME/size for files), AEAD-encrypted under the data key.
- `payloads` — `ObjectPayloadDescriptor`s carrying each payload's nonce,
  ciphertext size, and SHA-256 of the ciphertext.
- `created_at`, `source_device_id`, and the signed `ObjectEnvelopeV1`.

The actual payload ciphertext is stored apart from the record — its own row on
native, its own `localStorage` key in the browser — rather than inlined, so
reading a record does not drag the payload along with it. Natively the payload
row is tied to the object row and goes when it does, and the two are written in
one transaction, so a record whose payload never landed is not a state the
store can reach. File-object blobs are not cached locally at all; they are
downloaded and decrypted on demand (`SyncEngine::download_file_bytes`).

This ciphertext is byte-for-byte the same XChaCha20-Poly1305 ciphertext the
client uploaded to the server: the local cache reuses the E2EE ciphertext rather
than re-encrypting under a separate local key. Encryption/decryption uses
`crypto::encrypt` / `crypto::decrypt` (XChaCha20-Poly1305, 24-byte random nonce)
with the per-object envelope body bound in as AAD
(`object_meta_aad_v1` / `object_payload_aad_v1`), so a ciphertext cannot be
moved between objects, payload slots, meta-vs-payload roles, or fields without
failing authentication.

On hydration (`hydrate_ciphertext_cache`) the cache decrypts each held record
with the in-memory data key. A record that fails to decrypt or fails its
integrity checks is logged and its content discarded rather than surfaced — but
not its revision anchor, which is retained so that an unreadable cache entry
costs a refetch and not the rollback protection for that object. Before a
cached clipboard payload is
decrypted, `verify_payload_ciphertext` re-checks its length and SHA-256 against
the descriptor, so a tampered or truncated ciphertext file is rejected.

#### Plaintext that is _not_ persisted

Decrypted display state is kept only in memory (`MemoryState`). The only
plaintext-derived value retained is a **bounded clipboard preview**
(`CLIPBOARD_TEXT_PREVIEW_MAX_CHARS = 512` characters for text MIME types, or a
`"<mime> clipboard payload (N bytes)"` label for non-text). The preview is
recomputed from decrypted bytes on hydration — it is never written to the
on-disk record, and the caller-supplied preview text is _not_ trusted (the
`derives_bounded_preview_without_trusting_caller_text` test asserts this). Full
payload bytes are only ever decrypted transiently for the operation that needs
them (e.g. `clipboard_payload`, copy-to-clipboard, file download).

### Device signing identity

Each client device has an Ed25519 signing secret used to sign object envelopes
and login proofs (`docs/object-envelopes.md`). It is persisted across restarts
so the device keeps a stable identity. It is stored **wrapped** in a
profile-scoped `device-identity-v1.<profile_id>.json` file under the cache base
directory (native) or under a profile-scoped `localStorage` key (web).

The current on-disk record is `DeviceIdentityEncryptedRecord`:

- `version` — `DEVICE_IDENTITY_RECORD_VERSION_V3` (3); other versions are
  rejected with `UnsupportedDeviceIdentityVersion`.
- `device_id` — the server-assigned device UUID, stored in cleartext but
  authenticated (see the AAD below).
- `wrapped_signing_secret_key` — the signing secret sealed with
  `crypto::wrap_with_key(wrapping_key, secret, aad)`, i.e.
  `nonce_24 ‖ XChaCha20-Poly1305(secret)`.

The AAD is `device_identity_record_aad`: the postcard encoding of
`(clipper:wrap:device-signing-secret:v1, version, device_id, profile_id)`. It
binds the record's whole cleartext header to the secret, so a substituted
`device_id`, a changed version, or a record copied into another profile's slot
fails the tag instead of being accepted
(`rejects_device_identity_record_with_a_substituted_device_id`,
`rejects_device_identity_record_copied_from_another_profile`). A `device_id`
that is not a UUID is an error too, not a reason to mint a fresh identity
(`rejects_device_identity_record_with_a_malformed_device_id`), because
re-minting would abandon the registered device.

Unwrapping requires the device-identity wrapping key; a wrong key fails with
`DeviceIdentityDecrypt` (asserted by `encrypts_device_identity_at_rest`). The
in-memory `signing_secret_key` is held in `Zeroizing`.

#### Plaintext records

Plaintext identity records are not accepted. A record without
`version = 3` and `wrapped_signing_secret_key` fails closed and is not silently
promoted into a wrapped identity (`rejects_plaintext_device_identity_record`).

## Filesystem safeguards (native, Unix)

The native cache path is `<base_dir>/<profile_id>/...`, where `base_dir` for the
desktop daemon is `dirs::data_dir()/Clipper/client` and `profile_id` is the
lowercase hex of `SHA-256(data_key)` (`profile_id_from_encryption_key`). Keying
the profile directory off the data key means each user (each distinct passphrase)
gets a separate cache subtree without the username ever appearing in the path.
The device-identity file is also keyed by `profile_id`
(`device-identity-v1.<profile_id>.json`) because its wrapping key is derived
from that user's OPAQUE export key.

### Directory ownership and mode

Before any write, `ensure_private_dir` (`local_store.rs`):

1. `create_dir_all` the target.
2. `symlink_metadata` (does **not** follow symlinks) and reject the path if it is
   not a directory — a pre-positioned symlink here could otherwise redirect
   secret writes elsewhere.
3. On Unix, reject the directory unless it is owned by the process's effective
   uid (`geteuid`).
4. Force the mode to `0700`.

The profile directory and the base directory are both run through this. The
`restricts_cache_permissions_and_does_not_store_plaintext` test asserts the
profile directory ends up `0700`.

### File mode and durability

The store database file is created with `create_new` and mode `0600` before
SQLite ever opens it. That ordering is deliberate: SQLite copies the main
database's mode onto its `-wal` and `-shm` sidecars, which hold the same
ciphertext until a checkpoint, so creating the database permissively once would
leak through them. The same test asserts `0600` on both the database and its
write-ahead log, and searches the bytes of both for the plaintext
(`"super-secret"`).

The database runs in WAL mode with `synchronous = FULL`, so a committed write
survives an app crash. On macOS it does not set `fullfsync`, so a commit may not
survive a power loss: [`docs/issues.md`](issues.md), entry 78. A record and its
payload, or a delete that removes a payload and rewrites a record, are each one
transaction, so a half-applied state is never stored.

`write_private_file_atomic` writes the one thing that is not a database row, the
device-identity file:

1. Open a uniquely named temp file (`*.<uuid_v7>.tmp`) with `create_new(true)`
   (fails if it already exists) and, on Unix, mode `0600` at open time.
2. Write, flush, and `sync_all`.
3. `rename` the temp file over the final path.

Deletes drop the object row; the payload row is tied to it and goes with it, for
every object kind. What survives is the revision anchor, in its own table, so
dropping cached content is always safe and can never remove an anchor.

## Browser (`wasm`) storage

On `wasm`, there is no filesystem: records, payload ciphertexts, and the device
identity are JSON-serialized into `window.localStorage`. The encryption scheme is
identical (same wrapped device identity, same object ciphertext) — only the
_storage medium_ differs. Important differences from the native path:

- There are **no file modes or ownership checks**; confidentiality relies
  entirely on the browser's same-origin policy and on the fact that the only
  persisted secret is the wrapped signing key.
- Object records are namespaced by a key prefix that includes the profile id
  (`clipper.client.v1.<base_dir>.<profile_id>.…`) and bounded by an index of at
  most `OBJECT_INDEX_LIMIT = 1000` ids.
- The device-identity key
  (`clipper.client.v1.<base_dir>.<profile_id>.device_identity_v1`) is also
  profile-scoped.

### Browser session resume (`sessionStorage`)

So a page reload does not force re-authentication, the standalone web client
keeps a **session-resume blob** in `sessionStorage` under `clipper.session.v2`.
It is written after a successful login/register and read on the next load
(`web/src/backend/index.ts`; `SyncEngine::session_resume_material` /
`SyncEngine::resume_with_platform` in `crates/client/src/engine.rs`). It holds,
all base64-encoded:

- the server **bearer token** (the 30-day session credential),
- the **data key**, and
- the **device-identity wrapping key**.

It deliberately does **not** hold the passphrase or the OPAQUE export key. On
resume the client re-installs the token, confirms it is still live
(`GET /api/auth/validate`), then unwraps the existing on-disk device identity
with the stored wrapping key and re-mounts the engine — no OPAQUE login runs and
no new device is enrolled.

This is a deliberate confidentiality trade-off:

- `sessionStorage` is plaintext and same-origin-script readable, and the data
  key must be present as raw bytes for the wasm XChaCha20-Poly1305 AEAD (a
  non-extractable WebCrypto key cannot back it), so a script running on the
  origin (XSS) could read the blob, which decrypts all object content.
- What the blob is **not**: the passphrase/OPAQUE root. It cannot re-derive the
  passphrase, re-run OPAQUE login, or enroll a new device. The bearer token is
  **server-revocable** (logout or device removal deletes the session row), and
  the blob is wiped when the tab closes. So unlike persisting the passphrase,
  removing the device or rotating the token bounds an attacker's **API
  access**. The limit is that the data key is static (HKDF of the OPAQUE export
  key, never rotated), so revocation cannot claw back ciphertext an attacker
  already recorded (including the `localStorage` ciphertext cache, which
  outlives the tab). Against a malicious server that archives ciphertext,
  content confidentiality past and future is lost until the passphrase can be
  changed: [`docs/issues.md`](issues.md), entry 57.
- Under Tauri the daemon owns the session and survives webview reloads. Mobile
  exports the same resume operations over UniFFI and stores the token, data key,
  identity wrapping key and profile in `clipper.session.v2` in biometric-gated
  SecureStore. It never saves the passphrase. Resume reuses the device identity
  and validates the revocable session with the server. The static data-key limit
  above applies here too.

## The `fs-txn` crate is a different mechanism

`crates/fs-txn/src/lib.rs` (`FsTransaction`) is **not** what `local_store` uses
for atomic writes. The server uses it for staged payload files. The two differ:

- `FsTransaction` writes files to their **final paths immediately** and tracks
  them so that, unless `commit()` is called, `Drop` removes them. Its own doc
  comment is explicit that this is _rollback/cleanup, not isolation_: a
  concurrent reader can observe a half-written file, because there is no
  temp-file-then-rename step.
- `local_store::write_private_file_atomic`, by contrast, _does_ use the
  temp-then-rename pattern and so provides atomic replacement, but it does **not**
  provide rollback across multiple files.

`FsTransaction` is the on-disk analogue of a database transaction's rollback (undo
filesystem side effects on the same error paths that roll back the DB). It
creates each file with `create_new` and, on Unix, opens it with mode `0600`, so
a staged file is never readable by other users.
