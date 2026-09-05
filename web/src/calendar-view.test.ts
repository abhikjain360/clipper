import { test } from "node:test";
import assert from "node:assert/strict";
import { calendarWindow, movePeriod } from "./calendar-view.ts";

test("month navigation from the 31st cannot skip February", () => {
    const next = movePeriod(new Date(2026, 0, 31), "month", 1);
    assert.equal(next.getMonth(), 1);
    assert.equal(next.getDate(), 1);
    assert.equal(movePeriod(next, "month", -1).getMonth(), 0);
});
test("month window covers complete Monday-based weeks including spillover days", () => {
    const { start, end } = calendarWindow(new Date(2026, 7, 15), "month");
    assert.equal(start.getDay(), 1);
    assert.equal(start.getMonth(), 6);
    assert.equal(start.getDate(), 27);
    assert.equal(end.getMonth(), 8);
    assert.equal(end.getDate(), 7);
});
test("day and week windows advance in civil dates across DST and year boundaries", () => {
    const { start, end } = calendarWindow(new Date(2026, 2, 29, 12), "day");
    assert.equal(start.getHours(), 0);
    assert.equal(end.getHours(), 0);
    assert.equal(end.getDate(), 30);
    const next = movePeriod(new Date(2026, 11, 31), "week", 1);
    assert.equal(next.getFullYear(), 2027);
    assert.equal(next.getDay(), 1);
    assert.equal(next.getDate(), 4);
});
