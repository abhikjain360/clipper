import type { ClipperBackend, OccurrenceView } from "./types.ts";

type OccurrenceId = Pick<OccurrenceView, "item_id" | "occurrence_key">;
type DoneBackend = Pick<ClipperBackend, "queryAppData" | "writeAppData">;

export type DoneMark = OccurrenceId & { done: boolean };

export function occurrenceId(occurrence: OccurrenceId): string {
  return JSON.stringify([occurrence.item_id, occurrence.occurrence_key]);
}

export function occurrenceHidden(
  occurrence: OccurrenceId & Pick<OccurrenceView, "end">,
  marks: ReadonlySet<string>,
  now: number,
): boolean {
  return marks.has(occurrenceId(occurrence)) || Date.parse(occurrence.end) <= now;
}

export async function loadDoneMarks(backend: DoneBackend): Promise<Set<string>> {
  if (!backend.queryAppData) return new Set();
  const rows = await backend.queryAppData("SELECT value FROM schedule.done");
  const marks = new Set<string>();
  for (const row of rows) {
    const value = JSON.parse(String(row.value)) as DoneMark;
    if (value.done) marks.add(occurrenceId(value));
  }
  return marks;
}

export async function writeDoneMark(
  backend: DoneBackend,
  occurrence: OccurrenceId,
  done: boolean,
): Promise<void> {
  if (!backend.writeAppData) throw new Error("Done marks are unavailable in this app");
  await backend.writeAppData("schedule.done", null, {
    value: { item_id: occurrence.item_id, occurrence_key: occurrence.occurrence_key, done },
  });
}
