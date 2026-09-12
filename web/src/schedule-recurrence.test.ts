import assert from "node:assert/strict";
import { test } from "node:test";

import type { Recurrence } from "@clipper/shared";

import { buildRecurrence, repeatChoiceOf } from "./schedule-recurrence.ts";

/// Narrow a rebuild to the repeating case so a test can read its parts. A
/// rebuild that collapsed to `once` is a failure everywhere this is used.
function repeating(recurrence: Recurrence): Extract<Recurrence, { kind: "every" }> {
    assert.equal(recurrence.kind, "every");
    if (recurrence.kind !== "every") throw new Error("unreachable");
    return recurrence;
}

const fortnightly: Recurrence = {
    kind: "every",
    frequency: { unit: "weekly", weekdays: ["tue"] },
    interval: 2,
    end: { when: "after", value: 10 },
};

const secondTuesday: Recurrence = {
    kind: "every",
    frequency: {
        unit: "monthly",
        by: "on_weekday",
        nth: { from: "from_start", nth: 2 },
        weekday: "tue",
    },
    interval: 1,
    end: { when: "never" },
};

const yearly: Recurrence = {
    kind: "every",
    frequency: { unit: "yearly", month: "march", day: { from: "from_start", day: 14 } },
    interval: 1,
    end: { when: "never" },
};

/// Rules outside the repeat row's supported choices must show as custom, so
/// choosing Once is an explicit recurrence change.
test("a cadence the row cannot express reports custom, not once", () => {
    assert.equal(repeatChoiceOf(fortnightly), "custom");
    assert.equal(repeatChoiceOf(secondTuesday), "custom");
    assert.equal(repeatChoiceOf(yearly), "custom");
    assert.equal(
        repeatChoiceOf({ kind: "imported", import: "saved-import", uid: "provider-event" }),
        "custom",
    );

    // The five it can express still round-trip.
    assert.equal(repeatChoiceOf({ kind: "once" }), "once");
    assert.equal(
        repeatChoiceOf({
            kind: "every",
            frequency: { unit: "daily" },
            interval: 1,
            end: { when: "never" },
        }),
        "daily",
    );
    assert.equal(
        repeatChoiceOf({
            kind: "every",
            frequency: {
                unit: "weekly",
                weekdays: ["mon", "tue", "wed", "thu", "fri"],
            },
            interval: 1,
            end: { when: "never" },
        }),
        "weekdays",
    );
});

/// The row has no control for when a series stops, so a rebuild must not decide
/// that it never does. This was the quiet half of the bug: toggling one weekday
/// turned "every Tuesday, ten times" into "every Tuesday, forever".
test("rebuilding carries over the end condition the row cannot show", () => {
    const edited = buildRecurrence("weekly", ["tue", "thu"], "2026-09-08", fortnightly);
    assert.deepEqual(edited, {
        kind: "every",
        frequency: { unit: "weekly", weekdays: ["tue", "thu"] },
        // Same unit, so the period count survives.
        interval: 2,
        end: { when: "after", value: 10 },
    });

    const until: Recurrence = {
        kind: "every",
        frequency: { unit: "daily" },
        interval: 1,
        end: { when: "on", value: "2026-12-31T00:00:00Z" },
    };
    assert.deepEqual(repeating(buildRecurrence("monthly", [], "2026-09-08", until)).end, {
        when: "on",
        value: "2026-12-31T00:00:00Z",
    });
});

/// An interval counts periods, so carrying "2" from weekly to daily would
/// quietly mean something new. Changing the unit is the one case where losing
/// it is the honest answer.
test("the interval survives a same-unit edit and resets when the unit changes", () => {
    assert.equal(repeating(buildRecurrence("weekdays", [], "2026-09-08", fortnightly)).interval, 2);
    assert.equal(repeating(buildRecurrence("daily", [], "2026-09-08", fortnightly)).interval, 1);
    assert.equal(repeating(buildRecurrence("monthly", [], "2026-09-08", fortnightly)).interval, 1);
});

/// Choosing Once is a real instruction and must still clear the rule; and with
/// nothing stored to carry, the defaults are what they always were.
test("once clears the rule and a fresh block gets plain defaults", () => {
    assert.deepEqual(buildRecurrence("once", [], "2026-09-08", fortnightly), { kind: "once" });
    assert.deepEqual(buildRecurrence("weekly", ["wed"], "2026-09-08", null), {
        kind: "every",
        frequency: { unit: "weekly", weekdays: ["wed"] },
        interval: 1,
        end: { when: "never" },
    });
});

/// `custom` is not selectable, so submit() never rebuilds from it. If it ever
/// did, keeping the stored rule is the only answer that invents nothing.
test("custom keeps whatever was stored", () => {
    assert.deepEqual(buildRecurrence("custom", [], "2026-09-08", secondTuesday), secondTuesday);
    assert.deepEqual(buildRecurrence("custom", [], "2026-09-08", null), { kind: "once" });
});
