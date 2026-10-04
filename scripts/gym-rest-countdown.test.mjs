import assert from "node:assert/strict";
import { test } from "node:test";
import { restSecondsLeft } from "../packages/shared/src/gym.ts";

test("a rest shows its full target at the moment of completion and then counts down", () => {
  const completedAt = 1_000_000;
  const endsAt = completedAt + 120_000;
  assert.equal(restSecondsLeft(completedAt, endsAt, completedAt - 900), 120);
  assert.equal(restSecondsLeft(completedAt, endsAt, completedAt), 120);
  assert.equal(restSecondsLeft(completedAt, endsAt, completedAt + 300), 120);
  assert.equal(restSecondsLeft(completedAt, endsAt, completedAt + 1_000), 119);
  assert.equal(restSecondsLeft(completedAt, endsAt, endsAt - 1), 1);
  assert.equal(restSecondsLeft(completedAt, endsAt, endsAt), 0);
});
