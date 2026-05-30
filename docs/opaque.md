# OPAQUE in clipper

Math + step-by-step for what the OPAQUE wrappers in
`crates/core/src/crypto.rs` and the auth handlers in
`crates/server/src/routes/auth.rs` do. The variable names below are used
verbatim in code comments in both files.

## Notation

- `G` = Ristretto255, generator `B`, scalars in `Z_q`.
- `H(·)`: hash-to-curve into `G`.
- `KSF(·)`: Argon2id (key-stretching function).
- `Expand(k, info)`: HKDF-Expand-SHA-512.
- `MAC(k, m)`: HMAC-SHA-512.
- `‖`: byte concatenation.
- `·`: scalar multiplication in `G`.
- `r⁻¹`: modular inverse of `r` in `Z_q`.
- `← Z_q` / `← random`: uniform sampling.

Cipher suite: `Ristretto255 + TripleDH + SHA-512 + Argon2id`
(`ClipperOpaqueCipherSuite` in `crates/core/src/crypto.rs`).

## State kept on the server

After registration, the user row carries two opaque blobs.

### `opaque_server_setup` (serialized `ServerSetup`)

```
opaque_server_setup = oprf_seed ‖ sk_S ‖ fake_sk
```

- `oprf_seed`: 64 random bytes.
- `(sk_S, pk_S = sk_S · B)`: server static AKE keypair. `pk_S` is recomputed
  from `sk_S` on deserialize, not stored separately.
- `fake_sk`: random scalar that stock OPAQUE uses to fake a login response
  for a non-existent user (client-enumeration mitigation); never touched by
  registration. **Clipper does not get this mitigation**: because
  `opaque_server_setup` is per-user, `routes::auth::challenge` looks the user
  up first and returns `401` for an unknown username before any OPAQUE step,
  so `fake_sk` is never used on the missing-user path and usernames are
  enumerable. See the deviations section below.

Per-user OPRF key (never stored; recomputed every request):

```
id_U = "clipper:user:{user_id}:passphrase:v1"
k_U  = Expand(oprf_seed, id_U)
```

### `opaque_password_file` (serialized `RegistrationUpload`)

```
opaque_password_file = env ‖ masking_key ‖ pk_C
env                  = env_nonce ‖ auth_tag
```

- `env_nonce`: per-registration random nonce.
- `auth_tag = MAC(auth_key, env_nonce ‖ pk_S)`.
- `masking_key`: derived from the user's `rwd` at registration time;
  used at login to mask `(pk_S ‖ env)` in the response.
- `pk_C`: client static AKE public key. `sk_C` is never stored — it is
  re-derived from `pw` on every login.

## Registration

`pw` = the user's password. Two round-trips between client and server.

### Round 1 — client (`opaque_client_register_start(pw)`)

```
r       ← Z_q
M       = r · H(pw)
state_C = (r, pw, ...)               (kept in memory until finish)
send M                                ⟶ RegistrationRequest
```

### Round 1 — server (`opaque_server_register_start(opaque_server_setup, M, id_U)`)

```
k_U = Expand(oprf_seed, id_U)
N   = k_U · M
send (N, pk_S)                        ⟶ RegistrationResponse
```

Server keeps no per-flow state — the response is deterministic in
`(opaque_server_setup, M, id_U)`.

### Round 2 — client (`opaque_client_register_finish(state_C, pw, RegistrationResponse)`)

```
Y           = r⁻¹ · N                                (= k_U · H(pw), the OPRF output)
rwd         = KSF(Y)                                 (randomized password; Y already binds pw via OPRF)

masking_key = Expand(rwd, "MaskingKey")
export_key  = Expand(rwd, "ExportKey")               (returned to caller; clipper ignores it)

env_nonce   ← random
auth_key    = Expand(rwd, env_nonce ‖ "AuthKey")
sk_C_seed   = Expand(rwd, env_nonce ‖ "PrivateKey")
sk_C        = DeriveKeyPair(sk_C_seed)
pk_C        = sk_C · B
auth_tag    = MAC(auth_key, env_nonce ‖ pk_S)
env         = env_nonce ‖ auth_tag

send (env, masking_key, pk_C)                        ⟶ RegistrationUpload
```

### Round 2 — server (`opaque_server_register_finish(RegistrationUpload)`)

```
opaque_password_file = env ‖ masking_key ‖ pk_C      (persisted on the user row)
```

The server does no cryptographic check here — it only validates byte
layout and writes the blob.

## Login

Pre-conditions: server has `opaque_server_setup` and
`opaque_password_file = (env, masking_key, pk_C)` for `id_U`.

### Round 1 — client (`opaque_client_login_start(pw)`)

```
r       ← Z_q
M       = r · H(pw)
x_C     ← Z_q                        (client AKE ephemeral scalar)
X_C     = x_C · B                    (client AKE ephemeral point)
nonce_C ← random
ke1     = nonce_C ‖ X_C
state_C = (r, pw, x_C, nonce_C, ke1)
send (M ‖ ke1) = (M ‖ nonce_C ‖ X_C) ⟶ CredentialRequest
```

### Round 1 — server (`opaque_server_login_start(opaque_server_setup, opaque_password_file, CredentialRequest, id_U)`)

