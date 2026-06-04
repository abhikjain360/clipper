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

The `server_config` row carries one server-wide OPAQUE setup blob. After
registration, each user row carries its own OPAQUE password file.

### `opaque_server_setup` (serialized `ServerSetup`)

```
opaque_server_setup = oprf_seed ‖ sk_S ‖ fake_sk
```

- `oprf_seed`: 64 random bytes.
- `(sk_S, pk_S = sk_S · B)`: server static AKE keypair. `pk_S` is recomputed
  from `sk_S` on deserialize, not stored separately.
- `fake_sk`: random scalar that stock OPAQUE uses to fake a login response
  for a non-existent user (client-enumeration mitigation). Clipper uses this
  path by passing no password file into `opaque_server_login_start` when a
  username is missing, so `challenge` still returns a normal-looking response.

Per-credential OPRF key (never stored; recomputed every request):

```
id_U = "clipper:user:{username}:passphrase:v1"
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
export_key  = Expand(rwd, "ExportKey")               (client data-key root)

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
output session_key, export_key
```

### Round 2 — server (`opaque_server_login_finish(state_S, CredentialFinalization)`)

```
expected_mac = MAC(client_mac_key, transcript_pre ‖ server_mac)
check  client_mac = expected_mac                               else abort  (wrong pw)
output session_key
```

### Device proof-of-possession

OPAQUE proves knowledge of the account passphrase. It does not prove that the
client presenting an existing `device_id` still holds that device's signing key,
so Clipper layers a separate Ed25519 proof onto the same login challenge.

When issuing `LoginChallengeResponse`, the server samples:

```
η_D ← random 32 bytes
```

and stores `η_D` next to `state_S` under the same `challenge_id`. The response
includes `η_D` as `device_proof_challenge` for every username, including the
fabricated unknown-user path, so the response shape does not reveal whether the
account exists.

If the client is reusing a known local device identity `(device_id, sk_D)`, it
derives:

```
pk_D = Public(sk_D)
π_D  = (
  version = 1,
  challenge_id,
  η_D,
  username,
  device_id,
  pk_D
)
σ_D = Sign(sk_D, Canon(π_D))
```

and sends `(device_id, pk_D, σ_D)` in `LoginRequest`. New devices omit `σ_D`
because there is no existing server-side device key to prove yet.

After OPAQUE finalization succeeds and before issuing a bearer session, the
server checks existing-device reuse:

```
lookup devices[device_id] = (user_id, pk_D_stored)
check user_id == authenticated user_id
check pk_D == pk_D_stored
check Verify(pk_D_stored, Canon(π_D), σ_D)
```

Only then does the server update `devices.last_seen_at` and create a session.
This makes sessions and audit state reflect actual possession of the device key.
It does not protect against intentional key copying: anyone with `sk_D` can
produce both this login proof and object-envelope signatures.

## Clipper deviations from a "stock" OPAQUE deployment

- `opaque_server_setup` is server-wide and lives in `server_config`, not on
  each user row. This lets `challenge` use OPAQUE's fake-record path for
  unknown usernames while still deriving real user credentials from the same
  global setup.
- `session_key` is discarded after login; clipper authenticates the session
  with a fresh random bearer token issued by `issue_session` after
  `opaque_server_login_finish` succeeds.
- `export_key` is used only on the client. Clipper derives its local
  data-encryption key as `HKDF-SHA256(export_key,
"clipper:opaque-export:data-key:v1")`; the server never receives this
  key and no longer returns a separate encryption salt/KDF parameter set.
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

- `server_config.opaque_server_setup`
- `users.opaque_password_file`
- `users.encryption_salt` (legacy non-null placeholder until a schema
  migration removes it)
- `server_config.access_key_hash_salt`

Access keys:

- Argon2id for `access_keys.key_hash` is called with the server pepper
  passed as Argon2's `secret` ("pepper") parameter, so DB-only attackers
  cannot verify candidate access keys offline.

Plaintext on disk:

- `sessions.token_hash` — 32-byte random tokens; not brute-forceable.
- `objects.meta_ciphertext` and `object_payloads/*.bin` — already
  client-encrypted with a key derived from the OPAQUE `export_key`.

Wrap format and key derivation live in `clipper_core::crypto`
(`wrap_with_key`, `derive_subkey`, `HKDF_LABEL_*`, `AAD_WRAP_*`); the
column-to-subkey/AAD bindings live in
`crates/server/src/secret_storage.rs`.
The server unwraps these columns only at storage boundaries: before
calling OPAQUE helpers, before hashing/checking access keys, and during
startup validation of the stored `server_config.access_key_hash_salt`.
