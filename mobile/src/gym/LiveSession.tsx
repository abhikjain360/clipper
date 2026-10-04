import { Button } from "../tamagui.config";
import { palette } from "@clipper/shared";
import { ChevronDown, ChevronUp, Play, Plus, RotateCcw, SkipForward } from "lucide-react-native";
import { useCallback, useEffect, useRef, useState } from "react";
import { Alert, Keyboard, AppState as NativeAppState } from "react-native";
import { H2, ScrollView, Spinner, Text, XStack, YStack } from "tamagui";
import {
  SetKind,
  type GymExercise,
  type GymSession,
  type GymSessionExercise,
  type GymSet,
  type GymTemplate,
} from "@clipper/mobile-bridge";
import { formatBackendError } from "../backend";
import {
  formatClock,
  formatKg,
  formatMinutes,
  formatRestSeconds,
  formatSet,
  gym,
  parseCount,
  parseWeight,
  formatTime,
  setLabel,
} from "./gymClient";
import {
  colors,
  ExercisePicker,
  GymCard,
  Muted,
  NumberEntry,
  ReserveChips,
  SetEditor,
} from "./GymUi";
import { armRestEnd, askToNotifyRestEnd, stopRestEnd } from "./restAlarm";
import { restSecondsLeft } from "./restCountdown";

