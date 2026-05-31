# Signed Object Envelopes

Math + step-by-step for the object envelope code in
`crates/core/src/crypto.rs`, the client object upload/decrypt helpers, and the
server object routes.

## Notation

- `K`: per-user symmetric object encryption key derived from OPAQUE export key.
- `sk_D, pk_D`: Ed25519 signing keypair for source device `D`.
- `AEAD_Enc(K, N, P, A)`: XChaCha20-Poly1305 encryption of plaintext `P` with
  nonce `N` and associated data `A`.
- `AEAD_Dec(K, N, C, A)`: matching authenticated decryption.
- `Sign(sk, m)` / `Verify(pk, m, sig)`: Ed25519 signature and verification.
- `H(x)`: SHA-256.
- `Canon(x)`: postcard serialization of the Rust value `x`.

## Envelope Body

For object version 1, the signed body is:

```text
body = (
  object_id,
  object_type,
  object_version = 1,
  source_device_id,
  created_at,
  operation = create,
  meta_nonce,
  H(meta_ciphertext),
  [
    (
      payload_id,
      payload_nonce,
      ciphertext_size,
      H(payload_ciphertext)
    ),
    ...
  ]
)

signature = Sign(sk_D, Canon(body))
envelope  = (body, signature)
```

The server stores `envelope` with the object row and returns it from object
listing. The client verifies `signature` against the source device public key
before decrypting metadata or payload bytes.

## AAD Projection

Ciphertext hashes cannot be inside the AAD for the same ciphertext because the
ciphertext does not exist until after encryption. The AEAD AAD is therefore a
projection of the envelope identity and payload set:

```text
A_meta = Canon((
  "clipper:object-meta-aad:v1",
  object_id,
  object_type,
  object_version,
  source_device_id,
  created_at,
  operation,
  [payload_id_1, payload_id_2, ...],
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
(N_meta, C_meta) = AEAD_Enc(K, random, meta_plaintext, A_meta)
(N_i, C_i)       = AEAD_Enc(K, random, payload_plaintext_i, A_payload_i)
```

After encryption, the client fills `meta_nonce`, payload nonces, sizes, and
ciphertext hashes into `body`, signs `Canon(body)`, and uploads the request.

## Verification Flow

On object init, the server checks:

```text
body.object_id        == request.id
body.object_type      == request.kind
body.source_device_id == authenticated device id
body.meta_nonce       == request.meta_nonce
H(request.meta_ciphertext) == body.H(meta_ciphertext)
request payload set   == body payload set
Verify(pk_D, Canon(body), signature)
```

On list/download, the client repeats the envelope/list-item checks, verifies the
signature, checks downloaded payload hashes, then decrypts with the envelope AAD:

```text
Verify(pk_D, Canon(body), signature)
H(downloaded_payload_i) == body.payload_i.H(payload_ciphertext)
P_i = AEAD_Dec(K, N_i, C_i, A_payload_i)
```

If a server swaps object `Y`'s ciphertext into object `X`, the receiver uses
`A_payload_X` while the tag was created with `A_payload_Y`:

```text
AEAD_Dec(K, N_Y, C_Y, A_payload_X) = reject
```

If the server also edits the envelope to match `Y`, the signature no longer
verifies for object `X` unless it can forge `Sign(sk_D, Canon(body))`.
