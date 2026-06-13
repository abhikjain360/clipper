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

`id_U` is built by `opaque_credential_identifier(username)` in `auth.rs` and is
derivable from the submitted username alone, so the fake-record path stays
stable across probes for the same username.

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

`pw` = the user's passphrase. Two round-trips between client and server, gated
by a one-time access key (see "Access keys" below). All four crypto steps are
also available in-process for tests via `crypto::opaque_register`.

### Anti-enumeration at registration

Registration does **not** reveal whether a username is already taken at
`register/start`. Username uniqueness is enforced only at `register/finish`, by
the `users.username` unique constraint. `register_start` returns the same
`RegisterStartResponse` shape (a fresh `registration_id`, a fresh `user_id`, and
an OPAQUE `registration_response`) whether or not the username already exists —
there is no `409` on a duplicate username at this step.

Consequences worth understanding before reading the steps below:

- The access key is **verified** (not consumed) in `register_start`, and
  **consumed** (`used_at` set) in `register_finish`. Consumption happens before
  the `users` row insert, so it runs *before* the username uniqueness check.
- Because consumption precedes the insert, a `register_finish` against an
  already-taken username burns the one-time access key and then returns
  `409 CONFLICT ("Registration conflict")`. The consumed key is committed but
  left **unbound** to any user (`used_by_user_id = NULL`); it cannot be reused.
  See `register_start_does_not_reveal_existing_username` in the auth tests.
- So while `register/start` is non-revealing, the full two-step flow still lets a
  caller who holds a valid unused access key learn — at `register/finish`, and
  only by spending that key — whether a chosen username exists. This is flagged
  in `docs/security-review-2026-06-04.md` rather than blessed here.

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

Server keeps no per-flow *crypto* state — the response is deterministic in
`(opaque_server_setup, M, id_U)`. THIS server does, however, stash a
`PendingRegistration { user_id, username, access_key_hash }` in memory (keyed by
a `registration_id`) so `register_finish` can recover the freshly minted
`user_id` and the access-key hash to consume. The access key is verified here
(`verify_unused_registration_access_key`) but not yet consumed.

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
layout and writes the blob. Before storage it is AEAD-wrapped (see "At-rest
wrapping"), the access key is consumed, the `users` row is inserted (subject to
the username unique constraint), the consumed key is bound to the new
`user_id`, and an initial session/device is issued.

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

### Round 1 — server (`challenge` → `opaque_server_login_start(opaque_server_setup, opaque_password_file, CredentialRequest, id_U)`)

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
the client returns its finalization. KSF/Argon2id is **not** run server-side
during login — `rwd = KSF(Y)` runs only on the client in round 2. The server's
login-start work is the OPRF eval, the masking XOR, and the 3DH; an offline
brute-force of the password file is therefore the relevant cost, which is why
the password file is AEAD-wrapped at rest (see below).

#### Unknown-user (fake-record) path

A missing user does **not** short-circuit. `challenge` looks the user up by
username; if there is no row it passes `password_file = None` into
`opaque_server_login_start`, which selects opaque-ke's fake-record path. That
fabricates a `CredentialResponse` indistinguishable in shape from a real one and
stable for a given `id_U`, and the stashed `AuthChallenge.user_id` is `None`.
The fabricated challenge simply fails at finalization in `login` (a verifying
finalization can never be produced without the real password file), exactly as a
wrong passphrase against a real account would. Tests:
`challenge_for_unknown_user_is_indistinguishable`,
`login_rejects_fabricated_challenge`.

Caveat (not a documented guarantee): the response *shape* is indistinguishable,
but the real path performs one extra AEAD unwrap of the stored
`opaque_password_file` that the fake path skips, so the two paths are not
guaranteed to be wall-clock identical. The anti-enumeration property the tests
assert is shape/observable-failure equivalence, not constant-time equivalence.

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

### Round 2 — server (`login` → `opaque_server_login_finish(state_S, CredentialFinalization)`)

```
expected_mac = MAC(client_mac_key, transcript_pre ‖ server_mac)
check  client_mac = expected_mac                               else abort  (wrong pw)
output session_key
```

`login` first pops `state_S` from `create_auth_challenge`'s store (single-use:
the challenge is removed on take, so a captured finalization cannot be replayed
— `login_challenge_is_single_use`), verifies the finalization, then requires the
stashed `AuthChallenge.user_id` to be present. A fabricated challenge has
`user_id = None`, so even an attacker-supplied finalization cannot be turned into
a session. `session_key` is discarded; the session is authenticated by a fresh
random bearer token (see "Sessions and bearer tokens").

### Device proof-of-possession

OPAQUE proves knowledge of the account passphrase. It does not prove that the
client presenting an existing `device_id` still holds that device's signing key,
so Clipper layers a separate Ed25519 proof onto the same login challenge.

