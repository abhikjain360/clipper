import type { MobileClipperClientLike } from "@clipper/mobile-bridge";
import { SetKind } from "@clipper/mobile-bridge";
import { backend } from "../backend";

export function gym(): MobileClipperClientLike {
  return backend.nativeClient();
}

export function formatKg(kg: number | undefined): string {
  if (kg === undefined) return "–";
  return String(Math.round(kg * 100) / 100);
}

export function formatSet(set: {
  weightKg?: number;
  reps?: number;
  repsInReserve?: number;
}): string {
  const weight = set.weightKg === undefined ? "BW" : `${formatKg(set.weightKg)} kg`;
  const reps = set.reps === undefined ? "–" : `${set.reps}`;
  const reserve = set.repsInReserve === undefined ? "" : ` @ ${set.repsInReserve}`;
  return `${weight} × ${reps}${reserve}`;
}

export function formatClock(milliseconds: number): string {
  const seconds = Math.max(0, Math.floor(milliseconds / 1000));
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const rest = String(seconds % 60).padStart(2, "0");
  return hours > 0 ? `${hours}:${String(minutes).padStart(2, "0")}:${rest}` : `${minutes}:${rest}`;
}

export function formatMinutes(milliseconds: number): string {
  const minutes = Math.floor(Math.max(0, milliseconds) / 60_000);
  if (minutes < 60) return `${minutes} min`;
  return `${Math.floor(minutes / 60)} h ${minutes % 60} min`;
}

export function formatRestSeconds(seconds: number): string {
  return formatClock(seconds * 1000);
}

export function formatDay(millis: number): string {
  return new Date(millis).toLocaleDateString(undefined, {
    weekday: "short",
    month: "short",
    day: "numeric",
    year: "numeric",
  });
}

export function formatTime(millis: number): string {
  return new Date(millis).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
}

export function setLabel(kind: SetKind, number: number): string {
  return kind === SetKind.WarmUp ? `Warm-up ${number}` : `Set ${number}`;
}

export function parseWeight(text: string): number | undefined {
  const value = Number.parseFloat(text.replace(",", ".").trim());
  return Number.isFinite(value) && value > 0 ? value : undefined;
}

export function parseCount(text: string): number | undefined {
  const value = Number.parseInt(text.trim(), 10);
  return Number.isFinite(value) && value >= 0 ? value : undefined;
}
