# Server pepper

`clipper-server` requires a 32-byte secret ("pepper") at startup. It is
used to AEAD-wrap the auth blobs that live in the database
(`opaque_server_setup`, `opaque_password_file`, `encryption_salt`,
`access_key_hash_salt`) and to pepper Argon2id when hashing invite
access keys.

The point: an attacker with a database dump (sqlite file, leaked
backup, snapshot exfil) and no pepper cannot brute-force passphrases
or access keys offline. With the pepper they degrade back to the
ordinary "salt + Argon2" model.

## Generate

```sh
clipper-server generate-secret
```

prints a single line of base64 to stdout. Save it somewhere your DB
backups do not touch — that is the entire point.

## Provide it at startup

`clipper-server` reads exactly one of:

- `CLIPPER_SERVER_SECRET` — the base64 string directly in the env.
- `CLIPPER_SERVER_SECRET_FILE` — path to a file whose trimmed contents
  are the base64 string. Useful with systemd `LoadCredential=`,
  Kubernetes secret files, or any "secret-as-file" mount.

Setting both is an error. Setting neither is an error: `init`,
`add-access-key`, and `serve` all fail closed.

When a database already exists, `init`, `add-access-key`, and `serve`
also verify that the supplied pepper can unwrap
`server_config.access_key_hash_salt`. `serve` performs this check before
binding the HTTP listener, so a wrong pepper fails at startup instead of
on the first auth request.

## Where to put it

The pepper buys nothing if it leaks together with the database. In
order of preference for self-hosted boxes:

1. `systemd-creds encrypt`, exposed via `LoadCredential=` so the
   plaintext only exists inside the unit's process. Backups of `/etc`
   and `/var` see only the ciphertext.
2. A separate file outside any backup path, mode `0600`, owned by the
   service user. Set `CLIPPER_SERVER_SECRET_FILE` to it.
3. A real KMS / secrets manager if you have one. The codebase is
   structured so a KMS loader can replace `ServerSecrets::load_from_env`
   without touching call sites.

What to avoid: `.env` files committed alongside the DB, env vars set
in shell rc files, anything that gets snapshotted with the database.

## Rotation

There is no in-place rotation yet. The pepper is single-version. To
change it: dump no users (the only safe time), drop the database,
generate a new pepper, run `init`, re-issue invites.

A future revision will add a key-id byte inside the wrapped blob plus
an admin re-wrap command. Until then, treat the pepper as permanent
for the lifetime of the database.

## Loss

If you lose the pepper you lose **all** users — there is no recovery
path. Stored OPAQUE state cannot be unwrapped, `encryption_salt`
cannot be unwrapped, and clients can no longer derive their data
keys. Back the pepper up (separately from the DB), and document where
it lives in your runbook.

## What the pepper is not

- It is **not** a passphrase. Clients do not see it, do not derive
  anything from it, and do not need it.
- It does **not** protect against a live-server compromise. An
  attacker with code execution on the running process has the pepper
  in memory anyway. OPAQUE is what protects against the server seeing
  plaintext passphrases; the pepper protects only at-rest data.
- It does **not** weaken OPAQUE. The protocol bytes on the wire are
  unchanged.
