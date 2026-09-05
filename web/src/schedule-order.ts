import type { OccurrenceView } from "@clipper/shared";

/** Ignore occurrences already in progress and cancellations; sort by next start. */
export function nextStarts(occurrences: OccurrenceView[], now: number): Map<string, number> {
    const starts = new Map<string, number>();
    for (const occurrence of occurrences) {
        const start = Date.parse(occurrence.start);
        if (occurrence.cancelled || !Number.isFinite(start) || start < now) continue;
        starts.set(occurrence.item_id, Math.min(start, starts.get(occurrence.item_id) ?? Infinity));
    }
    return starts;
}

/** Stable ties preserve the backend's newest-created order. */
export function orderByNextStart<T extends { id: string }>(
    items: T[],
    starts: Map<string, number>,
): T[] {
    return items.toSorted((a, b) => {
        const left = starts.get(a.id) ?? Infinity;
        const right = starts.get(b.id) ?? Infinity;
        return left === right ? 0 : left < right ? -1 : 1;
    });
}
