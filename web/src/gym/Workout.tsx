import { ChevronDown, ChevronUp, Play, Plus, RotateCcw, SkipForward } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { H2, Spinner, Text, XStack, YStack } from "tamagui";
import { Button } from "../tamagui.config";
import {
    fatigueColors,
    formatClock,
    formatMinutes,
    formatRestSeconds,
    formatTime,
    palette,
    restEndNotice,
    restSecondsLeft,
    type GymBackend,
    type GymChange,
    type GymExercise,
    type GymSession,
    type GymSessionExercise,
    type GymSet,
    type GymSetKind,
    type GymTemplate,
} from "@clipper/shared";
import {
    CardTitle,
    Column,
    Columns,
    ConfirmDialog,
    ExercisePicker,
    formatSet,
    GymCard,
    Muted,
    numberedSets,
    SetEditor,
    SetFields,
    SetRow,
    setDraft,
    setLabel,
    setValues,
    useGymChange,
    useGymData,
    type ErrorHandler,
    type SetDraft,
} from "./GymUi";

type Editing = { set: GymSet; number: number; exerciseName: string };

export function Workout({
    backend,
    state,
    onError,
}: {
    backend: GymBackend;
    state: unknown;
    onError: ErrorHandler;
}) {
    const load = useCallback(
        () => Promise.all([backend.openSession(), backend.templates(), backend.exercises()]),
        [backend],
    );
    const { value, failed, reload } = useGymData(load, state, onError);
    const { busy, run } = useGymChange(reload, onError);
    const [now, setNow] = useState(Date.now);
    const [draft, setDraftState] = useState<SetDraft>(setDraft(null));
    const [picking, setPicking] = useState(false);
    const [editing, setEditing] = useState<Editing | null>(null);
    const [deleting, setDeleting] = useState<GymSet | null>(null);
    const [finishing, setFinishing] = useState(false);
    const session = value?.[0];
    const sessionRef = useRef(session);
    sessionRef.current = session;

    useEffect(() => {
        const timer = setInterval(() => setNow(Date.now()), 1000);
        return () => clearInterval(timer);
    }, []);

    const current = session?.exercises.find(
        (exercise) => exercise.exercise_id === session.current_exercise_id,
    );
    const nextKind = session?.next_set_kind ?? null;
    const nextNumber = current && nextKind !== null ? countOfKind(current.sets, nextKind) + 1 : 0;
    const pendingKey =
        session && current && nextKind !== null
            ? `${session.id}:${session.next_set_order}:${current.exercise_id}:${nextKind}`
            : null;

    useEffect(() => {
        if (pendingKey) setDraftState(setDraft(sessionRef.current?.prefill));
    }, [pendingKey]);

    useEffect(() => {
        if (session === undefined) return;
        const rest = session?.ended_at_millis === null ? session.rest : null;
        if (!session || !rest || rest.ends_at_millis <= Date.now()) {
            backend.cancelRestEnd().catch(() => undefined);
            return;
        }
        backend
            .scheduleRestEnd(rest.ends_at_millis, "Rest is over", nextSetNotice(session))
            .catch(() => undefined);
    }, [backend, session]);

    if (value === undefined)
        return failed ? (
            <YStack gap="$2" items="flex-start">
                <Muted>The workout could not be loaded.</Muted>
                <Button onPress={reload}>Try again</Button>
            </YStack>
        ) : (
            <Spinner />
        );

    const [, templates, exercises] = value;
    if (!session)
        return (
            <StartWorkout
                templates={templates}
                exercises={exercises}
                busy={busy}
                onStart={(templateId) =>
                    void run(() =>
                        backend.change({ change: "start_session", template_id: templateId }),
                    )
                }
            />
        );

    const open = session;
    function completeSet() {
        if (!current || nextKind === null) return;
        const values = setValues(draft);
        const exerciseId = current.exercise_id;
        void run(() =>
            backend.change({
                change: "complete_set",
                session_id: open.id,
                exercise_id: exerciseId,
                kind: nextKind,
                expected_order: open.next_set_order,
                values,
            }),
        );
    }

    const planIds = new Set(
        open.exercises
            .filter((exercise) => exercise.planned && !exercise.skipped)
            .map((exercise) => exercise.exercise_id),
    );
    const inPlanOrder: GymSessionExercise[] = [];
    for (const exercise of open.exercises) {
        if (exercise.plan_index !== null) inPlanOrder[exercise.plan_index] = exercise;
    }
    const unfinished = inPlanOrder.filter(
        (exercise) => !exercise.done && exercise.exercise_id !== open.current_exercise_id,
    );

    return (
        <YStack gap="$4">
            <XStack items="center" justify="space-between" gap="$3" flexWrap="wrap">
                <YStack>
                    <H2 size="$6">{open.name}</H2>
                    <Muted>
                        {`Started ${formatTime(open.started_at_millis)} · ${formatMinutes(now - open.started_at_millis)}`}
                    </Muted>
                </YStack>
                <XStack gap="$2">
                    <Button
                        icon={<Plus size={16} />}
                        disabled={busy}
                        onPress={() => setPicking(true)}
                    >
                        Add exercise
                    </Button>
                    <Button disabled={busy} onPress={() => setFinishing(true)}>
                        Finish workout
                    </Button>
                </XStack>
            </XStack>
            <Columns>
                <Column grow={3} minWidth={420}>
                    {open.rest && (
                        <RestCard
                            startedAt={open.rest.started_at_millis}
                            endsAt={open.rest.ends_at_millis}
                            now={now}
                        />
                    )}
                    {current && nextKind !== null ? (
                        <GymCard highlighted>
                            <YStack gap="$1">
                                <Text fontSize={22} fontWeight="700">
                                    {current.name}
                                </Text>
                                <SupersetNote
                                    exercises={open.exercises}
                                    exerciseId={current.exercise_id}
                                />
                                <Muted>{targetSummary(current)}</Muted>
                                {current.last_time.length > 0 && (
                                    <Muted>
                                        Last time: {current.last_time.map(formatSet).join(", ")}
                                    </Muted>
                                )}
                            </YStack>
                            <Text
                                fontSize={16}
                                fontWeight="600"
                                color={
                                    nextKind === "warm_up" ? fatigueColors.warmUp : palette.accent
                                }
                            >
                                Next: {setLabel(nextKind, nextNumber)}
                            </Text>
                            <SetFields
                                draft={draft}
                                onChange={setDraftState}
                                onSubmit={completeSet}
                            />
                            <XStack gap="$2" flexWrap="wrap">
                                <Button
                                    theme="green"
                                    disabled={busy}
                                    icon={busy ? <Spinner /> : undefined}
                                    onPress={completeSet}
                                >
                                    {`Complete ${setLabel(nextKind, nextNumber).toLowerCase()}`}
                                </Button>
                                {countOfKind(current.sets, "working") === 0 && (
                                    <Button
                                        disabled={busy}
                                        onPress={() =>
                                            void run(() =>
                                                backend.change({
                                                    change: "add_warm_up_set",
                                                    session_id: open.id,
                                                    exercise_id: current.exercise_id,
                                                }),
                                            )
                                        }
                                    >
                                        Add warm-up
                                    </Button>
                                )}
                                <Button
                                    disabled={busy}
                                    onPress={() =>
                                        void run(() =>
                                            backend.change({
                                                change: "add_working_set",
                                                session_id: open.id,
                                                exercise_id: current.exercise_id,
                                            }),
                                        )
                                    }
                                >
                                    Add set
                                </Button>
                                <Button
                                    disabled={busy}
                                    onPress={() =>
                                        void run(() =>
                                            backend.change({
                                                change: "skip_exercise",
                                                session_id: open.id,
                                                exercise_id: current.exercise_id,
                                                skipped: true,
                                            }),
                                        )
                                    }
                                >
                                    Skip exercise
                                </Button>
                            </XStack>
                            <SetRows
                                exercise={current}
                                nextKind={nextKind}
                                onEdit={(set, number) =>
                                    setEditing({ set, number, exerciseName: current.name })
                                }
                            />
                        </GymCard>
                    ) : (
                        <GymCard>
                            <Text fontWeight="600">
                                {open.exercises.length === 0
                                    ? "No exercises yet"
                                    : "Every planned set is done"}
                            </Text>
                            <Muted>Add an exercise or a set, or finish the workout.</Muted>
                        </GymCard>
                    )}
                </Column>
                <Column grow={2} minWidth={320}>
                    <GymCard>
                        <CardTitle>Exercises</CardTitle>
                        {open.exercises.length === 0 && <Muted>No exercises yet</Muted>}
                        {open.exercises.map((exercise, index) => {
                            const position = unfinished.indexOf(exercise);
                            return (
                                <PlanRow
                                    key={exercise.exercise_id}
                                    exercise={exercise}
                                    first={index === 0}
                                    isCurrent={exercise.exercise_id === open.current_exercise_id}
                                    moveUpTo={
                                        position > 0 ? unfinished[position - 1]?.plan_index : null
                                    }
                                    moveDownTo={
                                        position >= 0 ? unfinished[position + 1]?.plan_index : null
                                    }
                                    canDoNow={
                                        exercise.planned &&
                                        !exercise.done &&
                                        exercise.exercise_id !== open.current_exercise_id
                                    }
                                    busy={busy}
                                    sessionId={open.id}
                                    onChange={(change) => void run(() => backend.change(change))}
                                    onEdit={(set, number) =>
                                        setEditing({ set, number, exerciseName: exercise.name })
                                    }
                                />
                            );
                        })}
                    </GymCard>
                </Column>
            </Columns>
            <ExercisePicker
                open={picking}
                exercises={exercises}
                excluded={planIds}
                onClose={() => setPicking(false)}
                onPick={(exercise) => {
                    setPicking(false);
                    void run(() =>
                        backend.change({
                            change: "add_exercise",
                            session_id: open.id,
                            exercise_id: exercise.id,
                        }),
                    );
                }}
            />
            <SetEditor
                editing={editing}
                busy={busy}
                onClose={() => setEditing(null)}
                onSave={(values) => {
                    const set = editing?.set;
                    setEditing(null);
                    if (set)
                        void run(() =>
                            backend.change({ change: "edit_set", set_id: set.id, values }),
                        );
                }}
                onDelete={() => {
                    setDeleting(editing?.set ?? null);
                    setEditing(null);
                }}
            />
            <ConfirmDialog
                open={deleting !== null}
                title="Delete this set?"
                description="It is removed on every device."
                confirmLabel="Delete set"
                busy={busy}
                onCancel={() => setDeleting(null)}
                onConfirm={() => {
                    const set = deleting;
                    setDeleting(null);
                    if (set)
                        void run(() => backend.change({ change: "delete_set", set_id: set.id }));
                }}
            />
            <ConfirmDialog
                open={finishing}
                title="Finish workout?"
                description="The workout moves to History."
                confirmLabel="Finish"
                destructive={false}
                busy={busy}
                onCancel={() => setFinishing(false)}
                onConfirm={() => {
                    setFinishing(false);
                    void run(async () => {
                        await backend.change({ change: "finish_session", session_id: open.id });
                        await backend.cancelRestEnd();
                    });
                }}
            />
        </YStack>
    );
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
    onStart: (templateId: string | null) => void;
}) {
    const names = new Map(exercises.map((exercise) => [exercise.id, exercise.name]));
    return (
        <YStack gap="$4">
            <XStack items="center" justify="space-between" gap="$3" flexWrap="wrap">
                <H2 size="$6">Start a workout</H2>
                <Button disabled={busy} onPress={() => onStart(null)}>
                    Start an empty workout
                </Button>
            </XStack>
            {templates.length === 0 && <Muted>No workouts yet. Add one in the Library.</Muted>}
            <XStack gap="$4" flexWrap="wrap">
                {templates.map((template) => (
                    <YStack key={template.id} grow={1} flexBasis={280} maxW={400}>
                        <GymCard>
                            <CardTitle>{template.name}</CardTitle>
                            <YStack gap="$1">
                                {template.exercises.map((planned) => (
                                    <XStack
                                        key={planned.exercise_id}
                                        justify="space-between"
                                        gap="$2"
                                    >
                                        <Text>
                                            {planned.superset_with_previous ? "↳ " : ""}
                                            {names.get(planned.exercise_id) ?? "Unknown exercise"}
                                        </Text>
                                        <Text color={palette.secondary}>
                                            {`${planned.target_sets} × ${planned.target_reps}`}
                                        </Text>
                                    </XStack>
                                ))}
                            </YStack>
                            <Button
                                theme="blue"
                                icon={<Play size={16} />}
                                disabled={busy}
                                onPress={() => onStart(template.id)}
                            >
                                {`Start ${template.name}`}
                            </Button>
                        </GymCard>
                    </YStack>
                ))}
            </XStack>
        </YStack>
    );
}

