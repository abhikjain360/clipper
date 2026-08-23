# Cryptography Review — 2026-07-20

> Independent, cryptography-focused review at HEAD `09296c4`. Complements
> `docs/security-review.md` and `docs/client-security-audit-2026-06-27.md`
> (Part I + II). Scope: the cryptographic primitives and their composition —
> AEAD, key derivation, OPAQUE, the server pepper, client at-rest encryption,
> and the device signature scheme. Out of scope: the broader web-XSS, Android,
> and sync-availability surfaces already covered by the client audit.
>
> **Status: documentation only.** No code was changed. Recommendations are
> prose; nothing here has been applied.

## Method

Read the actual implementation rather than the surrounding docs:
`crates/core/src/crypto.rs` (all primitives), `crates/server/src/routes/auth.rs`
(OPAQUE register/login/session flow), `crates/server/src/secret.rs` and
`secret_storage.rs` (pepper), `crates/client/src/engine.rs` and
`local_store.rs` (client key handling and at-rest wrapping),
`crates/api-types/src/lib.rs` (wire types and Argon2 bounds),
`crates/daemon/src/handler.rs` (IPC HMAC), and the dependency sources
`opaque-ke-4.1.0-pre.2` and `argon2-0.5.3` (to confirm the effective KSF
parameters). Findings were then cross-checked against the prior audit.

## Verdict

**The cryptographic core is sound.** This is an independent conclusion that
matches the prior audit's Part II — no breaking flaw was found in the primitives
or in how they are composed. The primitive selection is uniformly modern and
correctly applied; there is no path to forge E2E ciphertext or recover a
passphrase from a database dump without the server pepper.

