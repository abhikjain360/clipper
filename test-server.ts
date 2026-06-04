#!/usr/bin/env -S deno run --allow-net --allow-env

/**
 * Clipper server JSON smoke test.
 *
 * Scope is limited to JSON endpoints (health, auth rejection, sync bootstrap,
 * logout). Objects and clipboard flows now use postcard wire format and are
 * covered by Rust integration tests in `crates/server/src/routes/objects.rs`
 * and the Rust client; replicating them here would require a postcard codec in
 * TypeScript.
 *
 * Usage:
 *   # Start server first:
 *   cargo run -p clipper-server -- init --data-dir /tmp/clipper-test-data
 *   cargo run -p clipper-server -- serve --data-dir /tmp/clipper-test-data --addr 127.0.0.1:8787
 *
 *   # Then run this script:
 *   # OPAQUE login is implemented in the Rust client because it needs the
 *   # shared Rust crypto stack. This script expects an already-issued token.
 *   CLIPPER_TOKEN="..." deno run --allow-net --allow-env test-server.ts
 */

const BASE = "http://127.0.0.1:8787";
const token = Deno.env.get("CLIPPER_TOKEN") ?? "";
assert(token.length > 0, "CLIPPER_TOKEN is set");

// ── Helpers ──

function api(
  method: string,
  path: string,
  body?: unknown,
  bearerToken?: string,
): Promise<Response> {
  const headers: Record<string, string> = {};
  if (body) headers["Content-Type"] = "application/json";
  if (bearerToken) headers["Authorization"] = `Bearer ${bearerToken}`;
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

// ── Tests ──

console.log("\n=== 1. Health Check ===");
{
  const res = await api("GET", "/api/health");
  const json = await res.json();
  assert(res.status === 200, `status 200 (got ${res.status})`);
  assert(json.ok === true, "health ok=true");
}

console.log("\n=== 2. Unauthenticated request rejected ===");
{
  const res = await api("GET", "/api/objects");
  assert(res.status === 401, `status 401 (got ${res.status})`);
  await res.body?.cancel();
}

console.log("\n=== 3. Sync bootstrap ===");
{
  const res = await api("GET", "/api/sync/bootstrap", undefined, token);
  assert(res.status === 200, `status 200 (got ${res.status})`);
  const json = await res.json();
  assert(typeof json.device === "object", "has device info");
  assert(typeof json.latest_seq === "number", "has latest_seq");
  assert(typeof json.server.encryption_salt_b64 === "string", "has encryption_salt_b64");
}

console.log("\n=== 4. Logout ===");
{
  const res = await api("POST", "/api/auth/logout", undefined, token);
  assert(res.status === 200, `status 200 (got ${res.status})`);

  // Verify token is invalidated
  const afterRes = await api("GET", "/api/objects", undefined, token);
  assert(afterRes.status === 401, `post-logout status 401 (got ${afterRes.status})`);
  await afterRes.body?.cancel();
}

console.log("\n=== ALL TESTS PASSED ===\n");