function RestCard({ startedAt, endsAt, now }: { startedAt: number; endsAt: number; now: number }) {
    const remaining = endsAt - now;
    const resting = remaining > 0;
    return (
        <GymCard>
            <XStack items="center" justify="space-between">
                <YStack>
                    <Text fontWeight="600">{resting ? "Rest" : "Rest is over"}</Text>
                    <Muted>{`Target ${formatClock(endsAt - startedAt)}`}</Muted>
                </YStack>
                <Text
                    fontSize={resting ? 44 : 28}
                    fontWeight="700"
                    color={resting ? palette.accent : palette.success}
                    aria-label={resting ? "Rest remaining" : "Time since rest ended"}
                    style={{ fontVariantNumeric: "tabular-nums" }}
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
        if (exercise.plan_index !== null) planned[exercise.plan_index] = exercise;
    }
    const index = planned.findIndex((exercise) => exercise.exercise_id === exerciseId);
    if (index < 0) return null;
    let start = index;
    while (start > 0 && planned[start]?.superset_with_previous) start -= 1;
    let end = index;
    while (planned[end + 1]?.superset_with_previous) end += 1;
    if (start === end) return null;
    const partners = planned
        .slice(start, end + 1)
        .filter((_, offset) => start + offset !== index)
        .map((exercise) => exercise.name);
    return <Text color={palette.accent}>Superset with {partners.join(", ")}</Text>;
}

