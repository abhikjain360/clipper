import type { Frequency, Recurrence, RecurrenceEnd, Weekday } from "@clipper/shared";

// The repeat row of the schedule composer, kept out of the component so it can
// be tested directly. Everything here is pure: a stored `Recurrence` in, a form
// choice out, and back again.

export type RepeatChoice = "once" | "daily" | "weekly" | "weekdays" | "monthly";

/// What the repeat row is showing, which is not always something it can offer.
///
/// A stored rule can be an every-two-weeks, a "second Tuesday", a yearly, or a
/// provider rule Clipper only passes through. None of those is a pill, and
/// calling them "Once" would be a lie the owner could act on — the Once pill
/// would sit highlighted, and pressing it would look like a no-op while
/// flattening the series. `custom` says the truth instead: the stored cadence
/// is not one of these choices, and picking one replaces it.
export type RepeatSelection = RepeatChoice | "custom";

/// Map a stored recurrence back onto the form's coarser choices.
///
/// The form offers a handful of common cadences while the record can express
/// more. Anything outside that handful reports `custom` rather than collapsing
/// to `once`: the two are not the same claim, and the pill row is live. The
/// original recurrence is retained until repeat settings are changed; the
/// composer also shows its full stored summary.
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

/// Rebuild the recurrence from the repeat row's coarser controls.
///
/// The row names a cadence unit, and for weekly its weekdays. Everything else a
/// stored rule carries — when it stops, which day its week starts on, how many
/// periods it skips — has no control here, so it is carried over from
/// `previous` instead of reset. Losing an end date because someone toggled one
/// weekday is data loss about a field the form never showed.
///
/// `interval` is the exception, because it counts periods and the period is
/// exactly what changed. It survives an edit that keeps the unit, so a
/// fortnightly series stays fortnightly when its weekdays move, and resets when
/// the unit itself changes, where carrying "2" would silently mean something
/// new.
export function buildRecurrence(
    choice: RepeatSelection,
    days: Weekday[],
    date: string,
    previous: Recurrence | null,
): Recurrence {
    const carried = previous?.kind === "every" ? previous : null;
    const end: RecurrenceEnd = carried?.end ?? { when: "never" };
    const weekStart: Weekday =
        carried?.frequency.unit === "weekly" ? carried.frequency.week_start : "mon";
    const intervalFor = (unit: Frequency["unit"]): number =>
        carried?.frequency.unit === unit ? carried.interval : 1;

    switch (choice) {
        // Not reachable from submit(), which keeps the stored rule whenever the
        // repeat row was left alone, and the row cannot select `custom`. Keeping
        // the rule anyway beats inventing a cadence nobody picked.
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
                    week_start: weekStart,
                },
                interval: intervalFor("weekly"),
                end,
            };
        case "weekly":
            return {
                kind: "every",
                frequency: { unit: "weekly", weekdays: days, week_start: weekStart },
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
                    // The day the block starts on, so "monthly" means "this date
                    // every month" without asking a second question.
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
