# Signed Object Envelopes

Math + step-by-step for the object envelope code in
`crates/core/src/crypto.rs`, the client object upload/decrypt helpers
(`crates/client/src/api_client.rs`, `crates/client/src/engine.rs`), and the
server object routes (`crates/server/src/routes/objects.rs`). The shared wire
types live in `crates/api-types/src/lib.rs` (`ObjectEnvelopeBodyV1`,
`ObjectEnvelopePayloadV1`, `ObjectEnvelopeV1`).

## Notation

- `K`: per-user symmetric object encryption key derived from the OPAQUE export
  key (see "Key derivation" below).
- `sk_D, pk_D`: Ed25519 signing keypair for source device `D`.
- `AEAD_Enc(K, N, P, A)`: XChaCha20-Poly1305 encryption of plaintext `P` with
  24-byte nonce `N` and associated data `A`.
- `AEAD_Dec(K, N, C, A)`: matching authenticated decryption.
- `Sign(sk, m)` / `Verify(pk, m, sig)`: Ed25519 signature (64 bytes) and
  verification.
- `H(x)`: SHA-256.
- `Canon(x)`: postcard serialization of the Rust value `x`. Postcard is a
  positional binary format, so field order in the serialized struct is part of
  the canonical bytes.

## Key derivation

`K` is derived from the stable OPAQUE `export_key` with HKDF-SHA256 and a domain
label, in `derive_data_key_from_opaque_export_key`:

```text
K = HKDF-SHA256(salt = none, ikm = export_key)
      .expand("clipper:opaque-export:data-key:v1", 32)
```

The same `export_key` is reproduced by OPAQUE registration and login, so `K` is
stable across sessions and devices for one user without `K` ever being sent to
the server. The server never learns `export_key` or `K`.

A second, independent subkey is derived from the same `export_key` for wrapping
the device's persisted signing secret at rest (see "Device key storage"):

```text
wrap_key = HKDF-SHA256(salt = none, ikm = export_key)
             .expand("clipper:opaque-export:device-identity-wrap-key:v1", 32)
```

The labels keep `K` and `wrap_key` independent.

## Envelope Body

For object version 1, the signed body (`ObjectEnvelopeBodyV1`) is, in
serialization order:

```text
body = (
  object_id,
  object_type,
  object_version = 1,
  source_device_id,
  created_at,                 // RFC 3339 string
  operation = create,         // only `create` exists today
  meta_nonce,                 // 24 bytes
  H(meta_ciphertext),         // 32 bytes
  [
    (
      payload_id,
      payload_nonce,          // 24 bytes
      ciphertext_size,
      H(payload_ciphertext)   // 32 bytes
    ),
    ...                       // 1..=MAX_OBJECT_PAYLOAD_ENTRIES (16), unique ids
  ]
)

signature = Sign(sk_D, Canon(body))   // 64-byte Ed25519
envelope  = (body, signature)
```

The server stores `envelope` (postcard-encoded) with the object row and returns
it from object listing/get. The client verifies `signature` before decrypting
metadata or payload bytes (see "Trust model" for what that signature does and
does not protect against).

Clients currently always send exactly one payload entry, but the body and the
wire validators accept up to `MAX_OBJECT_PAYLOAD_ENTRIES = 16` payloads.

## AAD Projection

Ciphertext hashes cannot be inside the AAD for the same ciphertext because the
ciphertext does not exist until after encryption. The AEAD AAD is therefore a
projection of the envelope identity and payload set. It deliberately excludes
`meta_nonce`, the per-payload nonces, sizes, and ciphertext hashes; it binds
only the stable identity fields plus the payload-id set (and, for a payload, the
specific payload id). In serialization order (`ObjectAadV1`):

```text
A_meta = Canon((
  "clipper:object-meta-aad:v1",
  object_id,
  object_type,
  object_version,
  source_device_id,
  created_at,
  operation,
  [payload_id_1, payload_id_2, ...],   // ids drawn from body.payloads
  None
))

A_payload_i = Canon((
  "clipper:object-payload-aad:v1",
  object_id,
  object_type,
  object_version,
  source_device_id,
  created_at,
  operation,
  [payload_id_1, payload_id_2, ...],
  payload_id_i
))
```

Then:

```text
(N_meta, C_meta) = AEAD_Enc(K, random_24, meta_plaintext, A_meta)
(N_i, C_i)       = AEAD_Enc(K, random_24, payload_plaintext_i, A_payload_i)
```

The metadata plaintext is the JSON encoding of `ClipboardMeta` / `FileMeta`; the
payload plaintext is the raw clipboard/file bytes. The client builds the AAD by
constructing an envelope body that already carries the final `object_id`,
`object_type`, `source_device_id`, `created_at`, `operation`, and the payload-id
list (`create_object_envelope_body_for_aad` in `engine.rs`); the nonce/size/hash
fields are irrelevant to the AAD and are filled in afterward.

After encryption, the client fills `meta_nonce`, payload nonces, sizes, and
ciphertext hashes into `body`, signs `Canon(body)`, and uploads the request.

## Verification Flow

### Server, on object init (`validate_object_init_envelope`)

The server cross-checks the envelope body against the request context and then
verifies the signature:

