import type { CalendarSourceView } from "./types";

export function calendarRefreshDue(source: CalendarSourceView, now = Date.now()): boolean {
  if (!source.enabled) return false;
  const checked = source.checked_at ? Date.parse(source.checked_at) : Number.NaN;
  return (
    !Number.isFinite(checked) || now - checked > 60 * 60 * 1000 || checked - now > 5 * 60 * 1000
  );
}

export function calendarSyncLabel(checkedAt: string | null, now = Date.now()): string {
  if (!checkedAt) return "Never synced";
  const checked = Date.parse(checkedAt);
  if (!Number.isFinite(checked)) return "Never synced";
  const minutes = Math.max(0, Math.floor((now - checked) / 60000));
  if (minutes < 1) return "Last synced just now";
  if (minutes < 60) return `Last synced ${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `Last synced ${hours}h ago`;
  return `Last synced ${Math.floor(hours / 24)}d ago`;
}
