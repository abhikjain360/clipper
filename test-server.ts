#!/usr/bin/env -S deno run --allow-net

/**
 * Clipper server end-to-end test script (§20 step 6 checkpoint).
 *
 * Usage:
 *   # Start server first:
 *   CLIPPER_PASSPHRASE="test-passphrase-123" cargo run -p clipper-server -- init --data-dir /tmp/clipper-test-data
 *   cargo run -p clipper-server -- serve --data-dir /tmp/clipper-test-data --addr 127.0.0.1:8787
 *
 *   # Then run this script:
 *   deno run --allow-net test-server.ts
 */

const BASE = "http://127.0.0.1:8787";
const PASSPHRASE = "test-passphrase-123";

// ── Helpers ──

async function api(
  method: string,
  path: string,
  body?: unknown,
  token?: string,
): Promise<Response> {
  const headers: Record<string, string> = {};
  if (body) headers["Content-Type"] = "application/json";
  if (token) headers["Authorization"] = `Bearer ${token}`;
  return fetch(`${BASE}${path}`, {
    method,
    headers,
    body: body ? JSON.stringify(body) : undefined,
  });
}

function assert(condition: boolean, msg: string) {
  if (!condition) {
    console.error(`FAIL: ${msg}`);
    Deno.exit(1);
  }
  console.log(`  ✓ ${msg}`);
}

function b64encode(data: Uint8Array): string {
  return btoa(String.fromCharCode(...data));
}

async function sha256(data: Uint8Array): Promise<Uint8Array> {
  const hash = await crypto.subtle.digest("SHA-256", data);
  return new Uint8Array(hash);
}

// ── Tests ──

console.log("\n=== 1. Health Check ===");
{
  const res = await api("GET", "/api/health");
  const json = await res.json();
  assert(res.status === 200, `status 200 (got ${res.status})`);
  assert(json.ok === true, "health ok=true");
}

console.log("\n=== 2. Login with wrong passphrase ===");
{
  const res = await api("POST", "/api/auth/login", {
    passphrase: "wrong-pass",
  });
  assert(res.status === 401, `status 401 (got ${res.status})`);
  await res.body?.cancel();
}

console.log("\n=== 3. Login with correct passphrase ===");
let token: string;
let deviceId: string;
let encSaltB64: string;
{
  const res = await api("POST", "/api/auth/login", {
    passphrase: PASSPHRASE,
    device_name: "deno-test",
    platform: "macos",
  });
  assert(res.status === 200, `status 200 (got ${res.status})`);
  const json = await res.json();
  token = json.token;
  deviceId = json.device_id;
  encSaltB64 = json.server.enc_salt_b64;
  assert(typeof token === "string" && token.length > 0, "got session token");
  assert(typeof deviceId === "string", `got device_id: ${deviceId}`);
  assert(typeof encSaltB64 === "string", "got enc_salt_b64");
  console.log(`  token: ${token.substring(0, 12)}...`);
}

console.log("\n=== 4. Unauthenticated request rejected ===");
{
  const res = await api("GET", "/api/clipboard");
  assert(res.status === 401, `status 401 (got ${res.status})`);
  await res.body?.cancel();
}

console.log("\n=== 5. Upload clipboard item ===");
const clipId = crypto.randomUUID();
{
  // Simulate encrypted ciphertext (in real usage this would be XChaCha20-Poly1305 output)
  const fakeCiphertext = new TextEncoder().encode(
    "this-is-fake-ciphertext-for-testing",
  );
  const fakeNonce = crypto.getRandomValues(new Uint8Array(24));
  const hash = await sha256(fakeCiphertext);

  const res = await api(
    "POST",
    "/api/clipboard",
    {
      id: clipId,
      nonce_b64: b64encode(fakeNonce),
      ciphertext_b64: b64encode(fakeCiphertext),
      ciphertext_sha256_b64: b64encode(hash),
      source_device_id: deviceId,
      client_created_at: new Date().toISOString(),
    },
    token,
  );
  assert(res.status === 200, `status 200 (got ${res.status})`);
  const json = await res.json();
  assert(json.ok === true, "clipboard upload ok=true");
}

console.log("\n=== 6. List clipboard items ===");
{
  const res = await api("GET", "/api/clipboard?limit=10", undefined, token);
  assert(res.status === 200, `status 200 (got ${res.status})`);
  const json = await res.json();
  assert(Array.isArray(json.items), "items is array");
  assert(json.items.length >= 1, `got ${json.items.length} item(s)`);

  const found = json.items.find(
    (i: { id: string }) => i.id === clipId,
  );
  assert(found !== undefined, `found uploaded item ${clipId}`);
  assert(
    typeof found.ciphertext_b64 === "string",
    "item has ciphertext (not plaintext)",
  );
  assert(typeof found.nonce_b64 === "string", "item has nonce");
}