When issuing `LoginChallengeResponse`, the server samples
`DEVICE_LOGIN_PROOF_CHALLENGE_BYTES` (32) random bytes:

```
η_D ← random 32 bytes
```

and stores `η_D` next to `state_S` (as `AuthChallenge.device_proof_challenge`)
under the same `challenge_id`. The response includes `η_D` as
`device_proof_challenge` for every username, including the fabricated
unknown-user path, so the response shape does not reveal whether the account
exists.

If the client is reusing a known local device identity `(device_id, sk_D)`, it
derives:

```
pk_D = Public(sk_D)
π_D  = DeviceLoginProofBodyV1 {
  version = DEVICE_LOGIN_PROOF_VERSION (1),
  challenge_id,
  challenge = η_D,
  username,
  device_id,
  device_signing_public_key = pk_D
}
σ_D = Sign(sk_D, Canon(π_D))         (Canon = postcard encoding of the body)
```

and sends `(device_id, pk_D, σ_D)` in `LoginRequest`. New devices omit `σ_D`
because there is no existing server-side device key to prove yet.

After OPAQUE finalization succeeds and before issuing a bearer session, the
server checks existing-device reuse in `issue_session` /
`verify_existing_device_login_proof`:

```
lookup devices[device_id] = (user_id, pk_D_stored)
check user_id == authenticated user_id                          else 409
check pk_D == pk_D_stored                                        else 409
require this is a Login (not a Registration) session            else 401
require σ_D present and η_D length == 32                         else 401
check Verify(pk_D_stored, Canon(π_D), σ_D)                       else 401
```

Only then does the server update `devices.last_seen_at` and create a session.
Registration always allocates a fresh device (a registration session that tries
to reuse an existing `device_id` is rejected with "Device proof required"). This
makes sessions and audit state reflect actual possession of the device key.
It does not protect against intentional key copying: anyone with `sk_D` can
produce both this login proof and object-envelope signatures.

## Sessions and bearer tokens

A successful `register_finish` or `login` calls `issue_session`, which:

- Resolves or inserts the `devices` row (subject to the device checks above).
- Generates a random session token of `config.crypto.session_token_bytes` bytes
  (default 32; valid range 32–64 per `MIN/MAX_SESSION_TOKEN_BYTES`), base64s it
  for the client, and stores **only** its SHA-256 hash in `sessions.token_hash`.
  The raw token is never persisted.
- Inserts a `sessions` row with `expires_at = now + 30 days` (hardcoded in
  `issue_session`; not configurable) and returns `{ token, device_id }`.

`auth_middleware` authenticates subsequent requests: it base64-decodes the
`Authorization: Bearer …` token, SHA-256-hashes it, looks the session up by
`token_hash`, rejects if `expires_at < now`, and injects
`AuthInfo { session_id, user_id, device_id }`. `last_seen_at` is refreshed at
most once per `LAST_SEEN_REFRESH_SECS` (60 s) to avoid a write per request.
`logout` deletes the current session row.

There is no token rotation/refresh: a token is valid until it expires (30 days)
or its session is deleted (logout, or cascade on device/user delete).

## Clipper deviations from a "stock" OPAQUE deployment

- `opaque_server_setup` is server-wide and lives in `server_config`, not on
  each user row. This lets `challenge` use OPAQUE's fake-record path for
  unknown usernames while still deriving real user credentials from the same
  global setup.
- `session_key` is discarded after login; clipper authenticates the session
  with a fresh random bearer token issued by `issue_session` after
  `opaque_server_login_finish` succeeds.
- `export_key` is used only on the client and never sent to the server. Clipper
  derives two client-side keys from it via HKDF-SHA256:
  - data-encryption key:
    `HKDF-SHA256(export_key, "clipper:opaque-export:data-key:v1")`
    (`derive_data_key_from_opaque_export_key`), used to encrypt object metadata
    and payloads.
  - device-identity wrapping key:
    `HKDF-SHA256(export_key, "clipper:opaque-export:device-identity-wrap-key:v1")`
    (`derive_device_identity_wrapping_key_from_opaque_export_key`), used to
    AEAD-wrap the persisted device signing secret at rest on the client (see
    `crates/client/src/local_store.rs`, `AAD_WRAP_DEVICE_SIGNING_SECRET_V1`).

  The server never receives either key and no longer returns a separate
  encryption salt / KDF parameter set.
- Access keys are an invite-gate that lives entirely outside OPAQUE.

## Access keys

Access keys are one-time registration invites. They are stored only as hashes
(`access_keys.key_hash`, the table primary key), never in cleartext.

- `key_hash = Argon2id(access_key, salt = access_key_hash_salt,
  secret = access_key_pepper, params = config.crypto.access_key_hash_params)`,
  base64-encoded (`server_auth::hash_access_key` →
  `crypto::access_key_hash_with_params`). Default params are
  `m_cost = 19 MiB, t_cost = 2, p_cost = 1`, output 32 bytes.
