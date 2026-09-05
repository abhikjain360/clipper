import type { Frequency, Recurrence, RecurrenceEnd, Weekday } from "@clipper/shared";

// The repeat row of the schedule composer, kept out of the component so it can
// be tested directly. Every function here is pure: a stored `Recurrence` in, a
// form choice out, and back again.

export type RepeatChoice = "once" | "daily" | "weekly" | "weekdays" | "monthly";

/// What the repeat row is showing, which is not always something it can offer.
///
/// A stored rule can be an every-two-weeks, a "second Tuesday", a yearly, or a
/// provider rule Clipper only passes through. The row has no pill for any of
/// those, and showing "Once" would claim the event never repeats. `custom`
/// says the stored cadence is outside these choices. Picking a supported
/// cadence then replaces the stored rule.
export type RepeatSelection = RepeatChoice | "custom";

/// Maps a stored recurrence onto the form's coarser choices.
///
/// The form offers a handful of common cadences. A record can express more.
/// Anything outside the handful reports `custom` instead of collapsing to
/// `once`, because the pill row is live and those two say different things.
/// The stored recurrence is kept until the repeat settings change, and the
/// composer shows its full summary alongside.
export function repeatChoiceOf(recurrence: Recurrence): RepeatSelection {
    if (recurrence.kind === "once") return "once";
    if (recurrence.kind !== "every" || recurrence.interval !== 1) return "custom";
    switch (recurrence.frequency.unit) {
        case "daily":
            return "daily";
        case "weekly":
            return isWeekdaySet(recurrence.frequency.weekdays) ? "weekdays" : "weekly";
        case "monthly":
            return recurrence.frequency.by === "on_day" ? "monthly" : "custom";
        default:
            return "custom";
    }
}

export function weekdaysOf(recurrence: Recurrence): Weekday[] | null {
    return recurrence.kind === "every" && recurrence.frequency.unit === "weekly"
        ? recurrence.frequency.weekdays
        : null;
}

export function isWeekdaySet(days: Weekday[]): boolean {
    const workweek: Weekday[] = ["mon", "tue", "wed", "thu", "fri"];
    return days.length === workweek.length && workweek.every((day) => days.includes(day));
}

/// Rebuilds the recurrence from the repeat row's coarser controls.
///
/// The row names a cadence unit, plus the weekdays for a weekly one. It has no
/// control for the rest of a stored rule, such as when the rule stops. Those
/// fields carry over from `previous` rather than reset, so toggling one weekday
/// cannot drop an end date the form never showed.
///
/// `interval` carries over only when the unit stays the same. A fortnightly
/// series stays fortnightly when its weekdays move, and resets when the unit
/// changes, where keeping "2" would mean something else entirely.
export function buildRecurrence(
    choice: RepeatSelection,
    days: Weekday[],
    date: string,
    previous: Recurrence | null,
): Recurrence {
    const carried = previous?.kind === "every" ? previous : null;
    const end: RecurrenceEnd = carried?.end ?? { when: "never" };
    const intervalFor = (unit: Frequency["unit"]): number =>
        carried?.frequency.unit === unit ? carried.interval : 1;

    switch (choice) {
        // submit() never reaches this: it keeps the stored rule whenever the
        // repeat row was left alone, and the row cannot select `custom`.
        // Keeping the rule anyway beats inventing a cadence nobody picked.
        case "custom":
            return previous ?? { kind: "once" };
        case "once":
            return { kind: "once" };
        case "daily":
            return {
                kind: "every",
                frequency: { unit: "daily" },
                interval: intervalFor("daily"),
                end,
            };
        case "weekdays":
            return {
                kind: "every",
                frequency: {
                    unit: "weekly",
                    weekdays: ["mon", "tue", "wed", "thu", "fri"],
                },
                interval: intervalFor("weekly"),
                end,
            };
        case "weekly":
            return {
                kind: "every",
                frequency: { unit: "weekly", weekdays: days },
                interval: intervalFor("weekly"),
                end,
            };
        case "monthly":
            return {
                kind: "every",
                frequency: {
                    unit: "monthly",
                    by: "on_day",
                    from: "from_start",
                    // The day the block starts on, so "monthly" means "this
                    // date every month" without a second question.
                    day: Number.parseInt(date.slice(8, 10), 10) || 1,
                },
                interval: intervalFor("monthly"),
                end,
            };
    }
}

export function repeatLabel(choice: RepeatSelection): string {
    switch (choice) {
        case "custom":
            return "Custom";
        case "once":
            return "Once";
        case "daily":
            return "Daily";
        case "weekdays":
            return "Weekdays";
        case "weekly":
            return "Weekly";
        case "monthly":
            return "Monthly";
    }
}
