import { requireNativeModule } from "expo-modules-core";
import { Platform } from "react-native";

/**
 * One alarm for the platform to register. Mirrors `AlarmView` on the Rust side,
 * which is where the fire times are actually decided — this layer only carries
 * them across.
 */
export type PlannedAlarm = {
  itemId: string;
  /** Identifies the occurrence, so a dismissal lands on the right one. */
  occurrenceKey: string;
  /** Shown on the ring screen. Carried rather than looked up: the screen can
   * appear before the device is unlocked, when nothing encrypted is readable. */
  label: string;
  fireAtMillis: number;
  occurrenceStartMillis: number;
};

type ClipperAlarmNative = {
  // The plan crosses as JSON: Expo's typed-record marshalling relies on reified
  // generics that do not survive a list parameter, and this is already JSON on
  // both sides.
  setAlarms: (planJson: string) => number;
  cancelAll: () => void;
  canScheduleExactAlarms: () => boolean;
  areNotificationsEnabled: () => boolean;
  canUseFullScreenIntent: () => boolean;
  openExactAlarmSettings: () => boolean;
  openNotificationSettings: () => boolean;
  openFullScreenIntentSettings: () => boolean;
  plannedCount: () => number;
  ringNow: (label: string) => void;
  dismiss: () => void;
};

const native: ClipperAlarmNative | null =
  Platform.OS === "android" ? requireNativeModule<ClipperAlarmNative>("ClipperAlarm") : null;

/**
 * Whether this platform has an alarm layer at all.
 *
 * Only Android does. iOS has no equivalent of an exact alarm that survives a
 * reboot and rings through Do Not Disturb, so there is nothing honest to
 * implement there — callers should hide alarm UI rather than offer something
 * that quietly does not work.
 */
export const alarmsSupported = native !== null;

/** Replace every registered alarm. Returns how many the OS actually armed. */
export function setAlarms(alarms: PlannedAlarm[]): number {
  // Field names must match AlarmMirror.PlannedAlarm.fromJson on the Kotlin side.
  const plan = alarms.map((alarm) => ({
    item_id: alarm.itemId,
    occurrence_key: alarm.occurrenceKey,
    label: alarm.label,
    fire_at: alarm.fireAtMillis,
    start: alarm.occurrenceStartMillis,
  }));
  return native?.setAlarms(JSON.stringify(plan)) ?? 0;
}

export function cancelAllAlarms(): void {
  native?.cancelAll();
}

/**
 * Whether the OS permits exact scheduling. Other permissions still determine
 * whether the alarm can notify and present its full-screen ring UI.
 */
export function canScheduleExactAlarms(): boolean {
  return native?.canScheduleExactAlarms() ?? false;
}

export function openExactAlarmSettings(): boolean {
  return native?.openExactAlarmSettings() ?? false;
}

export function areNotificationsEnabled(): boolean {
  return native?.areNotificationsEnabled() ?? false;
}

export function canUseFullScreenIntent(): boolean {
  return native?.canUseFullScreenIntent() ?? false;
}

export function openNotificationSettings(): boolean {
  return native?.openNotificationSettings() ?? false;
}

export function openFullScreenIntentSettings(): boolean {
  return native?.openFullScreenIntentSettings() ?? false;
}

/** How many alarms the device-protected mirror holds. Diagnostics only. */
export function plannedAlarmCount(): number {
  return native?.plannedCount() ?? 0;
}

/**
 * Ring immediately, for exercising the whole path — permissions, foreground
 * service, full-screen intent — without waiting for a real alarm time.
 */
export function ringNow(label: string): void {
  native?.ringNow(label);
}

export function dismissAlarm(): void {
  native?.dismiss();
}