export function LiveSession({ onError }: { onError: (error: string | null) => void }) {
  const [view, setView] = useState<GymSession | null | undefined>(undefined);
  const [loadFailed, setLoadFailed] = useState(false);
  const [templates, setTemplates] = useState<GymTemplate[]>([]);
  const [exercises, setExercises] = useState<GymExercise[]>([]);
  const [now, setNow] = useState(Date.now);
  const [busy, setBusy] = useState(false);
  const [picking, setPicking] = useState(false);
  const [editing, setEditing] = useState<{ set: GymSet; number: number } | null>(null);
  const [weight, setWeight] = useState("");
  const [reps, setReps] = useState("");
  const [reserve, setReserve] = useState<number | undefined>(undefined);
  const busyRef = useRef(false);
  const loadGeneration = useRef(0);
  const viewRef = useRef(view);
  viewRef.current = view;

  const load = useCallback(async () => {
    const generation = ++loadGeneration.current;
    try {
      const [open, templateList, exerciseList] = await Promise.all([
        gym().gymOpenSession(),
        gym().gymTemplates(),
        gym().gymExercises(),
      ]);
      if (generation !== loadGeneration.current) return;
      setNow(Date.now());
      setView(open ?? null);
      setTemplates(templateList);
      setExercises(exerciseList);
      setLoadFailed(false);
    } catch (caught) {
      if (generation !== loadGeneration.current) return;
      onError(formatBackendError(caught));
      setLoadFailed(true);
    }
  }, [onError]);

  useEffect(() => {
    void load();
    const timer = setInterval(() => setNow(Date.now()), 1000);
    const subscription = NativeAppState.addEventListener("change", (nextState) => {
      if (nextState === "active") {
        setNow(Date.now());
        void load();
      }
    });
    return () => {
      loadGeneration.current += 1;
      clearInterval(timer);
      subscription.remove();
    };
  }, [load]);

  const current = view?.exercises.find(
    (exercise) => exercise.exerciseId === view.currentExerciseId,
  );
  const nextKind = view?.nextSetKind;
  const nextNumber =
    current && nextKind !== undefined ? countOfKind(current.sets, nextKind) + 1 : 0;
  const pendingKey =
    view && current && nextKind !== undefined
      ? `${view.id}:${view.nextSetOrder}:${current.exerciseId}:${nextKind}`
      : null;

  useEffect(() => {
    const prefill = viewRef.current?.prefill;
    if (!pendingKey) return;
    setWeight(prefill?.weightKg === undefined ? "" : formatKg(prefill.weightKg));
    setReps(prefill?.reps === undefined ? "" : String(prefill.reps));
    setReserve(prefill?.repsInReserve);
  }, [pendingKey]);

  useEffect(() => {
    if (view !== undefined) armRestEnd(view);
  }, [view]);

  async function run(action: () => Promise<unknown>) {
    if (busyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    onError(null);
    Keyboard.dismiss();
    try {
      await action();
    } catch (caught) {
      onError(formatBackendError(caught));
    } finally {
      await load();
      busyRef.current = false;
      setBusy(false);
    }
  }

  function completeSet() {
    if (!view || !current || nextKind === undefined) return;
    const sessionId = view.id;
    const exerciseId = current.exerciseId;
    const order = view.nextSetOrder;
    const values = {
      weightKg: parseWeight(weight),
      reps: parseCount(reps),
      repsInReserve: reserve,
    };
    void askToNotifyRestEnd();
    void run(async () =>
      armRestEnd(await gym().gymCompleteSet(sessionId, exerciseId, nextKind, order, values)),
    );
  }

  function finish(session: GymSession) {
    Alert.alert("Finish workout?", "The workout moves to History.", [
      { text: "Cancel", style: "cancel" },
      {
        text: "Finish",
        onPress: () =>
          void run(async () => {
            await gym().gymFinishSession(session.id);
            stopRestEnd(session.id);
          }),
      },
    ]);
  }

  if (view === undefined) {
    return (
      <YStack flex={1} items="center" justify="center" gap="$3">
        {loadFailed ? (
          <>
            <Muted>The workout could not be loaded.</Muted>
            <Button onPress={() => void load()}>Try again</Button>
          </>
        ) : (
          <Spinner />
        )}
      </YStack>
    );
  }

  if (view === null) {
    return (
      <StartWorkout
        templates={templates}
        exercises={exercises}
        busy={busy}
        onStart={(templateId) => {
          void askToNotifyRestEnd();
          void run(() => gym().gymStartSession(templateId));
        }}
      />
    );
  }

  const session = view;
  const planIds = new Set(
    session.exercises
      .filter((exercise) => exercise.planned && !exercise.skipped)
      .map((exercise) => exercise.exerciseId),
  );
  const inPlanOrder: GymSessionExercise[] = [];
  for (const exercise of session.exercises) {
    if (exercise.planIndex !== undefined) inPlanOrder[exercise.planIndex] = exercise;
  }
  const unfinished = inPlanOrder.filter(
    (exercise) => !exercise.done && exercise.exerciseId !== session.currentExerciseId,
  );

  return (
    <ScrollView flex={1} keyboardShouldPersistTaps="always">
      <YStack gap="$3" pb="$8">
        <XStack items="center" justify="space-between" gap="$2">
          <YStack flex={1}>
            <H2 size="$6" numberOfLines={1}>
              {session.name}
            </H2>
            <Muted>{`Started ${formatTime(session.startedAtMillis)} · ${formatMinutes(now - session.startedAtMillis)}`}</Muted>
          </YStack>
          <Button disabled={busy} onPress={() => finish(session)}>
            Finish
          </Button>
        </XStack>

        {session.rest && (
          <RestCard
            startedAt={session.rest.startedAtMillis}
            endsAt={session.rest.endsAtMillis}
            now={now}
          />
        )}

        {current && nextKind !== undefined ? (
          <GymCard highlighted>
            <YStack gap="$3">
              <YStack gap="$1">
                <Text fontSize={20} fontWeight="700">
                  {current.name}
                </Text>
                <SupersetNote exercises={session.exercises} exerciseId={current.exerciseId} />
                <Muted>{targetSummary(current)}</Muted>
                {current.lastTime.length > 0 && (
                  <Muted>Last time: {current.lastTime.map(formatSet).join(", ")}</Muted>
                )}
              </YStack>
              <Text
                fontSize={16}
                fontWeight="600"
                color={nextKind === SetKind.WarmUp ? colors.warmUp : colors.accent}
              >
                Next: {setLabel(nextKind, nextNumber)}
              </Text>
              <NumberEntry
                label="Weight (kg)"
                value={weight}
                onChange={setWeight}
                step={2.5}
                decimal
                placeholder="BW"
              />
              <NumberEntry label="Reps" value={reps} onChange={setReps} step={1} placeholder="–" />
              <ReserveChips value={reserve} onChange={setReserve} />
              <Button
                theme="green"
                size="$6"
                disabled={busy}
                icon={busy ? <Spinner /> : undefined}
                onPress={completeSet}
              >
                {`Complete ${setLabel(nextKind, nextNumber).toLowerCase()}`}
              </Button>
              <XStack gap="$2" flexWrap="wrap">
                {countOfKind(current.sets, SetKind.Working) === 0 && (
                  <Button
                    size="$3"
                    disabled={busy}
                    onPress={() =>
                      void run(() => gym().gymAddWarmUpSet(session.id, current.exerciseId))
                    }
                  >
                    Add warm-up
                  </Button>
                )}
                <Button
                  size="$3"
                  disabled={busy}
                  onPress={() =>
                    void run(() => gym().gymAddWorkingSet(session.id, current.exerciseId))
                  }
                >
                  Add set
                </Button>
                <Button
                  size="$3"
                  disabled={busy}
                  onPress={() =>
                    void run(() => gym().gymSkipExercise(session.id, current.exerciseId, true))
                  }
                >
                  Skip exercise
                </Button>
              </XStack>
              <SetRows
                exercise={current}
                nextKind={nextKind}
                onEdit={(set, number) => setEditing({ set, number })}
              />
            </YStack>
          </GymCard>
        ) : (
          <GymCard>
            <YStack gap="$2">
              <Text fontWeight="600">
                {session.exercises.length === 0 ? "No exercises yet" : "Every planned set is done"}
              </Text>
              <Muted>Add an exercise or a set below, or finish the workout.</Muted>
            </YStack>
          </GymCard>
        )}

        <YStack gap="$2">
          <XStack items="center" justify="space-between">
            <Text fontWeight="600">Exercises</Text>
            <Button
              size="$3"
              icon={<Plus size={16} />}
              disabled={busy}
              onPress={() => setPicking(true)}
            >
              Add exercise
            </Button>
          </XStack>
          {session.exercises.map((exercise) => {
            const position = unfinished.indexOf(exercise);
            return (
              <PlanRow
                key={exercise.exerciseId}
                exercise={exercise}
                isCurrent={exercise.exerciseId === session.currentExerciseId}
                moveUpTo={position > 0 ? unfinished[position - 1]?.planIndex : undefined}
                moveDownTo={position >= 0 ? unfinished[position + 1]?.planIndex : undefined}
                canDoNow={
                  exercise.planned &&
                  !exercise.done &&
                  exercise.exerciseId !== session.currentExerciseId
                }
                busy={busy}
                onMove={(to) =>
                  void run(() => gym().gymMoveExercise(session.id, exercise.exerciseId, to))
                }
                onDoNow={() =>
                  void run(() => gym().gymSwitchExercise(session.id, exercise.exerciseId))
                }
                onSkip={(skipped) =>
                  void run(() => gym().gymSkipExercise(session.id, exercise.exerciseId, skipped))
                }
                onAddSet={() =>
                  void run(() => gym().gymAddWorkingSet(session.id, exercise.exerciseId))
                }
                onEdit={(set, number) => setEditing({ set, number })}
              />
            );
          })}
        </YStack>
      </YStack>

      <ExercisePicker
        visible={picking}
        exercises={exercises}
        excluded={planIds}
        onClose={() => setPicking(false)}
        onPick={(exercise) => {
          setPicking(false);
          void run(() => gym().gymAddExercise(session.id, exercise.id));
        }}
      />
      <SetEditor
        set={editing?.set ?? null}
        number={editing?.number ?? 0}
        onClose={() => setEditing(null)}
        onSave={(values) => {
          const set = editing?.set;
          setEditing(null);
          if (set) void run(() => gym().gymEditSet(set.id, values));
        }}
        onDelete={() => {
          const set = editing?.set;
          setEditing(null);
          if (set) confirmDeleteSet(() => void run(() => gym().gymDeleteSet(set.id)));
        }}
      />
    </ScrollView>
  );
}

export function confirmDeleteSet(onDelete: () => void) {
  Alert.alert("Delete this set?", "It is removed on every device.", [
    { text: "Cancel", style: "cancel" },
    { text: "Delete", style: "destructive", onPress: onDelete },
  ]);
}

function StartWorkout({
  templates,
  exercises,
  busy,
  onStart,
}: {
  templates: GymTemplate[];
  exercises: GymExercise[];
  busy: boolean;
  onStart: (templateId: string | undefined) => void;
}) {
  const names = new Map(exercises.map((exercise) => [exercise.id, exercise.name]));
  return (
    <ScrollView flex={1} keyboardShouldPersistTaps="always">
      <YStack gap="$3" pb="$8">
        <H2 size="$6">Start a workout</H2>
        {templates.length === 0 && <Muted>No workouts yet. Add one in the Library.</Muted>}
        {templates.map((template) => (
          <GymCard key={template.id}>
            <YStack gap="$2">
              <Text fontSize={17} fontWeight="600">
                {template.name}
              </Text>
              <Muted>
                {template.exercises
                  .map((planned) => names.get(planned.exerciseId) ?? "Unknown exercise")
                  .join(", ")}
              </Muted>
              <Button
                theme="blue"
                size="$5"
                icon={<Play size={18} />}
                disabled={busy}
                onPress={() => onStart(template.id)}
              >
                {`Start ${template.name}`}
              </Button>
            </YStack>
          </GymCard>
        ))}
        <Button size="$5" disabled={busy} onPress={() => onStart(undefined)}>
          Start an empty workout
        </Button>
      </YStack>
    </ScrollView>
  );
}

function RestCard({ startedAt, endsAt, now }: { startedAt: number; endsAt: number; now: number }) {
  const remaining = endsAt - now;
  const resting = remaining > 0;
  return (
    <GymCard>
      <XStack items="center" justify="space-between">
        <YStack>
          <Text color={colors.muted} fontSize={13}>
            {resting ? "Rest" : "Rest is over"}
          </Text>
          <Muted>{`Target ${formatClock(endsAt - startedAt)}`}</Muted>
        </YStack>
        <Text
          fontSize={resting ? 44 : 28}
          fontWeight="700"
          color={resting ? colors.accent : colors.good}
          aria-label={resting ? "Rest remaining" : "Time since rest ended"}
        >
          {resting
            ? formatClock(restSecondsLeft(startedAt, endsAt, now) * 1000)
            : `+${formatMinutes(-remaining)}`}
        </Text>
      </XStack>
    </GymCard>
  );
}

function SupersetNote({
  exercises,
  exerciseId,
}: {
  exercises: GymSessionExercise[];
  exerciseId: string;
}) {
  const planned: GymSessionExercise[] = [];
  for (const exercise of exercises) {
    if (exercise.planIndex !== undefined) planned[exercise.planIndex] = exercise;
  }
  const index = planned.findIndex((exercise) => exercise.exerciseId === exerciseId);
  if (index < 0) return null;
  let start = index;
  while (start > 0 && planned[start]?.supersetWithPrevious) start -= 1;
  let end = index;
  while (planned[end + 1]?.supersetWithPrevious) end += 1;
  if (start === end) return null;
  const partners = planned
    .slice(start, end + 1)
    .filter((_, offset) => start + offset !== index)
    .map((exercise) => exercise.name);
  return <Text color={colors.accent}>Superset with {partners.join(", ")}</Text>;
}

function SetRows({
  exercise,
  nextKind,
  onEdit,
}: {
  exercise: GymSessionExercise;
  nextKind: SetKind | undefined;
  onEdit: (set: GymSet, number: number) => void;
}) {
  const numbers = new Map<SetKind, number>();
  const done = exercise.sets.map((set) => {
    const number = (numbers.get(set.kind) ?? 0) + 1;
    numbers.set(set.kind, number);
    return { set, number };
  });
  const warmUpsLeft = Math.max(0, exercise.warmUpSets - (numbers.get(SetKind.WarmUp) ?? 0));
  const workingLeft = Math.max(0, exercise.targetSets - (numbers.get(SetKind.Working) ?? 0));
  const planned: { kind: SetKind; number: number }[] = [
    ...Array.from({ length: warmUpsLeft }, (_, offset) => ({
      kind: SetKind.WarmUp,
      number: (numbers.get(SetKind.WarmUp) ?? 0) + offset + 1,
    })),
    ...Array.from({ length: workingLeft }, (_, offset) => ({
      kind: SetKind.Working,
      number: (numbers.get(SetKind.Working) ?? 0) + offset + 1,
    })),
  ];
  return (
    <YStack gap="$1">
      {done.map(({ set, number }) => (
        <XStack
          key={set.id}
          items="center"
          justify="space-between"
          px="$2"
          py="$2"
          rounded="$2"
          bg={palette.raised}
          onPress={() => onEdit(set, number)}
          pressStyle={{ bg: palette.raised }}
          aria-label={`${setLabel(set.kind, number)} done`}
        >
          <Text color={set.kind === SetKind.WarmUp ? colors.warmUp : undefined}>
            {setLabel(set.kind, number)}
          </Text>
          <Text>{formatSet(set)}</Text>
        </XStack>
      ))}
      {planned.map(({ kind, number }, offset) => (
        <XStack
          key={`${kind}-${number}`}
          items="center"
          justify="space-between"
          px="$2"
          py="$2"
          rounded="$2"
          borderWidth={1}
          borderColor={offset === 0 && kind === nextKind ? colors.accent : colors.border}
        >
          <Text color={colors.muted}>{setLabel(kind, number)}</Text>
          <Text color={colors.faint}>
            {kind === SetKind.Working ? `target ${exercise.targetReps} reps` : "warm-up"}
          </Text>
        </XStack>
      ))}
    </YStack>
  );
}

function PlanRow({
  exercise,
  isCurrent,
  moveUpTo,
  moveDownTo,
  canDoNow,
  busy,
  onMove,
  onDoNow,
  onSkip,
  onAddSet,
  onEdit,
}: {
  exercise: GymSessionExercise;
  isCurrent: boolean;
  moveUpTo: number | undefined;
  moveDownTo: number | undefined;
  canDoNow: boolean;
  busy: boolean;
  onMove: (to: number) => void;
  onDoNow: () => void;
  onSkip: (skipped: boolean) => void;
  onAddSet: () => void;
  onEdit: (set: GymSet, number: number) => void;
}) {
  const [open, setOpen] = useState(false);
  const working = countOfKind(exercise.sets, SetKind.Working);
  const status = exercise.skipped
    ? " · skipped"
    : isCurrent
      ? " · now"
      : exercise.done
        ? " · done"
        : "";
  return (
    <GymCard highlighted={isCurrent}>
      <YStack gap="$2">
        <XStack
          items="center"
          gap="$2"
          onPress={() => setOpen(!open)}
          pressStyle={{ bg: palette.raised }}
        >
          <YStack flex={1}>
            <Text
              numberOfLines={1}
              color={exercise.skipped ? colors.faint : undefined}
              fontWeight={isCurrent ? "700" : "400"}
            >
              {exercise.supersetWithPrevious ? "↳ " : ""}
              {exercise.name}
            </Text>
            <Muted>{`${working}/${exercise.targetSets} sets${status}`}</Muted>
          </YStack>
          {moveUpTo !== undefined && (
            <Button
              size="$3"
              aria-label={`Move ${exercise.name} up`}
              icon={<ChevronUp size={18} />}
              disabled={busy}
              onPress={() => onMove(moveUpTo)}
            />
          )}
          {moveDownTo !== undefined && (
            <Button
              size="$3"
              aria-label={`Move ${exercise.name} down`}
              icon={<ChevronDown size={18} />}
              disabled={busy}
              onPress={() => onMove(moveDownTo)}
            />
          )}
        </XStack>
        {open && (
          <YStack gap="$2">
            {exercise.sets.length > 0 && (
              <SetRows
                exercise={{ ...exercise, warmUpSets: 0, targetSets: 0 }}
                nextKind={undefined}
                onEdit={onEdit}
              />
            )}
            {exercise.planned && (
              <XStack gap="$2" flexWrap="wrap">
                {canDoNow && (
                  <Button size="$3" icon={<Play size={14} />} disabled={busy} onPress={onDoNow}>
                    Do now
                  </Button>
                )}
                <Button size="$3" icon={<Plus size={14} />} disabled={busy} onPress={onAddSet}>
                  Add set
                </Button>
                {(exercise.skipped || !exercise.done) && (
                  <Button
                    size="$3"
                    icon={exercise.skipped ? <RotateCcw size={14} /> : <SkipForward size={14} />}
                    disabled={busy}
                    onPress={() => onSkip(!exercise.skipped)}
                  >
                    {exercise.skipped ? "Unskip" : "Skip"}
                  </Button>
                )}
              </XStack>
            )}
          </YStack>
        )}
      </YStack>
    </GymCard>
  );
}

function targetSummary(exercise: GymSessionExercise): string {
  const reserve =
    exercise.targetRepsInReserve === undefined ? "" : ` @ ${exercise.targetRepsInReserve} RIR`;
  const warmUps =
    exercise.warmUpSets > 0
      ? `${exercise.warmUpSets} warm-up (rest ${formatRestSeconds(exercise.warmUpRestSeconds)}) + `
      : "";
  return `${warmUps}${exercise.targetSets} × ${exercise.targetReps}${reserve} · rest ${formatRestSeconds(exercise.restSeconds)}`;
}

function countOfKind(sets: GymSet[], kind: SetKind): number {
  return sets.filter((set) => set.kind === kind).length;
}
