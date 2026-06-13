# Local At-Rest Encryption

Clipper is end-to-end encrypted: clipboard and file payloads are encrypted on
the client before they reach the server, and the server only ever stores
ciphertext (see `docs/object-envelopes.md`, `docs/opaque.md`). This document
covers the *client* side of at-rest protection: what the local cache and the
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
- `crates/fs-txn/src/lib.rs` — a separate rollback guard for staged file writes
  (used elsewhere; see the caveat below).

## Key material and where it comes from

All client-side keys derive from a single root: the **OPAQUE export key**
returned by `opaque_client_login_finish` / `opaque_client_register_finish`. The
export key is stable for a given `(passphrase, server registration)` pair across
logins, and it never leaves the client. From it the client derives two
independent 32-byte keys with HKDF-SHA256, using distinct domain-separation
labels (`crates/core/src/crypto.rs`):

| Key | Derivation | Label | Purpose |
| --- | --- | --- | --- |
| **Data key** | `derive_data_key_from_opaque_export_key` | `clipper:opaque-export:data-key:v1` | Encrypts/decrypts all object material (clipboard meta, clipboard payloads, file meta, file blobs). This is the E2EE key shared with the server-stored ciphertext. |
| **Device-identity wrapping key** | `derive_device_identity_wrapping_key_from_opaque_export_key` | `clipper:opaque-export:device-identity-wrap-key:v1` | Wraps the persisted device signing secret at rest. |

Crucially, **neither key is persisted anywhere** — not on disk, not in the OS
keychain, not in `localStorage`. Both are re-derived from the OPAQUE export key
on every login or registration (`ApiClient::login_prepare` /
`register_prepare`), held only in memory inside `SyncEngine`
(`encryption_key` and, indirectly via the loaded signing key, the wrapping key),
and dropped on logout. They live inside `Zeroizing` wrappers so per-call copies
are wiped on drop.

This means a cold attacker who reads the on-disk cache (or `localStorage`) but
does not know the passphrase cannot decrypt anything: the only persisted secret
is the *wrapped* device signing key, and unwrapping it requires the
OPAQUE-derived wrapping key, which is gone once the process exits.

### What the OS keychain stores (and does not)

For the desktop daemon, the platform credential store
(`crates/daemon/src/keychain.rs`) holds only two things:

- The 32-byte **IPC secret** (`ipc-secret-v1`) used for the local daemon/UI HMAC
  handshake — unrelated to data encryption. See `docs/local-ipc-security.md`.
- A `Credentials` record: `{ device_name, server_url, username }`.

