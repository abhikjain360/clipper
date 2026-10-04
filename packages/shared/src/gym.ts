export type GymMuscle =
  | "chest"
  | "shoulders"
  | "triceps"
  | "lats"
  | "upper_back"
  | "biceps"
  | "forearms"
  | "quadriceps"
  | "hamstrings"
  | "glutes"
  | "calves"
  | "abs"
  | "obliques"
  | "erectors";

export type GymMuscleGroup = "push" | "pull" | "legs" | "core";

export type GymSetKind = "warm_up" | "working";

export type GymFatigueBand = "recovered" | "low" | "moderate" | "high" | "very_high";

export type GymStarterLibrary = "written" | "not_needed" | "waiting_for_download";

export type GymMuscleInfo = {
  muscle: GymMuscle;
  display_name: string;
  group: GymMuscleGroup;
  default_recovery_days: number;
};

export type GymMuscleShare = {
  muscle: GymMuscle;
  share: number;
};

export type GymExercise = {
  id: string;
  name: string;
  muscles: GymMuscleShare[];
  archived: boolean;
};

export type GymExerciseInput = {
  name: string;
  muscles: GymMuscleShare[];
  archived: boolean;
};

export type GymPlannedExercise = {
  exercise_id: string;
  warm_up_sets: number;
  warm_up_rest_seconds: number;
  target_sets: number;
  target_reps: number;
  target_reps_in_reserve: number | null;
  rest_seconds: number;
  superset_with_previous: boolean;
};

export type GymTemplate = {
  id: string;
  name: string;
  exercises: GymPlannedExercise[];
  archived: boolean;
};

export function activeGymItems<T extends { archived?: boolean }>(items: readonly T[]): T[] {
  return items.filter((item) => !item.archived);
}

export type GymSet = {
  id: string;
  exercise_id: string;
  order: number;
  kind: GymSetKind;
  weight_kg: number | null;
  reps: number | null;
  reps_in_reserve: number | null;
  completed_at_millis: number;
  estimated_one_rep_max_kg: number | null;
};

export type GymSessionExercise = {
  exercise_id: string;
  name: string;
  planned: boolean;
  plan_index: number | null;
  warm_up_sets: number;
  warm_up_rest_seconds: number;
  target_sets: number;
  target_reps: number;
  target_reps_in_reserve: number | null;
  rest_seconds: number;
  superset_with_previous: boolean;
  skipped: boolean;
  done: boolean;
  sets: GymSet[];
  last_time: GymSet[];
};

export type GymSetValues = {
  weight_kg: number | null;
  reps: number | null;
  reps_in_reserve: number | null;
};

export type GymRest = {
  exercise_id: string;
  started_at_millis: number;
  ends_at_millis: number;
};

export type GymSession = {
  id: string;
  name: string;
  started_at_millis: number;
  ended_at_millis: number | null;
  exercises: GymSessionExercise[];
  current_exercise_id: string | null;
  next_set_kind: GymSetKind | null;
  next_set_order: number;
  prefill: GymSetValues | null;
  rest: GymRest | null;
};

export type GymSessionSummary = {
  id: string;
  name: string;
  started_at_millis: number;
  ended_at_millis: number | null;
  exercise_names: string[];
  working_sets: number;
};

export type GymBodyWeight = {
  id: string;
  time_millis: number;
  kg: number;
};

export type GymWeeklyBodyWeight = {
  week_start: string;
  average_kg: number;
  measurements: number;
  change_kg: number | null;
};

export type GymOneRepMax = {
  session_id: string;
  started_at_millis: number;
  kg: number;
};

export type GymMuscleFatigue = {
  muscle: GymMuscle;
  display_name: string;
  group: GymMuscleGroup;
  score: number;
  band: GymFatigueBand;
  recovery_days: number;
  default_recovery_days: number;
};