```text
body.object_id        == request.id
body.object_type      == request.kind
body.object_version   == 1
body.source_device_id == authenticated device id
body.operation        == create
body.meta_nonce       == request.meta_nonce
H(request.meta_ciphertext) == body.sha256_meta_ciphertext
request payload set    == body payload set            // matched by payload id
for each payload: body.{nonce, ciphertext_size, sha256_ciphertext}
                    == request.{nonce, ciphertext_size, sha256_ciphertext}
Verify(pk_D, Canon(body), signature)
```

`pk_D` is loaded from the `devices` row keyed by `(authenticated device id,
authenticated user_id)`, so the server checks the signature against the
signing key it has on file for that device. `created_at` is parsed for RFC 3339
validity but is not compared against any server clock on init.

### Client, on list/get/download

The client repeats the envelope/list-item checks (`verify_object_list_item_envelope`),
verifies the signature, checks downloaded payload hashes, then decrypts with the
envelope AAD:

```text
body.object_id            == item.id
body.object_type          == item.kind
body.object_version       == 1
body.operation            == create
body.source_device_id     == item.source_device_id
body.created_at           == item.created_at
body.meta_nonce           == item.meta_nonce
body.sha256_meta_ciphertext == H(item.meta_ciphertext)
body payload set          == item payload set         // matched by id, with
                                                      // nonce/size/hash equality
Verify(pk_D, Canon(body), signature)
H(downloaded_payload_i)   == body.payload_i.H(payload_ciphertext)
P_i = AEAD_Dec(K, N_i, C_i, A_payload_i)
```

Here `pk_D` is `item.source_device_signing_public_key`, which the **server**
supplies in the list/get response (it is read from the source device's row,
scoped to the authenticated user, in `object_list_items`). The client does not
pin or otherwise independently verify device public keys, so the signature check
confirms only that the envelope is internally consistent with whatever key the
server returned — see "Trust model".

`source_device_signing_public_key` is `Option<…>` and is **`None` once the
source device has been reclaimed**: the `objects.source_device_id` foreign key is
`ON DELETE SET NULL`, so deleting a device detaches its objects (the column
becomes NULL) and the server no longer holds a key to attest provenance. With no
key the client **skips** `Verify(pk_D, …)` but still performs every other
equality check above and, critically, the AEAD step — which is the real
authenticity mechanism (see "Trust model"). `item.source_device_id` itself is
unaffected: the server reports it from the signed envelope body, which survives
reclamation, so the `body.source_device_id == item.source_device_id` check still
holds.

### Ciphertext-substitution argument

If a server swaps object `Y`'s ciphertext into object `X`, the receiver uses
`A_payload_X` while the tag was created with `A_payload_Y`:

```text
AEAD_Dec(K, N_Y, C_Y, A_payload_X) = reject
```

Because `K` is derived from the user's OPAQUE export key, the server cannot
produce a valid `(C, tag)` under `A_payload_X` for chosen plaintext, and it
cannot move a tag created under `A_payload_Y` onto `X` whose AAD differs in
`object_id` (and the payload-id set / payload id). The AAD also pins
`object_type`, `source_device_id`, `created_at`, and `operation`, so none of
those identity fields can be retargeted without breaking the AEAD tag.

## Trust model — what authenticity the AEAD vs. the signature provide

The cryptographic authenticity of object contents rests on the **AEAD with `K`
plus its AAD**, not on the Ed25519 envelope signature.

- `K` is derived from the OPAQUE export key and is never disclosed to the
  server. AEAD verification (with the AAD binding above) is what stops the
  server (or anyone in the middle) from forging, swapping, or retargeting
  ciphertext that a legitimate client will accept and decrypt.
- The Ed25519 envelope signature is verified against a public key the **server**
  hands back alongside the object (`source_device_signing_public_key`). There is
  no client-side device-key trust store or pinning. A malicious/compromised
  server could substitute its own device public key into the listing **and**
  re-sign the (matching) envelope body with the corresponding secret key; the
  client's signature check would pass. The AEAD step would still fail for any
  payload the server cannot encrypt under `K`.

So the signature is best understood as a server-checked provenance/consistency
field (the server rejects an init whose body is not signed by the claiming
device's on-file key, and clients reject a body that is internally inconsistent
with the listing), not as an end-to-end authenticity guarantee against the
server. Treat AEAD+AAD as the real authenticity mechanism. Genuine end-to-end
device authentication would require clients to learn and pin peer device public
keys out of band; that is not implemented today.

## Device key storage (at rest)

The device signing secret `sk_D` is generated locally
(`generate_device_signing_secret_key`) and persisted wrapped, not in the clear.
The on-disk / browser-storage record (`DeviceIdentityEncryptedRecord`, version 2) stores `wrap_with_key(wrap_key, sk_D, "clipper:wrap:device-signing-secret:v1")`,
where `wrap_key` is the export-key-derived wrapping key above and the wrap is
XChaCha20-Poly1305 (`nonce_24 || ciphertext_with_tag`). On native targets the
record file and its parent directory are created `0600`/`0700`, ownership-checked
against the current euid, and written atomically; legacy or forged plaintext
identity records are rejected rather than migrated. The locally cached object
records (metadata + payload ciphertext) are stored as the same ciphertext the
server holds, keyed by profile id derived from `K`; plaintext is only recovered
transiently by decrypting with `K`.