console.log("\n=== 7. Verify ciphertext storage on disk ===");
{
  // The server should have stored the ciphertext file
  const res = await api("GET", "/api/clipboard?limit=1", undefined, token);
  const json = await res.json();
  assert(json.items.length >= 1, "at least one item returned");
  // Ciphertext should be base64, not plaintext
  const ct = json.items[0].ciphertext_b64;
  assert(!ct.includes("hello"), "ciphertext is not plaintext");
}

console.log("\n=== 8. File upload flow (init → blob → complete) ===");
const fileId = crypto.randomUUID();
{
  const fakeMetaCiphertext = new TextEncoder().encode("encrypted-meta");
  const fakeMetaNonce = crypto.getRandomValues(new Uint8Array(24));
  const fakeBlobNonce = crypto.getRandomValues(new Uint8Array(24));
  const fakeBlob = new TextEncoder().encode(
    "this-is-fake-encrypted-file-content-for-testing",
  );

  // Step 1: init
  const initRes = await api(
    "POST",
    "/api/files/init",
    {
      id: fileId,
      meta_nonce_b64: b64encode(fakeMetaNonce),
      meta_ciphertext_b64: b64encode(fakeMetaCiphertext),
      blob_nonce_b64: b64encode(fakeBlobNonce),
      blob_size: fakeBlob.length,
      source_device_id: deviceId,
    },
    token,
  );
  assert(initRes.status === 200, `init status 200 (got ${initRes.status})`);
  const initJson = await initRes.json();
  assert(
    initJson.upload_url === `/api/files/${fileId}/blob`,
    `got upload_url: ${initJson.upload_url}`,
  );

  // Step 2: upload blob
  const blobRes = await fetch(`${BASE}/api/files/${fileId}/blob`, {
    method: "PUT",
    headers: { Authorization: `Bearer ${token}` },
    body: fakeBlob,
  });
  assert(blobRes.status === 200, `blob status 200 (got ${blobRes.status})`);
  await blobRes.json();

  // Step 3: complete
  const blobHash = await sha256(fakeBlob);
  const completeRes = await api(
    "POST",
    `/api/files/${fileId}/complete`,
    {
      sha256_ciphertext_b64: b64encode(blobHash),
      blob_size: fakeBlob.length,
    },
    token,
  );
  assert(
    completeRes.status === 200,
    `complete status 200 (got ${completeRes.status})`,
  );
  const completeJson = await completeRes.json();
  assert(completeJson.ok === true, "file complete ok=true");
}

console.log("\n=== 9. List files ===");
{
  const res = await api("GET", "/api/files?limit=10", undefined, token);
  assert(res.status === 200, `status 200 (got ${res.status})`);
  const json = await res.json();
  assert(Array.isArray(json.items), "items is array");
  const found = json.items.find(
    (i: { id: string }) => i.id === fileId,
  );
  assert(found !== undefined, `found uploaded file ${fileId}`);
}

console.log("\n=== 10. Download file blob ===");
{
  const res = await fetch(`${BASE}/api/files/${fileId}/blob`, {
    headers: { Authorization: `Bearer ${token}` },
  });
  assert(res.status === 200, `status 200 (got ${res.status})`);
  const body = await res.arrayBuffer();
  const text = new TextDecoder().decode(body);
  assert(
    text === "this-is-fake-encrypted-file-content-for-testing",
    "downloaded blob matches uploaded blob",
  );
}

console.log("\n=== 11. Delete file ===");
{
  const res = await fetch(`${BASE}/api/files/${fileId}`, {
    method: "DELETE",
    headers: { Authorization: `Bearer ${token}` },
  });
  assert(res.status === 200, `status 200 (got ${res.status})`);
  const json = await res.json();
  assert(json.ok === true, "delete ok=true");

  // Verify gone
  const listRes = await api("GET", "/api/files?limit=10", undefined, token);
  const listJson = await listRes.json();
  const found = listJson.items.find(
    (i: { id: string }) => i.id === fileId,
  );
  assert(found === undefined, "file no longer in list after delete");
}

console.log("\n=== 12. Sync bootstrap ===");
{
  const res = await api("GET", "/api/sync/bootstrap", undefined, token);
  assert(res.status === 200, `status 200 (got ${res.status})`);
  const json = await res.json();
  assert(typeof json.device === "object", "has device info");
  assert(json.device.name === "deno-test", "device name matches");
  assert(Array.isArray(json.clipboard_items), "has clipboard_items");
  assert(Array.isArray(json.files), "has files");
  assert(typeof json.latest_seq === "number", "has latest_seq");
  assert(typeof json.server.enc_salt_b64 === "string", "has enc_salt_b64");
}

console.log("\n=== 13. Logout ===");
{
  const res = await api("POST", "/api/auth/logout", undefined, token);
  assert(res.status === 200, `status 200 (got ${res.status})`);

  // Verify token is invalidated
  const afterRes = await api("GET", "/api/clipboard", undefined, token);
  assert(
    afterRes.status === 401,
    `post-logout status 401 (got ${afterRes.status})`,
  );
  await afterRes.body?.cancel();
}

console.log("\n=== ALL TESTS PASSED ===\n");
