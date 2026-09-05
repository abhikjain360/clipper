export type DisplaySpan = { start: string; end: string };

function dayBounds(day: Date): [number, number] {
    const start = new Date(day);
    start.setHours(0, 0, 0, 0);
    const end = new Date(start);
    end.setDate(end.getDate() + 1);
    return [start.getTime(), end.getTime()];
}

export function overlapsDay(span: DisplaySpan, day: Date, now = Date.now()): boolean {
    const [start, end] = dayBounds(day);
    return Date.parse(span.start) < end && (span.end ? Date.parse(span.end) : now) > start;
}

function wallMinutes(date: Date): number {
    return date.getHours() * 60 + date.getMinutes() + date.getSeconds() / 60;
}

/** The grid's axis uses local clock hours, including on 23- and 25-hour days. */
export function spanMinutes(span: DisplaySpan, day: Date, now = Date.now()): [number, number] {
    const [dayStart, dayEnd] = dayBounds(day);
    const start = Date.parse(span.start);
    const end = span.end ? Date.parse(span.end) : now;
    const from = start <= dayStart ? 0 : wallMinutes(new Date(start));
    const to = end >= dayEnd ? 1440 : wallMinutes(new Date(end));
    // On the repeated fall-back hour, a positive interval can end at an earlier
    // clock time. Keep it visible at its start instead of drawing backwards.
    return [from, Math.max(from, to)];
}

type Positioned<T> = { span: T; lane: number; lanes: number };

/** Give overlapping blocks separate lanes so every meeting remains visible. */
export function layoutDay<T extends DisplaySpan>(spans: T[], day: Date): Positioned<T>[] {
    const sorted = spans
        .filter((span) => overlapsDay(span, day))
        .map((span) => ({ span, minutes: spanMinutes(span, day) }))
        .toSorted((a, b) => a.minutes[0] - b.minutes[0] || b.minutes[1] - a.minutes[1]);
    const result: Positioned<T>[] = [];
    let group: Positioned<T>[] = [];
    let laneEnds: number[] = [];
    let groupEnd = -Infinity;
    function finishGroup() {
        for (const entry of group) entry.lanes = laneEnds.length;
        result.push(...group);
        group = [];
        laneEnds = [];
    }
    for (const {
        span,
        minutes: [start, end],
    } of sorted) {
        if (start >= groupEnd) finishGroup();
        let lane = laneEnds.findIndex((previousEnd) => previousEnd <= start);
        if (lane < 0) lane = laneEnds.length;
        laneEnds[lane] = end;
        group.push({ span, lane, lanes: 1 });
        groupEnd = Math.max(groupEnd, end);
    }
    finishGroup();
    return result;
}