```
k_U = Expand(oprf_seed, id_U)
N   = k_U · M

(env, masking_key, pk_C) = opaque_password_file

nonce_M ← random                                              (masking nonce)
pad     = Expand(masking_key, nonce_M ‖ "CredentialResponsePad")
masked  = (pk_S ‖ env) XOR pad

x_S     ← Z_q                        (server AKE ephemeral scalar)
X_S     = x_S · B
nonce_S ← random

# 3DH from the server's side:
dh1 = x_S · X_C                      (eph_S · eph_C)
dh2 = x_S · pk_C                     (eph_S · static_C)
dh3 = sk_S · X_C                     (static_S · eph_C)
ikm = dh1 ‖ dh2 ‖ dh3

ke2_pre        = nonce_S ‖ X_S
transcript_pre = id_U ‖ ke1 ‖ N ‖ nonce_M ‖ masked ‖ ke2_pre
(server_mac_key, client_mac_key, session_key) = Expand(ikm, transcript_pre)
server_mac     = MAC(server_mac_key, transcript_pre)

state_S = (client_mac_key, session_key, transcript_pre ‖ server_mac)
send (N, nonce_M, masked, nonce_S, X_S, server_mac)            ⟶ CredentialResponse
```

`state_S` is what `auth.rs` stashes via `state.create_auth_challenge` until
the client returns its finalization.

### Round 2 — client (`opaque_client_login_finish(state_C, pw, CredentialResponse)`)

```
Y            = r⁻¹ · N
rwd          = KSF(Y)

masking_key  = Expand(rwd, "MaskingKey")
pad          = Expand(masking_key, nonce_M ‖ "CredentialResponsePad")
(pk_S ‖ env) = masked XOR pad
env_nonce, auth_tag = env

auth_key     = Expand(rwd, env_nonce ‖ "AuthKey")
sk_C_seed    = Expand(rwd, env_nonce ‖ "PrivateKey")
sk_C         = DeriveKeyPair(sk_C_seed)
pk_C         = sk_C · B
check  auth_tag = MAC(auth_key, env_nonce ‖ pk_S)              else abort  (wrong pw)

# 3DH from the client's side (same shared values as server):
dh1 = x_C · X_S                      (eph_C · eph_S    = server's dh1)
dh2 = sk_C · X_S                     (static_C · eph_S = server's dh2)
dh3 = x_C · pk_S                     (eph_C · static_S = server's dh3)
ikm = dh1 ‖ dh2 ‖ dh3

transcript_pre = id_U ‖ ke1 ‖ N ‖ nonce_M ‖ masked ‖ nonce_S ‖ X_S
(server_mac_key, client_mac_key, session_key) = Expand(ikm, transcript_pre)
check  server_mac = MAC(server_mac_key, transcript_pre)        else abort  (bad server)

client_mac = MAC(client_mac_key, transcript_pre ‖ server_mac)
send client_mac                                                ⟶ CredentialFinalization
output session_key
```

### Round 2 — server (`opaque_server_login_finish(state_S, CredentialFinalization)`)

```
expected_mac = MAC(client_mac_key, transcript_pre ‖ server_mac)
check  client_mac = expected_mac                               else abort  (wrong pw)
output session_key
```

## Clipper deviations from a "stock" OPAQUE deployment

- `opaque_server_setup` is **per user**, not global: each user has
  independent `oprf_seed`, `sk_S`, `fake_sk`. A consequence is that the
  standard OPAQUE enumeration defense (a global setup that fabricates a
  response for unknown users via `fake_sk`) does not apply here — the server
  must look up the per-user setup first, so unknown usernames are
  distinguishable. Username enumeration is therefore an accepted property of
  this design, not something `fake_sk` mitigates.
- `session_key` is discarded after login; clipper authenticates the session
  with a fresh random bearer token issued by `issue_session` after
  `opaque_server_login_finish` succeeds.
- `export_key` is unused. Clipper derives its local data-encryption key
  separately via Argon2id over `pw` and a server-stored
  `encryption_salt`; that salt rides in the same response payload but has
  no relation to OPAQUE.
- Access keys are an invite-gate that lives entirely outside OPAQUE.

## At-rest wrapping (server pepper)

OPAQUE protects against the server ever seeing `pw`, but a DB dump
still leaks `opaque_server_setup` and `opaque_password_file`, and that
is enough for offline brute force (compute `k_U`, compute `Y`, derive
`rwd`, check `auth_tag`). Clipper closes that gap by AEAD-wrapping the
at-rest blobs with subkeys derived from a server-managed root pepper.
The OPAQUE protocol itself runs on the unwrapped bytes — the wrap is
purely a storage-boundary concern. See `docs/server-secret.md` for ops.

Wrapped on disk:

- `users.opaque_server_setup`
- `users.opaque_password_file`
- `users.encryption_salt`
- `server_config.access_key_hash_salt`

Access keys:

- Argon2id for `access_keys.key_hash` is called with the server pepper
  passed as Argon2's `secret` ("pepper") parameter, so DB-only attackers
  cannot verify candidate access keys offline.

Plaintext on disk:

- `sessions.token_hash` — 32-byte random tokens; not brute-forceable.
- `objects.meta_ciphertext` and `object_payloads/*.bin` — already
  client-encrypted with a key bound to the wrapped `encryption_salt`.

The client receives the plaintext `encryption_salt` in `ServerInfo`
during registration, login, and `/api/sync/bootstrap`; only the
database column is wrapped.

Wrap format and key derivation live in `clipper_core::crypto`
(`wrap_with_key`, `derive_subkey`, `HKDF_LABEL_*`, `AAD_WRAP_*`); the
column-to-subkey/AAD bindings live in
`crates/server/src/secret_storage.rs`.
The server unwraps these columns only at storage boundaries: before
calling OPAQUE helpers, before hashing/checking access keys, before
returning `ServerInfo`, and during startup validation of the stored
`server_config.access_key_hash_salt`.