- `access_key_hash_salt` is server-wide (`server_config.access_key_hash_salt`),
  AEAD-wrapped at rest.
- `secret` is the Argon2 "pepper": `ServerSecrets::access_key_pepper`, a
  dedicated HKDF-SHA256 subkey of the root pepper
  (`HKDF_LABEL_ACCESS_KEY_PEPPER_V1`). It is a distinct subkey from the at-rest
  wrapping keys but derived from the same root secret. Because the salt is
  wrapped and Argon2 mixes in the pepper, a DB-only attacker cannot verify
  candidate access keys offline.
- Argon2id is memory-hard and blocks for tens of milliseconds, so
  `register_start` runs it on the blocking pool (`spawn_blocking`) rather than
  stalling an async worker.

Lifecycle: `register_start` rejects (401 "Invalid access key") if the key is
unknown, already used (`used_at` set), or expired (`expires_at <= now`).
`register_finish` re-checks the same conditions and then consumes the key with a
guarded `UPDATE … SET used_at WHERE used_at IS NULL` (the `rows_affected != 1`
race guard rejects with 409). After the user row inserts successfully, a second
guarded update binds `used_by_user_id` to the new user. As noted above, a
duplicate-username `register_finish` consumes the key but leaves it unbound.

## Rate limiting around the OPAQUE endpoints

The auth routes (`register/start`, `register/finish`, `challenge`, `login`) sit
behind `auth_rate_limit_middleware`, which enforces a per-client-IP bucket and a
global bucket before the handler runs. The per-client middleware cannot see the
username (it is inside the encrypted/serialized body), so `challenge`
additionally enforces a per-username budget (`check_auth_username`) right after
parsing the body and before any DB or OPAQUE work — this is the backstop against
distributed guessing that rotates source addresses. Defaults:
`auth_per_client_per_minute = 10`, `auth_per_username_per_minute = 30`,
`auth_global_per_minute = 3000`. Tests:
`login_rate_limits_by_client_ip`,
`challenge_rate_limits_opaque_password_attempts_by_client_ip`,
`challenge_rate_limits_by_username_across_client_ips`.

## At-rest wrapping (server pepper)

OPAQUE protects against the server ever seeing `pw`, but a DB dump
still leaks `opaque_server_setup` and `opaque_password_file`, and that
is enough for offline brute force (compute `k_U`, compute `Y`, derive
`rwd`, check `auth_tag`). Clipper closes that gap by AEAD-wrapping the
at-rest blobs with subkeys derived from a server-managed root pepper.
The OPAQUE protocol itself runs on the unwrapped bytes — the wrap is
purely a storage-boundary concern. See `docs/server-secret.md` for ops.

The root pepper is loaded once at startup from `CLIPPER_SERVER_SECRET`
(base64) or `CLIPPER_SERVER_SECRET_FILE`, must decode to exactly 32 bytes, and
is expanded into per-purpose subkeys by `ServerSecrets::from_root` via
HKDF-SHA256. A DB-only leak (without the root pepper) defeats none of these.

Wrapped on disk (each column bound to its own HKDF subkey + AAD label so a
ciphertext cannot be moved between columns):

- `server_config.opaque_server_setup`
- `users.opaque_password_file` (AAD also binds the `user_id`)
- `users.encryption_salt` (legacy non-null column; now wrapped over an **empty**
  byte string as a placeholder until a schema migration removes it — new clients
  derive object-encryption keys from OPAQUE's `export_key`, so the server no
  longer generates or returns a salt)
- `server_config.access_key_hash_salt`

Access keys:

- Argon2id for `access_keys.key_hash` is called with the access-key pepper
  subkey passed as Argon2's `secret` ("pepper") parameter, so DB-only attackers
  cannot verify candidate access keys offline.

Plaintext on disk:

- `sessions.token_hash` — SHA-256 of a 32-byte (default) random token; not
  brute-forceable.
- `objects.meta_ciphertext` and `object_payloads/*.bin` — already
  client-encrypted with a key derived from the OPAQUE `export_key`.

Wrap format and key derivation live in `clipper_core::crypto`
(`wrap_with_key`, `unwrap_with_key`, `derive_subkey`, `HKDF_LABEL_*`,
`AAD_WRAP_*`); each blob is `nonce_24 ‖ XChaCha20-Poly1305(plaintext, aad)`. The
column-to-subkey/AAD bindings live in
`crates/server/src/secret_storage.rs`.
The server unwraps these columns only at storage boundaries: before
calling OPAQUE helpers, before hashing/checking access keys, and during
startup validation of the stored `server_config.access_key_hash_salt`.
