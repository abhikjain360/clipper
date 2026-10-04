import assert from "node:assert/strict";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { readWithDeadline } from "../mobile/src/secureRead.ts";

test("a fingerprint read that never settles fails and can be retried", async () => {
  await assert.rejects(
    readWithDeadline(() => new Promise(() => {}), 5),
    /Fingerprint unlock did not finish.*retry/,
  );
  assert.equal(await readWithDeadline(async () => "credentials", 5), "credentials");
});

test("a refused fingerprint prompt surfaces its error", async () => {
  const error = new Error("Unlock the phone and bring Clipper to the foreground, then retry");
  await assert.rejects(
    readWithDeadline(async () => {
      throw error;
    }, 5),
    (caught) => caught === error,
  );
});

test("a late credential read cannot complete a timed out unlock", async () => {
  const events = [];
  const pending = delay(30).then(() => {
    events.push("read finished");
    return "late credentials";
  });
  const unlock = readWithDeadline(() => pending, 5).then(
    () => events.push("unlocked"),
    (error) => {
      assert.match(error.message, /Fingerprint unlock did not finish/);
      events.push("unlock refused");
    },
  );
  await Promise.all([pending, unlock]);
  assert.deepEqual(events, ["unlock refused", "read finished"]);
});
