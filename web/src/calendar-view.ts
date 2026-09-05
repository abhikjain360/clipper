export type CalendarView = "day" | "week" | "month";

export function periodStart(date: Date, view: CalendarView): Date {
    const start = new Date(date);
    start.setHours(0, 0, 0, 0);
    if (view === "week") start.setDate(start.getDate() - ((start.getDay() + 6) % 7));
    if (view === "month") start.setDate(1);
    return start;
}

export function movePeriod(date: Date, view: CalendarView, direction: number): Date {
    const next = periodStart(date, view);
    if (view === "month") next.setMonth(next.getMonth() + direction);
    else next.setDate(next.getDate() + direction * (view === "week" ? 7 : 1));
    return next;
}

export function calendarWindow(date: Date, view: CalendarView): { start: Date; end: Date } {
    const start = periodStart(date, view === "month" ? "month" : view);
    const end = movePeriod(start, view, 1);
    if (view === "month") {
        const gridStart = periodStart(start, "week");
        const gridEnd = periodStart(end, "week");
        if (gridEnd.getTime() < end.getTime()) gridEnd.setDate(gridEnd.getDate() + 7);
        return { start: gridStart, end: gridEnd };
    }
    return { start, end };
}