function SetRows({
    exercise,
    nextKind,
    onEdit,
}: {
    exercise: GymSessionExercise;
    nextKind: GymSetKind;
    onEdit: (set: GymSet, number: number) => void;
}) {
    const done = numberedSets(exercise.sets);
    const warmUpsDone = countOfKind(exercise.sets, "warm_up");
    const workingDone = countOfKind(exercise.sets, "working");
    const planned: { kind: GymSetKind; number: number }[] = [
        ...Array.from(
            { length: Math.max(0, exercise.warm_up_sets - warmUpsDone) },
            (_, offset) => ({
                kind: "warm_up" as const,
                number: warmUpsDone + offset + 1,
            }),
        ),
        ...Array.from({ length: Math.max(0, exercise.target_sets - workingDone) }, (_, offset) => ({
            kind: "working" as const,
            number: workingDone + offset + 1,
        })),
    ];
    return (
        <YStack gap="$1">
            {done.map(({ set, number }) => (
                <SetRow
                    key={set.id}
                    set={set}
                    number={number}
                    onEdit={() => onEdit(set, number)}
                    exerciseName={exercise.name}
                />
            ))}
            {planned.map(({ kind, number }, offset) => (
                <XStack
                    key={`${kind}-${number}`}
                    items="center"
                    justify="space-between"
                    px="$3"
                    py="$2"
                    rounded="$3"
                    borderWidth={1}
                    borderColor={
                        offset === 0 && kind === nextKind ? palette.accent : palette.border
                    }
                >
                    <Text color={palette.secondary}>{setLabel(kind, number)}</Text>
                    <Text color={palette.secondary}>
                        {kind === "working" ? `target ${exercise.target_reps} reps` : "warm-up"}
                    </Text>
                </XStack>
            ))}
        </YStack>
    );
}

