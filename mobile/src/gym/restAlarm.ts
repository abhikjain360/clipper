import { PermissionsAndroid, Platform } from "react-native";
import type { GymSession } from "@clipper/mobile-bridge";
import { cancelRestEnd, scheduleRestEnd } from "../../modules/clipper-alarm";
import { gym, setLabel } from "./gymClient";

let notificationsAsked = false;

export async function askToNotifyRestEnd(): Promise<void> {
  if (notificationsAsked || Platform.OS !== "android" || Number(Platform.Version) < 33) return;
  notificationsAsked = true;
  try {
    const permission = PermissionsAndroid.PERMISSIONS.POST_NOTIFICATIONS;
    if (!(await PermissionsAndroid.check(permission))) await PermissionsAndroid.request(permission);
  } catch {}
}

export function armRestEnd(session: GymSession | null): void {
  try {
    const rest = session?.endedAtMillis === undefined ? session?.rest : undefined;
    if (!session || !rest || rest.endsAtMillis <= Date.now()) {
      cancelRestEnd(null);
      return;
    }
    scheduleRestEnd(session.id, rest.endsAtMillis, "Rest is over", nextSetNotice(session));
  } catch {}
}

export function stopRestEnd(sessionId: string | null): void {
  try {
    cancelRestEnd(sessionId);
  } catch {}
}

export async function armRestEndForOpenWorkout(): Promise<boolean> {
  try {
    const open = await gym().gymOpenSession();
    armRestEnd(open ?? null);
    return open !== undefined;
  } catch {
    return false;
  }
}

function nextSetNotice(session: GymSession): string {
  const current = session.exercises.find(
    (exercise) => exercise.exerciseId === session.currentExerciseId,
  );
  const kind = session.nextSetKind;
  if (!current || kind === undefined) return "Time for the next set";
  const number = current.sets.filter((set) => set.kind === kind).length + 1;
  return `Next: ${current.name}, ${setLabel(kind, number).toLowerCase()}`;
}
