import type { ActualView, OccurrenceView } from "./types.ts";

export function occurrenceRunning(
  occurrence: Pick<OccurrenceView, "item_id" | "occurrence_key">,
  running: ActualView | null | undefined,
): boolean {
  return Boolean(
    running?.running &&
    running.end === "" &&
    running.item_id !== "" &&
    running.item_id === occurrence.item_id &&
    running.occurrence_key === occurrence.occurrence_key,
  );
}
