import assert from "node:assert/strict";
import { test } from "node:test";
import { layoutDay, overlapsDay, spanMinutes } from "./schedule-layout.ts";

process.env.TZ = "Europe/Berlin";

function block(title: string, start: string, end: string) {
    return { title, start: `2026-09-08T${start}:00Z`, end: `2026-09-08T${end}:00Z` };
}

test("day boundaries follow local midnight through DST", () => {
    const spring = new Date("2026-03-29T12:00:00+02:00");
    assert.equal(
        overlapsDay({ start: "2026-03-29T22:15:00Z", end: "2026-03-29T22:45:00Z" }, spring),
        false,
    );
    const autumn = new Date("2026-10-25T12:00:00+01:00");
    assert.equal(
        overlapsDay({ start: "2026-10-25T22:15:00Z", end: "2026-10-25T22:45:00Z" }, autumn),
        true,
    );
    assert.deepEqual(
        spanMinutes({ start: "2026-03-29T07:00:00Z", end: "2026-03-29T08:00:00Z" }, spring),
        [540, 600],
    );
    assert.deepEqual(
        spanMinutes({ start: "2026-10-25T08:00:00Z", end: "2026-10-25T09:00:00Z" }, autumn),
        [540, 600],
    );
});

test("overnight spans clip at midnight and a running timer ends at now", () => {
    const day = new Date("2026-09-08T12:00:00+02:00");
    assert.deepEqual(
        spanMinutes({ start: "2026-09-07T21:00:00Z", end: "2026-09-09T02:00:00Z" }, day),
        [0, 1440],
    );
    assert.deepEqual(
        spanMinutes(
            { start: "2026-09-08T07:00:00Z", end: "" },
            day,
            Date.parse("2026-09-08T07:30:00Z"),
        ),
        [540, 570],
    );
});

test("overlaps get separate lanes and disconnected blocks reclaim full width", () => {
    const day = new Date("2026-09-08T12:00:00+02:00");
    const positioned = layoutDay(
        [
            block("A", "07:00", "08:00"),
            block("B", "07:30", "08:30"),
            block("C", "08:00", "09:00"),
            block("D", "10:00", "11:00"),
        ],
        day,
    );
    assert.deepEqual(
        positioned.map(({ span, lane, lanes }) => [span.title, lane, lanes]),
        [
            ["A", 0, 2],
            ["B", 1, 2],
            ["C", 0, 2],
            ["D", 0, 1],
        ],
    );
});
