export function restSecondsLeft(
  startedAtMillis: number,
  endsAtMillis: number,
  nowMillis: number,
): number {
  const remaining = Math.min(endsAtMillis - nowMillis, endsAtMillis - startedAtMillis);
  return Math.max(0, Math.ceil(remaining / 1000));
}
