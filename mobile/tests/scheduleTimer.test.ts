import assert from "node:assert/strict";
import { test } from "node:test";
import { occurrenceRunning } from "../../packages/shared/src/schedule-timer.ts";

test("running identity uses the series and recurrence key even when the start changes", () => {
  const event = { item_id: "series", occurrence_key: "floating:2026-10-09T09:00:00" };
  const running = {
    ...event,
    id: "timer",
    title: "Work",
    start: "2026-10-09T09:07:00Z",
    end: "",
    running: true,
  };
  assert.equal(occurrenceRunning(event, running), true);
  assert.equal(occurrenceRunning({ ...event, item_id: "other" }, running), false);
  assert.equal(occurrenceRunning({ ...event, occurrence_key: "next" }, running), false);
  assert.equal(occurrenceRunning(event, { ...running, running: false }), false);
  assert.equal(occurrenceRunning(event, { ...running, end: "2026-10-09T10:00:00Z" }), false);
  assert.equal(occurrenceRunning(event, { ...running, item_id: "" }), false);
  assert.equal(occurrenceRunning(event, null), false);
});