export type GymChange =
  | { change: "save_exercise"; id: string | null; exercise: GymExerciseInput }
  | { change: "save_template"; id: string | null; name: string; exercises: GymPlannedExercise[] }
  | { change: "delete_template"; id: string }
  | { change: "archive_template"; id: string; archived: boolean }
  | { change: "archive_exercise"; id: string; archived: boolean }
  | { change: "start_session"; template_id: string | null }
  | {
      change: "complete_set";
      session_id: string;
      exercise_id: string;
      kind: GymSetKind;
      expected_order: number;
      values: GymSetValues;
    }
  | {
      change: "add_set";
      session_id: string;
      exercise_id: string;
      kind: GymSetKind;
      values: GymSetValues;
    }
  | { change: "edit_set"; set_id: string; values: GymSetValues }
  | { change: "delete_set"; set_id: string }
  | { change: "delete_session"; session_id: string }
  | { change: "add_exercise"; session_id: string; exercise_id: string }
  | { change: "move_exercise"; session_id: string; exercise_id: string; to_index: number }
  | { change: "switch_exercise"; session_id: string; exercise_id: string }
  | { change: "skip_exercise"; session_id: string; exercise_id: string; skipped: boolean }
  | { change: "add_working_set"; session_id: string; exercise_id: string }
  | { change: "add_warm_up_set"; session_id: string; exercise_id: string }
  | { change: "finish_session"; session_id: string }
  | { change: "add_body_weight"; kg: number }
  | { change: "delete_body_weight"; id: string }
  | { change: "set_recovery_days"; muscle: GymMuscle; recovery_days: number };

export type GymBackend = {
  muscles: () => Promise<GymMuscleInfo[]>;
  moveTemplateExercise: (
    exercises: GymPlannedExercise[],
    from: number,
    to: number,
  ) => Promise<GymPlannedExercise[]>;
  seedStarterLibrary: () => Promise<GymStarterLibrary>;
  exercises: () => Promise<GymExercise[]>;
  templates: () => Promise<GymTemplate[]>;
  openSession: () => Promise<GymSession | null>;
  session: (sessionId: string) => Promise<GymSession>;
  sessions: () => Promise<GymSessionSummary[]>;
  bodyWeights: () => Promise<GymBodyWeight[]>;
  weeklyBodyWeight: (zone: string) => Promise<GymWeeklyBodyWeight[]>;
  oneRepMaxProgress: (exerciseId: string) => Promise<GymOneRepMax[]>;
  fatigue: () => Promise<GymMuscleFatigue[]>;
  change: (change: GymChange) => Promise<void>;
  scheduleRestEnd: (endsAtMillis: number, title: string, body: string) => Promise<void>;
  cancelRestEnd: () => Promise<void>;
};

export function formatKg(kg: number | null | undefined): string {
  if (kg === null || kg === undefined) return "–";
  return String(Math.round(kg * 100) / 100);
}

export function formatSetValues(
  weightKg: number | null | undefined,
  reps: number | null | undefined,
  repsInReserve: number | null | undefined,
): string {
  const weight = weightKg === null || weightKg === undefined ? "BW" : `${formatKg(weightKg)} kg`;
  const count = reps === null || reps === undefined ? "–" : `${reps}`;
  const reserve =
    repsInReserve === null || repsInReserve === undefined ? "" : ` @ ${repsInReserve}`;
  return `${weight} × ${count}${reserve}`;
}

export function formatSetLabel(warmUp: boolean, number: number): string {
  return warmUp ? `Warm-up ${number}` : `Set ${number}`;
}

export function restEndNotice(
  next: { name: string; warmUp: boolean; number: number } | null,
): string {
  if (!next) return "Time for the next set";
  return `Next: ${next.name}, ${formatSetLabel(next.warmUp, next.number).toLowerCase()}`;
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

export function parseWeight(text: string): number | undefined {
  const value = Number.parseFloat(text.replace(",", ".").trim());
  return Number.isFinite(value) && value > 0 ? value : undefined;
}

export function parseCount(text: string): number | undefined {
  const value = Number.parseInt(text.trim(), 10);
  return Number.isFinite(value) && value >= 0 ? value : undefined;
}

export function deviceZone(): string {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
  } catch {
    return "UTC";
  }
}

export function localDate(isoDate: string): Date {
  const [year, month, day] = isoDate.split("-").map(Number);
  return new Date(year ?? 1970, (month ?? 1) - 1, day ?? 1);
}

export function formatShortDate(date: Date): string {
  return date.toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

export function restSecondsLeft(
  startedAtMillis: number,
  endsAtMillis: number,
  nowMillis: number,
): number {
  const remaining = Math.min(endsAtMillis - nowMillis, endsAtMillis - startedAtMillis);
  return Math.max(0, Math.ceil(remaining / 1000));
}