function PlanRow({
    exercise,
    first,
    isCurrent,
    moveUpTo,
    moveDownTo,
    canDoNow,
    busy,
    sessionId,
    onChange,
    onEdit,
}: {
    exercise: GymSessionExercise;
    first: boolean;
    isCurrent: boolean;
    moveUpTo: number | null | undefined;
    moveDownTo: number | null | undefined;
    canDoNow: boolean;
    busy: boolean;
    sessionId: string;
    onChange: (change: GymChange) => void;
    onEdit: (set: GymSet, number: number) => void;
}) {
    const target = { session_id: sessionId, exercise_id: exercise.exercise_id };
    const working = countOfKind(exercise.sets, "working");
    const status = exercise.skipped
        ? " · skipped"
        : isCurrent
          ? " · now"
          : exercise.done
            ? " · done"
            : "";
    return (
        <YStack
            gap="$2"
            pt={first ? 0 : "$3"}
            borderTopWidth={first ? 0 : 1}
            borderColor={palette.border}
        >
            <XStack items="center" gap="$2">
                <YStack flex={1}>
                    <Text
                        fontWeight={isCurrent ? "700" : "500"}
                        color={
                            exercise.skipped
                                ? palette.secondary
                                : isCurrent
                                  ? palette.accent
                                  : undefined
                        }
                    >
                        {exercise.superset_with_previous ? "↳ " : ""}
                        {exercise.name}
                    </Text>
                    <Muted>
                        {exercise.planned
                            ? `${working}/${exercise.target_sets} sets${status}`
                            : `${working} sets · not in the plan`}
                    </Muted>
                </YStack>
                {moveUpTo !== null && moveUpTo !== undefined && (
                    <Button
                        size="$2"
                        aria-label={`Move ${exercise.name} up`}
                        icon={<ChevronUp size={16} />}
                        disabled={busy}
                        onPress={() =>
                            onChange({ change: "move_exercise", ...target, to_index: moveUpTo })
                        }
                    />
                )}
                {moveDownTo !== null && moveDownTo !== undefined && (
                    <Button
                        size="$2"
                        aria-label={`Move ${exercise.name} down`}
                        icon={<ChevronDown size={16} />}
                        disabled={busy}
                        onPress={() =>
                            onChange({ change: "move_exercise", ...target, to_index: moveDownTo })
                        }
                    />
                )}
            </XStack>
            {exercise.sets.length > 0 && (
                <XStack gap="$1.5" flexWrap="wrap">
                    {numberedSets(exercise.sets).map(({ set, number }) => (
                        <Button
                            key={set.id}
                            size="$2"
                            chromeless
                            bg={palette.page}
                            aria-label={`Edit ${setLabel(set.kind, number).toLowerCase()} of ${exercise.name}`}
                            onPress={() => onEdit(set, number)}
                        >
                            <Text
                                fontSize={12}
                                color={set.kind === "warm_up" ? fatigueColors.warmUp : undefined}
                            >
                                {formatSet(set)}
                            </Text>
                        </Button>
                    ))}
                </XStack>
            )}
            {exercise.planned && (
                <XStack gap="$2" flexWrap="wrap">
                    {canDoNow && (
                        <Button
                            size="$2"
                            icon={<Play size={12} />}
                            disabled={busy}
                            onPress={() => onChange({ change: "switch_exercise", ...target })}
                        >
                            Do now
                        </Button>
                    )}
                    <Button
                        size="$2"
                        icon={<Plus size={12} />}
                        disabled={busy}
                        onPress={() => onChange({ change: "add_working_set", ...target })}
                    >
                        Add set
                    </Button>
                    {(exercise.skipped || !exercise.done) && (
                        <Button
                            size="$2"
                            icon={
                                exercise.skipped ? (
                                    <RotateCcw size={12} />
                                ) : (
                                    <SkipForward size={12} />
                                )
                            }
                            disabled={busy}
                            onPress={() =>
                                onChange({
                                    change: "skip_exercise",
                                    ...target,
                                    skipped: !exercise.skipped,
                                })
                            }
                        >
                            {exercise.skipped ? "Unskip" : "Skip"}
                        </Button>
                    )}
                </XStack>
            )}
        </YStack>
    );
}

function nextSetNotice(session: GymSession): string {
    const current = session.exercises.find(
        (exercise) => exercise.exercise_id === session.current_exercise_id,
    );
    const kind = session.next_set_kind;
    return restEndNotice(
        current && kind !== null
            ? {
                  name: current.name,
                  warmUp: kind === "warm_up",
                  number: countOfKind(current.sets, kind) + 1,
              }
            : null,
    );
}

function targetSummary(exercise: GymSessionExercise): string {
    const reserve =
        exercise.target_reps_in_reserve === null ? "" : ` @ ${exercise.target_reps_in_reserve} RIR`;
    const warmUps =
        exercise.warm_up_sets > 0
            ? `${exercise.warm_up_sets} warm-up (rest ${formatRestSeconds(exercise.warm_up_rest_seconds)}) + `
            : "";
    return `${warmUps}${exercise.target_sets} × ${exercise.target_reps}${reserve} · rest ${formatRestSeconds(exercise.rest_seconds)}`;
}

function countOfKind(sets: GymSet[], kind: GymSetKind): number {
    return sets.filter((set) => set.kind === kind).length;
}
