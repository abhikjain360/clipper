# Server pepper

`clipper-server` requires a 32-byte secret ("pepper") at startup. It is
used to AEAD-wrap the auth-side blobs that live in the database and to
pepper Argon2id when hashing invite access keys. The root secret must
live **outside** the database — a DB-only leak should not enable offline
brute force.

The pepper is loaded once at startup, then expanded with HKDF-SHA256
into one independent 32-byte subkey per purpose (see `derive_subkey` in
`crates/core/src/crypto.rs` and `ServerSecrets::from_root` in
`crates/server/src/secret.rs`). Call sites take a `ServerSecrets`
reference and reach for the field matching the column they touch.

## What is wrapped at rest

Today exactly four database columns are AEAD-wrapped
(XChaCha20-Poly1305, stored as `nonce_24 ‖ ciphertext_tag`). Each uses
its own HKDF subkey **and** its own AAD string, so a ciphertext cannot be
moved between columns even though every subkey descends from the same
root:

| Column                               | Subkey / AAD               | Scope                         |
| ------------------------------------ | -------------------------- | ----------------------------- |
| `server_config.opaque_server_setup`  | `…opaque-server-setup:v1`  | server-wide                   |
| `server_config.access_key_hash_salt` | `…access-key-hash-salt:v1` | server-wide                   |
| `users.opaque_password_file`         | `…opaque-password-file:v1` | per-user                      |
| `users.encryption_salt`              | `…encryption-salt:v1`      | per-user (legacy placeholder) |

The two per-user columns additionally bind the authenticated `user_id`
into the AAD (`<column-aad> ‖ ":user_id:" ‖ <16 raw UUID bytes>`, see
`user_column_aad` in `crates/server/src/secret_storage.rs`). That means a
wrapped `opaque_password_file` cannot be lifted from one user's row and
replayed into another's; unwrap fails closed if the row's `user_id` does
not match. The two server-wide columns live in the single
`server_config` row (`id = 1`) and use a fixed per-column AAD with no
`user_id`.

`users.encryption_salt` is a **legacy non-null column**. Client
object-encryption keys now derive from OPAQUE's `export_key`, so the
server no longer generates or returns a salt. At registration the column
is filled with a wrapped _empty_ plaintext (`wrap_encryption_salt(…, &[])`
in `crates/server/src/routes/auth.rs`) — i.e. a real AEAD blob whose
payload is zero bytes, not a plaintext zero and not NULL. The wrapper and
subkey are retained only until the column itself is removed from the
schema.

Beyond wrapping, the pepper is also fed to Argon2id as its `secret`
parameter when hashing invite access keys (the `access_key_pepper`
subkey). The on-disk access-key salt
(`server_config.access_key_hash_salt`, itself wrapped above) is combined
with this pepper, so an access-key hash dump is useless without the
pepper.

### Not in this list: the device signing key

The server stores `devices.signing_public_key` in plaintext. That is a
device's **public** verification key, used to check object-envelope and
login-proof signatures, so it is intentionally not secret and not
wrapped. The at-rest encryption of a device's **secret** signing key is a
separate, client-side feature that lives in the client's local store
(`crates/client/src/local_store.rs`, AAD
`clipper:wrap:device-signing-secret:v1`) and has nothing to do with this
server pepper.

The point: an attacker with a database dump (sqlite file, leaked backup,
snapshot exfil) and no pepper cannot brute-force passphrases or access
keys offline, and cannot read the stored OPAQUE state. With the pepper
they degrade back to the ordinary "salt + Argon2" model.

## Generate

```sh
clipper-server generate-secret
```

prints a single line of base64 to stdout (32 random bytes, base64
standard alphabet). Save it somewhere your DB backups do not touch — that
is the entire point. The CLI only prints it; storing it is on you.

## Provide it at startup

`clipper-server` reads exactly one of:

- `CLIPPER_SERVER_SECRET` — the base64 string directly in the env.
- `CLIPPER_SERVER_SECRET_FILE` — path to a file whose trimmed contents
  are the base64 string. Useful with systemd `LoadCredential=`,
  Kubernetes secret files, or any "secret-as-file" mount.

Setting **both** is an error (`BothEnvAndFileSet`). Setting **neither**
is an error (`NotSet`). A value that is not valid base64, or that does
not decode to exactly 32 bytes, is also rejected. `init`,
`add-access-key`, and `serve` all load the pepper and fail closed on any
of these; only `generate-secret` runs without one.

When a database already exists, `init`, `add-access-key`, and `serve`
also verify the supplied pepper can unwrap
`server_config.access_key_hash_salt` (via `load_access_key_hash_salt`); a
wrong pepper surfaces as a clear "server secret cannot decrypt existing
server configuration" error rather than a cryptic auth failure later.
`serve` performs this unwrap **before** binding the TCP listener, so a
wrong or missing pepper fails at startup instead of on the first auth
request. (`init` on a _fresh_, uninitialized database writes the wrapped
config rather than verifying it; the verify path only runs when a config
row already exists.)

## Where to put it

The pepper buys nothing if it leaks together with the database. In order
of preference for self-hosted boxes:

1. `systemd-creds encrypt`, exposed via `LoadCredential=` so the
   plaintext only exists inside the unit's process. Backups of `/etc`
   and `/var` see only the ciphertext.
2. A separate file outside any backup path, mode `0600`, owned by the
   service user. Set `CLIPPER_SERVER_SECRET_FILE` to it.
3. A real KMS / secrets manager if you have one. The codebase is
   structured so a KMS loader can replace `ServerSecrets::load_from_env`
   without touching call sites.

What to avoid: `.env` files committed alongside the DB, env vars set in
shell rc files, anything that gets snapshotted with the database.

## Rotation

There is no in-place rotation yet. The pepper is single-version: the
HKDF labels and AAD strings are all `…:v1`, and no key-id is stored
inside the wrapped blobs. To change it: drop the database, generate a new
pepper, run `init`, re-issue invites. (There is no "dump users, re-wrap"
path — existing rows can only be unwrapped with the original pepper.)

A future revision will add a key-id byte inside the wrapped blob plus an
admin re-wrap command. Until then, treat the pepper as permanent for the
lifetime of the database.

## Loss

If you lose the pepper you lose **all** users — there is no recovery
path. Stored OPAQUE state cannot be unwrapped, clients cannot complete
login, and they cannot rederive their OPAQUE-exported data-key root
through this server. Back the pepper up (separately from the DB), and
document where it lives in your runbook.

## What the pepper is not

- It is **not** a passphrase. Clients do not see it, do not derive
  anything from it, and do not need it.
- It does **not** protect against a live-server compromise. An attacker
  with code execution on the running process has the pepper (and its
  derived subkeys) in memory anyway. OPAQUE is what protects against the
  server seeing plaintext passphrases; the pepper protects only at-rest
  data.
- It does **not** weaken OPAQUE. The protocol bytes on the wire are
  unchanged.
- It is **not** the client's local device-key encryption. That is a
  separate per-device secret managed entirely on the client.
