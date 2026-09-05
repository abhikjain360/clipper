import { test } from "node:test";
import assert from "node:assert/strict";
import { nextStarts, orderByNextStart } from "./schedule-order.ts";
import type { OccurrenceView } from "@clipper/shared";

const occurrence = (id: string, start: string, cancelled = false) =>
    ({ item_id: id, start, cancelled }) as OccurrenceView;

test("next start ignores past starts and cancellations, retaining the earliest repeat", () => {
    const now = Date.parse("2026-09-10T12:00:00Z");
    const starts = nextStarts(
        [
            occurrence("repeat", "2026-09-12T07:00:00Z"),
            occurrence("repeat", "2026-09-11T07:00:00Z"),
            occurrence("repeat", "2026-09-10T07:00:00Z"),
            occurrence("cancelled", "2026-09-10T13:00:00Z", true),
            occurrence("now", "2026-09-10T12:00:00Z"),
        ],
        now,
    );
    assert.equal(starts.get("repeat"), Date.parse("2026-09-11T07:00:00Z"));
    assert.equal(starts.has("cancelled"), false);
    assert.equal(starts.get("now"), now);
});
test("upcoming first, stable ties, missing starts last, and no input mutation", () => {
    const items = ["past", "later", "tie1", "soon", "tie2", "missing"].map((id) => ({ id }));
    const starts = new Map([
        ["later", 30],
        ["tie1", 20],
        ["soon", 10],
        ["tie2", 20],
    ]);
    assert.deepEqual(
        orderByNextStart(items, starts).map((x) => x.id),
        ["soon", "tie1", "tie2", "later", "past", "missing"],
    );
    assert.equal(items[0]?.id, "past");
});
