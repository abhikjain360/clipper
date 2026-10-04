import type { MobileClipperClientLike } from "@clipper/mobile-bridge";
import { SetKind } from "@clipper/mobile-bridge";
import { formatSetLabel, formatSetValues } from "@clipper/shared";
import { backend } from "../backend";

export function gym(): MobileClipperClientLike {
  return backend.nativeClient();
}

export function formatSet(set: {
  weightKg?: number;
  reps?: number;
  repsInReserve?: number;
}): string {
  return formatSetValues(set.weightKg, set.reps, set.repsInReserve);
}

export function setLabel(kind: SetKind, number: number): string {
  return formatSetLabel(kind === SetKind.WarmUp, number);
}
