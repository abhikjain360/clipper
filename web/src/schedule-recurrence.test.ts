import assert from "node:assert/strict";
import { test } from "node:test";

import type { Recurrence } from "@clipper/shared";

import {
    buildRecurrence,
    isDerivedMonthlyRule,
    repeatChoiceOf,
    weekdaysOf,
} from "./schedule-recurrence.ts";

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

test("stored intervals select a supported pill and round-trip through the repeat controls", () => {
    const rules: Extract<Recurrence, { kind: "every" }>[] = [
        {
            kind: "every",
            frequency: { unit: "daily" },
            interval: 3,
            end: { when: "never" },
        },
        fortnightly,
        {
            kind: "every",
            frequency: { unit: "weekly", weekdays: ["mon", "tue", "wed", "thu", "fri"] },
            interval: 2,
            end: { when: "never" },
        },
        {
            kind: "every",
            frequency: { unit: "monthly", by: "on_day", from: "from_start", day: 8 },
            interval: 65_535,
            end: { when: "on", value: "2026-12-31T00:00:00Z" },
        },
    ];

    for (const rule of rules) {
        const choice = repeatChoiceOf(rule);
        assert.notEqual(choice, "custom");
        assert.deepEqual(
            buildRecurrence(choice, weekdaysOf(rule) ?? [], "2026-09-08", rule.interval, rule),
            rule,
        );
    }
});

test("unsupported rules stay custom until a supported repeat setting replaces them", () => {
    const rules: Recurrence[] = [
        secondTuesday,
        {
            kind: "every",
            frequency: { unit: "monthly", by: "on_day", from: "from_end", day: 1 },
            interval: 2,
            end: { when: "never" },
        },
        {
            kind: "every",
            frequency: { unit: "yearly", month: "march", day: { from: "from_start", day: 14 } },
            interval: 1,
            end: { when: "never" },
        },
        { kind: "imported", import: "saved-import", uid: "provider-event" },
    ];

    for (const rule of rules) {
        const choice = repeatChoiceOf(rule);
        assert.equal(choice, "custom");
        assert.deepEqual(buildRecurrence(choice, [], "2026-09-08", 1, rule), rule);
        assert.deepEqual(buildRecurrence("daily", [], "2026-09-08", 3, rule), {
            kind: "every",
            frequency: { unit: "daily" },
            interval: 3,
            end: { when: "never" },
        });
    }
});

test("repeat edits use the explicit interval across units and keep the stored end condition", () => {
    assert.deepEqual(buildRecurrence("weekly", ["tue", "thu"], "2026-09-08", 4, fortnightly), {
        kind: "every",
        frequency: { unit: "weekly", weekdays: ["tue", "thu"] },
        interval: 4,
        end: { when: "after", value: 10 },
    });
    assert.deepEqual(buildRecurrence("daily", [], "2026-09-08", 2, fortnightly), {
        kind: "every",
        frequency: { unit: "daily" },
        interval: 2,
        end: { when: "after", value: 10 },
    });
    assert.deepEqual(buildRecurrence("monthly", [], "2026-09-08", 2, fortnightly), {
        kind: "every",
        frequency: { unit: "monthly", by: "on_day", from: "from_start", day: 8 },
        interval: 2,
        end: { when: "after", value: 10 },
    });
});

test("moving a derived monthly rule moves its day and keeps its interval and end condition", () => {
    const rule = buildRecurrence("monthly", [], "2026-09-08", 2, fortnightly);
    assert.equal(isDerivedMonthlyRule(rule, "2026-09-08"), true);
    assert.equal(isDerivedMonthlyRule(rule, "2026-09-09"), false);
    assert.equal(isDerivedMonthlyRule(secondTuesday, "2026-09-08"), false);
    assert.deepEqual(buildRecurrence("monthly", [], "2026-09-09", 2, rule), {
        kind: "every",
        frequency: { unit: "monthly", by: "on_day", from: "from_start", day: 9 },
        interval: 2,
        end: { when: "after", value: 10 },
    });
});

test("once clears a stored rule and a new repeating block has no end condition", () => {
    assert.deepEqual(buildRecurrence("once", [], "2026-09-08", 2, fortnightly), { kind: "once" });
    assert.deepEqual(buildRecurrence("weekly", ["wed"], "2026-09-08", 1, null), {
        kind: "every",
        frequency: { unit: "weekly", weekdays: ["wed"] },
        interval: 1,
        end: { when: "never" },
    });
});