What stands between the current state and "bulletproof" is **not a bug but a
small set of hardening gaps plus one missing capability (key rotation)**. The
findings below are ordered by leverage toward that goal. Severities are given
two ways where they diverge: the strict client-threat-model rating (matching the
prior audit's conventions) and, in parentheses, the rating under an explicit
"make the cryptography bulletproof" goal.

## Independently verified safe

Re-derived from code so the surface does not get re-litigated:

- **AEAD.** XChaCha20-Poly1305 with a fresh random 24-byte nonce per message
  (`crypto.rs:601-650`). At this application's message volume the nonce-collision
  probability is negligible (birthday bound ≈ 2⁹⁶). Every decrypt path verifies
  the tag before releasing plaintext.
- **AAD binding is tight, and the "excluded" fields are fine.** Object meta and
  payload AAD (`crypto.rs:176-212`) bind `object_id`, `object_type`,
  `object_version`, `source_device_id`, `created_at`, `operation`, the payload-id
  set, and (for a payload) the specific payload id, with distinct
  `object-meta-aad` / `object-payload-aad` domain strings. The nonce deliberately
  is **not** in the AAD — this is correct, not a gap: ChaCha20-Poly1305's tag
  already authenticates the nonce, so a server cannot change the nonce without
  failing decryption. `created_at` _is_ authenticated, so a server cannot rewrite
  an object's timestamp without breaking the tag. Ciphertext swap or retargeting
  between objects / payload slots / meta-vs-payload roles fails the tag.
- **Key separation.** The data key, the device-identity wrapping key, and the five
  server pepper subkeys are all HKDF-SHA256 expansions under distinct labels; the
  separation is unit-tested (`crypto.rs` tests, `secret.rs` tests). No label
  collisions.
- **OPAQUE.** `Ristretto255 + TripleDH + SHA-512 + Argon2id` is an RFC-recommended
  suite. Verified in `auth.rs`: unknown usernames take the fake-record path
  (no enumeration oracle), challenges are single-use (`take_auth_challenge` pops
  the state), the OPAQUE `session_key` is genuinely discarded — the `?` on
  `opaque_server_login_finish` drops and zeroizes it — in favor of an independent
  random bearer token, and `export_key` never leaves the client.
- **Server pepper.** Per-purpose HKDF subkeys, `Zeroize`/`ZeroizeOnDrop` on the
  root and subkeys, fail-closed loading (wrong/missing pepper fails before the
  listener binds), and the two per-user columns bind `user_id` into the AAD so a
  wrapped blob cannot be replayed across rows (`secret_storage.rs:user_column_aad`).
  The Argon2 config enforces a 19 MiB / t=2 **floor** (`api-types.rs:30-33`), so the
  server cannot be misconfigured into weak access-key hashing.
- **No weak primitives anywhere.** A full sweep found no MD5, SHA-1, AES-ECB/CBC,
  or non-CSPRNG used for secret material. The daemon IPC handshake is a
  constant-time (`Mac::verify_slice`) two-nonce HMAC-SHA256 challenge-response with
  client/daemon message separation (`daemon/src/handler.rs:285-320`). The legacy
  passphrase+salt `derive_key` KDF is now **test-only** — production keys derive
  solely from the OPAQUE `export_key`.
- **Key commitment is not a concern.** All object material encrypts under a single
  per-user key `K`, so multi-key ambiguous-decrypt ("invisible salamanders")
  attacks do not apply; a given `(nonce, ciphertext, aad)` under `K` decrypts to
  exactly one plaintext or fails.

## Findings

| #   | Sev (threat-model / bulletproof-goal) | Area       | Title                                                                             |
| --- | ------------------------------------- | ---------- | --------------------------------------------------------------------------------- |
| CR1 | Low / **Strategic**                   | design     | No data-key rotation and no passphrase-change flow — compromise is permanent      |
| CR2 | Info / Medium                         | OPAQUE     | KSF is the argon2 crate _default_ (19 MiB/t=2), unpinned and at OWASP's floor     |
| CR3 | Info                                  | signatures | Device key signs two message types with no domain separation (R38/R39)            |
| CR4 | Low                                   | at-rest    | `device_id` is plaintext and unauthenticated in the device-identity record (R25)  |
| CR5 | Info                                  | docs/web   | The web data key _is_ script-readable — docs understate this                      |
| CR6 | Medium                                | mobile     | Confirmed R1: Android persists the passphrase (the E2E root) for biometric resume |

### CR1 — [Strategic] No data-key rotation and no passphrase-change flow

- **Files:** `crates/core/src/crypto.rs:286` (`derive_data_key_from_opaque_export_key`) ·
  `docs/object-envelopes.md` ("Key derivation") · no rotation/change flow anywhere
- **What:** `K = HKDF-SHA256(OPAQUE export_key, "clipper:opaque-export:data-key:v1")`
  is static for the life of the account. The export key is stable for a given
  `(passphrase, server registration)`, and there is no passphrase-change or
  key-rotation flow in the repo.
- **Why it matters:** if `K` or the passphrase is ever exposed — XSS reading the web
  session blob (CR5), a keylogger, a coerced unlock, or a malicious server that
  archives ciphertext — **all past and future ciphertext is permanently decryptable,
  with no remediation short of a new account.** Revoking sessions/devices bounds only
  API access, not ciphertext already recorded. The prior audit noted the static key as
  Low (R14) inside the web-`sessionStorage` context; under a "bulletproof" goal this is
  the single most important property to fix, because it is what converts every other
  key-exposure scenario from _bounded_ into _permanent_.
- **Recommendation:** add a passphrase-change / re-registration flow. OPAQUE supports
  re-registering the credential under a new passphrase, which yields a new `rwd`, a new
  export key, and therefore a new `K`; the missing piece is a client re-encrypt-under-
  new-`K` pass over retained objects plus a server-side credential update. Until this
  exists, treat any key exposure as total and document that explicitly.

### CR2 — [Info / Medium] OPAQUE KSF is the argon2 crate default, unpinned, at OWASP's floor

- **Files:** `crates/core/src/crypto.rs:37-42` (`ClipperOpaqueCipherSuite`,
  `type Ksf = opaque_ke::argon2::Argon2<'static>`) ·
  `argon2-0.5.3/src/params.rs:42-61` (`DEFAULT_M_COST = 19 * 1024`, `DEFAULT_T_COST = 2`,
  `DEFAULT_P_COST = 1`)
- **What:** the passphrase-hardening KSF resolves to `argon2::Argon2::default()` =
  Argon2id `m = 19 MiB, t = 2, p = 1`. Two issues:
  1. It is OWASP's _lower-bound_ Argon2id recommendation. For a secrets vault, the
     offline brute-force cost per passphrase guess could be raised (OWASP's first option
     is 46 MiB / t=1; 64 MiB / t=3 is a common stronger choice). The KSF runs
     client-side at login, so the tradeoff is login latency, not server load.
  2. It is **implicit.** The KSF parameters are not stored in the password file, so a
     future `opaque-ke`/`argon2` bump that moves the default would _silently_ change
     passphrase hardness — and silently break logins (or fragment clients) if deployed
     versions diverge, since `rwd` would no longer match.
- **Why it matters:** this is the per-guess cost against an offline attacker who has the
  unwrapped password file. It is adequate today, but it is the weakest dial in the
  passphrase-protection chain and it is not under the project's explicit control.
- **Recommendation:** pin the KSF parameters explicitly in `ClipperOpaqueCipherSuite`
  (construct the Argon2 KSF with chosen `Params` rather than relying on `Default`), add
  a test asserting them, and consider raising toward OWASP's first recommendation. This
  sharpens R45, which flagged the pre-release `opaque-ke` dependency but not the
  unpinned KSF.

### CR3 — [Info] Device key signs two message types with no domain separation

- **Files:** `crates/core/src/crypto.rs:108-160` (`sign_object_envelope_body`,
  `sign_device_login_proof_body`, both signing raw postcard bytes) ·
  `crates/api-types/src/lib.rs:282` (`DeviceLoginProofBodyV1`), `:344` (`ObjectEnvelopeBodyV1`)
- **What:** the same Ed25519 device key signs both `ObjectEnvelopeBodyV1` and
  `DeviceLoginProofBodyV1` over raw postcard bytes with no domain-separation tag.
- **Assessment:** confirmed the prior audit's dismissal (R38/R39). Cross-type signature
  replay would require making the two canonical byte strings equal, but their fields
  include SHA-256 outputs and random nonces — a preimage barrier — and the envelope
  signature is not the E2E authenticity mechanism anyway (the AEAD under `K` is; the
  server supplies the verifying key). Not exploitable today.
- **Why fix it anyway:** signing two distinct message types under one key without a
  separator is a textbook anti-pattern, and the fix is one line per signer. Doing it now,
  before any deployment, closes R38 and R39 permanently.
- **Recommendation:** sign `"clipper:object-envelope:v1" ‖ Canon(body)` and
  `"clipper:device-login-proof:v1" ‖ Canon(body)` (or fold a distinct `domain` field
  into each body) so the two signed messages can never collide.

### CR4 — [Low] `device_id` is plaintext and unauthenticated in the device-identity record

- **Files:** `crates/client/src/local_store.rs:1713-1738` (wrap/unwrap with the constant
  `AAD_WRAP_DEVICE_SIGNING_SECRET_V1`) · `docs/local-at-rest-encryption.md` ("Flagged gaps")
- **What:** the at-rest wrap of the device signing secret uses a **constant** AAD, and
  `device_id` sits in cleartext beside the wrapped secret. A same-uid process or
  same-origin script can substitute another UUID undetected, causing silent device
  re-mint on the next login.
- **Recommendation:** bind `device_id ‖ profile_id` into the wrap AAD so the header is
  authenticated alongside the secret. (Same as R25; recorded here for completeness.)

### CR5 — [Info] The web data key is script-readable — docs understate this

- **Files:** `docs/local-at-rest-encryption.md` (sessionStorage section — already
  accurate) · `docs/client-security-audit-2026-06-27.md` A3 ("not JS-readable" framing —
  optimistic)
- **What:** A3 describes the wasm-resident data key as living "only in wasm linear memory
  and [not] JS-readable." That is optimistic: wasm linear memory is JS-readable whenever
  the module exports its memory (wasm-bindgen does), and the confused-deputy (A3's own
  point) makes the distinction moot — injected script can simply call the backend to
  decrypt. The honest statement is: **on web, the data key is exposed to any same-origin
  script, period.**
- **Also verified:** the implied stronger mitigation is unavailable. WebCrypto has no
  ChaCha20-Poly1305, so the data key cannot be held as a non-extractable browser key
  without changing the E2E AEAD — and web ciphertext must stay XChaCha20-Poly1305 to
  remain decryptable by the native clients it syncs with. So the _only_ real web controls
  are XSS / bundle-tamper prevention (a real CSP **header** + SRI, A2), session-scoped key
  lifetime (done in `clipper.session.v2`), and — the missing keystone — key rotation (CR1).
- **Recommendation:** correct the A3 wording (a correction note is appended to that
  finding), and lean on A2 + CR1 rather than on the "memory-only" framing.

### CR6 — [Medium] Confirmed R1: Android persists the passphrase for biometric resume

- **Files:** `mobile/src/backend.ts` (SecureStore passphrase resume) ·
  `crates/mobile-uniffi/src/lib.rs` (no resume export) ·
  `docs/local-at-rest-encryption.md` (mobile note)
- **What:** re-confirmed that Android stores the OPAQUE passphrase — the revocation-proof
  E2E root — in the OS keystore behind Class-3 biometric auth, replaying a full OPAQUE
  login on cold start, while the safer v2 resume material (token + data key + wrapping
  key, no passphrase) built for the web A1 fix was never wired to UniFFI.
- **Assessment:** the keystore + biometric gate makes extraction materially harder than
  the web `sessionStorage` blob (a separate installed app gets nothing), hence Medium not
  High — but server-side revocation still cannot bound a store compromise, and the
  passphrase re-enters the RN JS heap on every cold start.
- **Recommendation:** export the resume / `session_resume_material` equivalents over
  UniFFI and store the v2 material in the keystore slot instead of the passphrase,
  mirroring `clipper.session.v2`. (Same as R1.)

## Prioritized recommendations

Toward "bulletproof," in order of leverage:

1. **CR1 — build a key-rotation / passphrase-change flow.** This is the one change that
   converts key-exposure from _permanent_ to _bounded_; every other web/mobile key-exposure
   finding is capped by its absence.
2. **CR2 — pin and strengthen the OPAQUE KSF.** Small, mechanical, and safe now that nothing
   is deployed; removes a silent-dependency-drift hazard and raises the offline-guess cost.
3. **CR3 — add signature domain separation** (one line per signer; closes R38/R39).
4. **CR4 — bind `device_id ‖ profile_id` into the device-identity wrap AAD** (closes R25).
5. **CR5 — keep the web key out of persistent storage and prevent XSS** (CSP header + SRI,
   A2); correct the "not JS-readable" doc wording.
6. **CR6 — move mobile off the persisted passphrase** to the v2 resume material (R1).

Items 2–4 are small, low-risk, and appropriate to land before any deployment. Item 1 is the
substantial one and is the difference between "well-engineered E2E" and "bulletproof."
