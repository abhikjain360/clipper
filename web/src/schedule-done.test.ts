import assert from "node:assert/strict";
import { test } from "node:test";
import {
    loadDoneMarks,
    occurrenceHidden,
    occurrenceId,
    writeDoneMark,
    type DoneMark,
} from "../../packages/shared/src/schedule-done.ts";

const event = {
    item_id: "019a6312-6680-7000-8000-000000000001",
    occurrence_key: "date:2026-10-08",
    start: "2026-10-08T09:00:00Z",
    end: "2026-10-08T10:00:00Z",
};

test("an event stays visible after starting and hides at its end without changing marks", () => {
    const marks = new Set<string>();
    assert.equal(occurrenceHidden(event, marks, Date.parse(event.start) - 1), false);
    assert.equal(occurrenceHidden(event, marks, Date.parse(event.start)), false);
    assert.equal(occurrenceHidden(event, marks, Date.parse(event.end) - 1), false);
    assert.equal(occurrenceHidden(event, marks, Date.parse(event.end)), true);
    assert.equal(occurrenceHidden(event, marks, Date.parse(event.end) + 1), true);
    assert.equal(marks.size, 0);
});

test("another device reads a done mark, hides only that occurrence, and undo restores it", async () => {
    const rows = new Map<string, DoneMark>();
    const phone = {
        writeAppData: async (collection: string, id: string | null, write: unknown) => {
            assert.equal(collection, "schedule.done");
            assert.equal(id, null);
            assert.ok(typeof write === "object" && write !== null && "value" in write);
            const mark = write.value as DoneMark;
            rows.set(occurrenceId(mark), mark);
            return "row-id";
        },
    };
    const desktop = {
        queryAppData: async (sql: string) => {
            assert.equal(sql, "SELECT value FROM schedule.done");
            return Array.from(rows.values(), (value) => ({ value: JSON.stringify(value) }));
        },
    };
    const now = Date.parse(event.start);
    await writeDoneMark(phone, event, true);
    const marks = await loadDoneMarks(desktop);
    assert.equal(occurrenceHidden(event, marks, now), true);
    assert.equal(
        occurrenceHidden({ ...event, occurrence_key: "date:2026-10-09" }, marks, now),
        false,
    );
    assert.equal(occurrenceHidden({ ...event, item_id: "other" }, marks, now), false);
    const edited = { ...event, plan_context: "new revision", end: "2026-10-08T11:00:00Z" };
    assert.equal(occurrenceHidden(edited, marks, now), true);
    await writeDoneMark(phone, edited, false);
    assert.equal(occurrenceHidden(edited, await loadDoneMarks(desktop), now), false);
    assert.equal(
        occurrenceHidden(edited, await loadDoneMarks(desktop), Date.parse(edited.end)),
        true,
    );
    assert.equal(rows.size, 1);
});