The passphrase and all encryption/wrapping/signing key material are
**intentionally not persisted** (`crates/daemon/src/main.rs`: "The passphrase is
intentionally not persisted, so the daemon waits for the app to provide it after
startup"). The daemon cannot decrypt the local cache on its own after a restart;
it waits for the UI to re-supply the passphrase, which re-derives the keys via
OPAQUE login. Legacy `Credentials` JSON that still carries a `passphrase` field
is migrated by re-storing the record without it on the next load.

## What is encrypted at rest

### Cached objects (clipboard and files)

The local cache stores, per object, a `StoredObjectRecord` and (for clipboard
objects) a separate payload-ciphertext blob. The persisted record never contains
plaintext. Specifically, a `Present` record holds an `EncryptedObject`:

- `meta_nonce` + `meta_ciphertext` — the object metadata (clipboard MIME type and
  size, or filename/MIME/size for files), AEAD-encrypted under the data key.
- `payloads` — `ObjectPayloadDescriptor`s carrying each payload's nonce,
  ciphertext size, and SHA-256 of the ciphertext.
- `created_at`, `source_device_id`, and the signed `ObjectEnvelopeV1`.

For clipboard objects, the actual payload ciphertext is written to a separate
file (`<object_id>.payload.ciphertext`) / `localStorage` key, not inlined into
the JSON record. File-object blobs are not cached locally at all; they are
downloaded and decrypted on demand (`SyncEngine::download_file_bytes`).

This ciphertext is byte-for-byte the same XChaCha20-Poly1305 ciphertext the
client uploaded to the server: the local cache reuses the E2EE ciphertext rather
than re-encrypting under a separate local key. Encryption/decryption uses
`crypto::encrypt` / `crypto::decrypt` (XChaCha20-Poly1305, 24-byte random nonce)
with the per-object envelope body bound in as AAD
(`object_meta_aad_v1` / `object_payload_aad_v1`), so a ciphertext cannot be
moved between objects, payload slots, meta-vs-payload roles, or fields without
failing authentication.

On hydration (`hydrate_ciphertext_cache`) the cache decrypts each record with the
in-memory data key. A record that fails to decrypt or fails its integrity checks
is logged and removed rather than surfaced. Before a cached clipboard payload is
decrypted, `verify_payload_ciphertext` re-checks its length and SHA-256 against
the descriptor, so a tampered or truncated ciphertext file is rejected.

#### Plaintext that is *not* persisted

Decrypted display state is kept only in memory (`MemoryState`). The only
plaintext-derived value retained is a **bounded clipboard preview**
(`CLIPBOARD_TEXT_PREVIEW_MAX_CHARS = 512` characters for text MIME types, or a
`"<mime> clipboard payload (N bytes)"` label for non-text). The preview is
recomputed from decrypted bytes on hydration — it is never written to the
on-disk record, and the caller-supplied preview text is *not* trusted (the
`derives_bounded_preview_without_trusting_caller_text` test asserts this). Full
payload bytes are only ever decrypted transiently for the operation that needs
them (e.g. `clipboard_payload`, copy-to-clipboard, file download).

### Device signing identity

Each client device has an Ed25519 signing secret used to sign object envelopes
and login proofs (`docs/object-envelopes.md`). It is persisted across restarts
so the device keeps a stable identity. It is stored **wrapped**, in a
`device-identity-v1.json` file under the cache base directory (native) or under a
`localStorage` key (web).

The current on-disk record is `DeviceIdentityEncryptedRecord`:

- `version` — `DEVICE_IDENTITY_RECORD_VERSION_V2` (2); other versions are
  rejected with `UnsupportedDeviceIdentityVersion`.
- `device_id` — the server-assigned device UUID, **stored in cleartext**
  (see "Flagged gaps" below).
- `wrapped_signing_secret_key` — the signing secret sealed with
  `crypto::wrap_with_key(wrapping_key, secret, AAD_WRAP_DEVICE_SIGNING_SECRET_V1)`,
  i.e. `nonce_24 ‖ XChaCha20-Poly1305(secret)` with AAD
  `clipper:wrap:device-signing-secret:v1`.

Unwrapping requires the device-identity wrapping key; a wrong key fails with
`DeviceIdentityDecrypt` (asserted by `encrypts_device_identity_at_rest`). The
in-memory `signing_secret_key` is held in `Zeroizing`.

#### Plaintext-record migration

An older record format (`DeviceIdentityPlaintextRecord`, an unversioned
`{ device_id?, signing_secret_key }` with the secret in the clear) is still
*read* for backward compatibility. When such a record is loaded, the identity is
recovered and immediately **re-written in the encrypted v2 format**
(`migrates_plaintext_device_identity_to_encrypted_record`). This is a one-way
upgrade; the legacy plaintext layout is never written by current code.

## Filesystem safeguards (native, Unix)

The native cache path is `<base_dir>/<profile_id>/...`, where `base_dir` for the
desktop daemon is `dirs::data_dir()/Clipper/client` and `profile_id` is the
lowercase hex of `SHA-256(data_key)` (`profile_id_from_encryption_key`). Keying
the profile directory off the data key means each user (each distinct passphrase)
gets a separate cache subtree without the username ever appearing in the path.
The device-identity file lives directly under `base_dir` (not under a profile),
because the signing identity is per-device, not per-user.

### Directory ownership and mode

Before any write, `ensure_private_dir` (`local_store.rs`):

1. `create_dir_all` the target.
2. `symlink_metadata` (does **not** follow symlinks) and reject the path if it is
   not a directory — a pre-positioned symlink here could otherwise redirect
   secret writes elsewhere.
3. On Unix, reject the directory unless it is owned by the process's effective
   uid (`geteuid`).
4. Force the mode to `0700`.

The object directory, clipboard directory, and base directory are all run
through this. The
`restricts_cache_permissions_and_does_not_store_plaintext` test asserts both
directories end up `0700`.

### File mode and atomic replacement

`write_private_file_atomic` writes records, payload ciphertext, and the
device-identity file:

1. Open a uniquely named temp file (`*.<uuid_v7>.tmp`) with `create_new(true)`
   (fails if it already exists).
2. On Unix, `set_permissions(0o600)` on the temp file.
3. Write and flush.
4. `rename` the temp file over the final path.

The `rename` makes the *replacement* of an existing record atomic — a reader sees
either the old or the new file, never a partial write. The same test asserts the
object record and the payload-ciphertext file are both `0600`, and that the
on-disk record contains neither the plaintext (`"super-secret"`), nor a `"text"`
field, nor an inlined `payload_ciphertext`.

Deletes (`remove_payloads_for_object`, `remove_stored_object_record_and_payloads`)
unlink the record and any payload sidecar files (including legacy `.payload` /
`.txt` names), and drop the in-memory record.

## Browser (`wasm`) storage

On `wasm`, there is no filesystem: records, payload ciphertexts, and the device
identity are JSON-serialized into `window.localStorage`. The encryption scheme is
identical (same wrapped device identity, same object ciphertext) — only the
*storage medium* differs. Important differences from the native path:

- There are **no file modes or ownership checks**; confidentiality relies
  entirely on the browser's same-origin policy and on the fact that the only
  persisted secret is the wrapped signing key.
- Object records are namespaced by a key prefix that includes the profile id
  (`clipper.client.v1.<base_dir>.<profile_id>.…`) and bounded by an index of at
  most `OBJECT_INDEX_LIMIT = 1000` ids.
- The device-identity key (`clipper.client.v1.<base_dir>.device_identity_v1`) is
  **not** profile-scoped, consistent with the native layout (one device identity
  per origin/base, shared across users).

## The `fs-txn` crate is a different mechanism

`crates/fs-txn/src/lib.rs` (`FsTransaction`) is **not** what `local_store` uses
for atomic writes, and it is worth not conflating the two:

- `FsTransaction` writes files to their **final paths immediately** and tracks
  them so that, unless `commit()` is called, `Drop` removes them. Its own doc
  comment is explicit that this is *rollback/cleanup, not isolation*: a
  concurrent reader can observe a half-written file, because there is no
  temp-file-then-rename step.
- `local_store::write_private_file_atomic`, by contrast, *does* use the
  temp-then-rename pattern and so provides atomic replacement, but it does **not**
  provide rollback across multiple files.

`FsTransaction` is the on-disk analogue of a database transaction's rollback (undo
filesystem side effects on the same error paths that roll back the DB). It also
sets `0600` on the files it creates, via the same `create_new` + `set_permissions`
sequence.

## Flagged gaps

These are factual descriptions of current behavior that may warrant a closer
security look; they are reported in the discrepancy list accompanying this
document.

- **`device_id` is stored in cleartext** in the device-identity record (both the
  wrapped native record and the `localStorage` record). Only the signing secret
  is wrapped. This is a low-sensitivity identifier, but it is metadata that is
  not protected at rest.
- **`set_permissions(0o600)` happens after the file is created**, not via the
  open mode, in both `write_private_file_atomic` and `FsTransaction`. There is a
  brief window in which the temp file exists with default-umask permissions
  before the `chmod`. On the native cache this is mitigated by the enclosing
  directory being forced to `0700` (and uid-checked), so the window is not
  reachable by other users in the normal layout; it is noted here for
  completeness.
</content>
</invoke>
