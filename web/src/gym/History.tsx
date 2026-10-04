import { Plus, Trash2 } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { H2, ScrollView, Spinner, Text, XStack, YStack } from "tamagui";
import { Button } from "./Button";
import {
    formatClock,
    formatDay,
    formatTime,
    palette,
    type GymBackend,
    type GymSessionSummary,
    type GymSet,
    type GymSetKind,
    type GymSetValues,
} from "@clipper/shared";
import {
    CardTitle,
    Column,
    Columns,
    ConfirmDialog,
    ExercisePicker,
    GymCard,
    GymDialog,
    Muted,
    numberedSets,
    SetEditor,
    SetFields,
    SetRow,
    setDraft,
    setValues,
    useGymChange,
    useGymData,
    type ErrorHandler,
    type SetDraft,
} from "./GymUi";

type Adding = { exerciseId: string; name: string; prefill: GymSetValues | null };

export function History({
    backend,
    state,
    onError,
}: {
    backend: GymBackend;
    state: unknown;
    onError: ErrorHandler;
}) {
    const load = useCallback(() => backend.sessions(), [backend]);
    const { value: sessions, reload } = useGymData(load, state, onError);
    const [selectedId, setSelectedId] = useState<string | null>(null);
    const shownId =
        sessions?.find((session) => session.id === selectedId)?.id ?? sessions?.[0]?.id ?? null;

    const days: { day: string; sessions: GymSessionSummary[] }[] = [];
    for (const session of sessions ?? []) {
        const day = formatDay(session.started_at_millis);
        const last = days[days.length - 1];
        if (last?.day === day) last.sessions.push(session);
        else days.push({ day, sessions: [session] });
    }

    return (
        <Columns>
            <Column grow={2} minWidth={300}>
                <GymCard>
                    <CardTitle>Workouts</CardTitle>
                    {sessions === undefined && <Spinner />}
                    {sessions?.length === 0 && <Muted>No workouts logged yet</Muted>}
                    <ScrollView maxH="75vh">
                        <YStack gap="$3">
                            {days.map(({ day, sessions: daySessions }) => (
                                <YStack key={day} gap="$1">
                                    <Text fontWeight="600" color={palette.secondary}>
                                        {day}
                                    </Text>
                                    {daySessions.map((session) => (
                                        <SessionRow
                                            key={session.id}
                                            session={session}
                                            selected={session.id === shownId}
                                            onSelect={() => setSelectedId(session.id)}
                                        />
                                    ))}
                                </YStack>
                            ))}
                        </YStack>
                    </ScrollView>
                </GymCard>
            </Column>
            <Column grow={3} minWidth={440}>
                {shownId && (
                    <SessionDetail
                        key={shownId}
                        sessionId={shownId}
                        backend={backend}
                        state={state}
                        onError={onError}
                        onChanged={reload}
                        onDeleted={() => {
                            setSelectedId(null);
                            reload();
                        }}
                    />
                )}
            </Column>
        </Columns>
    );
}

function SessionRow({
    session,
    selected,
    onSelect,
}: {
    session: GymSessionSummary;
    selected: boolean;
    onSelect: () => void;
}) {
    return (
        <YStack
            gap="$1"
            px="$3"
            py="$2"
            rounded="$3"
            borderWidth={selected ? 1 : 0}
            borderColor={palette.selectedBorder}
            bg={selected ? palette.selectedFill : palette.cardFill}
            cursor="pointer"
            hoverStyle={{ bg: selected ? palette.selectedFill : palette.cardFill }}
            pressStyle={{ bg: selected ? palette.selectedFill : palette.cardFill }}
            role="button"
            aria-pressed={selected}
            onPress={onSelect}
        >
            <XStack justify="space-between" gap="$2">
                <Text fontWeight="600">{session.name}</Text>
                <Text
                    color={session.ended_at_millis === null ? palette.warning : palette.secondary}
                >
                    {session.ended_at_millis === null
                        ? "In progress"
                        : `${formatTime(session.started_at_millis)} · ${formatClock(session.ended_at_millis - session.started_at_millis)}`}
                </Text>
            </XStack>
            <Muted>{session.exercise_names.join(", ") || "No exercises"}</Muted>
            <Muted>{`${session.working_sets} working sets`}</Muted>
        </YStack>
    );
}

function SessionDetail({
    sessionId,
    backend,
    state,
    onError,
    onChanged,
    onDeleted,
}: {
    sessionId: string;
    backend: GymBackend;
    state: unknown;
    onError: ErrorHandler;
    onChanged: () => Promise<void>;
    onDeleted: () => void;
}) {
    const load = useCallback(
        () => Promise.all([backend.session(sessionId), backend.exercises()]),
        [backend, sessionId],
    );
    const { value, failed, reload } = useGymData(load, state, onError);
    const changed = useCallback(async () => {
        await Promise.all([reload(), onChanged()]);
    }, [reload, onChanged]);
    const { busy, run } = useGymChange(changed, onError);
    const [editing, setEditing] = useState<{
        set: GymSet;
        number: number;
        exerciseName: string;
    } | null>(null);
    const [deletingSet, setDeletingSet] = useState<GymSet | null>(null);
    const [deletingSession, setDeletingSession] = useState(false);
    const [picking, setPicking] = useState(false);
    const [adding, setAdding] = useState<Adding | null>(null);

    if (value === undefined)
        return failed ? <Muted>This workout could not be loaded.</Muted> : <Spinner />;
    const [session, exercises] = value;

    async function deleteWorkout() {
        const saved = await run(async () => {
            await backend.change({ change: "delete_session", session_id: session.id });
            if (session.ended_at_millis === null) await backend.cancelRestEnd();
            onDeleted();
        }, false);
        if (saved) setDeletingSession(false);
    }

    const shown = session.exercises.filter(
        (exercise) => exercise.sets.length > 0 || !exercise.skipped,
    );
    return (
        <YStack gap="$4">
            <GymCard>
                <XStack items="flex-start" justify="space-between" gap="$3" flexWrap="wrap">
                    <YStack flex={1} minW={200}>
                        <H2 size="$6">{session.name}</H2>
                        <Muted>
                            {`${formatDay(session.started_at_millis)} · ${formatTime(session.started_at_millis)}`}
                            {session.ended_at_millis === null
                                ? " · in progress"
                                : ` · ${formatClock(session.ended_at_millis - session.started_at_millis)}`}
                        </Muted>
                    </YStack>
                    <XStack gap="$2">
                        <Button
                            icon={<Plus size={16} />}
                            busy={busy}
                            onPress={() => setPicking(true)}
                        >
                            Add exercise
                        </Button>
                        <Button
                            tone="danger"
                            icon={<Trash2 size={16} />}
                            busy={busy}
                            onPress={() => setDeletingSession(true)}
                        >
                            Delete workout
                        </Button>
                    </XStack>
                </XStack>
                <Muted>Select a set to change it. Add sets you did but did not log.</Muted>
            </GymCard>
            {shown.length === 0 && <Muted>No exercises in this workout.</Muted>}
            {shown.map((exercise) => (
                <GymCard key={exercise.exercise_id}>
                    <XStack items="center" justify="space-between" gap="$2">
                        <CardTitle>{exercise.name}</CardTitle>
                        <Button
                            size="$3"
                            icon={<Plus size={14} />}
                            busy={busy}
                            onPress={() => {
                                const last = exercise.sets[exercise.sets.length - 1];
                                setAdding({
                                    exerciseId: exercise.exercise_id,
                                    name: exercise.name,
                                    prefill: last ?? null,
                                });
                            }}
                        >
                            Add set
                        </Button>
                    </XStack>
                    {exercise.sets.length === 0 && <Muted>No sets logged</Muted>}
                    <YStack gap="$1">
                        {numberedSets(exercise.sets).map(({ set, number }) => (
                            <SetRow
                                key={set.id}
                                set={set}
                                number={number}
                                exerciseName={exercise.name}
                                onEdit={() =>
                                    setEditing({ set, number, exerciseName: exercise.name })
                                }
                            />
                        ))}
                    </YStack>
                </GymCard>
            ))}
            <ExercisePicker
                open={picking}
                exercises={exercises}
                excluded={new Set(shown.map((exercise) => exercise.exercise_id))}
                onClose={() => setPicking(false)}
                onPick={(exercise) => {
                    setPicking(false);
                    setAdding({ exerciseId: exercise.id, name: exercise.name, prefill: null });
                }}
            />
            <AddSetDialog
                adding={adding}
                busy={busy}
                onClose={() => setAdding(null)}
                onAdd={(kind, values) => {
                    const target = adding;
                    if (target)
                        void run(() =>
                            backend.change({
                                change: "add_set",
                                session_id: session.id,
                                exercise_id: target.exerciseId,
                                kind,
                                values,
                            }),
                        ).then((saved) => {
                            if (saved) setAdding(null);
                        });
                }}
            />
            <SetEditor
                editing={editing}
                busy={busy}
                onClose={() => setEditing(null)}
                onSave={(values) => {
                    const set = editing?.set;
                    if (set)
                        void run(() =>
                            backend.change({ change: "edit_set", set_id: set.id, values }),
                        ).then((saved) => {
                            if (saved) setEditing(null);
                        });
                }}
                onDelete={() => {
                    setDeletingSet(editing?.set ?? null);
                    setEditing(null);
                }}
            />
            <ConfirmDialog
                open={deletingSet !== null}
                title="Delete this set?"
                description="It is removed on every device."
                confirmLabel="Delete set"
                busy={busy}
                onCancel={() => setDeletingSet(null)}
                onConfirm={() => {
                    const set = deletingSet;
                    if (set)
                        void run(() =>
                            backend.change({ change: "delete_set", set_id: set.id }),
                        ).then((saved) => {
                            if (saved) setDeletingSet(null);
                        });
                }}
            />
            <ConfirmDialog
                open={deletingSession}
                title="Delete this workout?"
                description="Its sets are deleted on every device."
                confirmLabel="Delete workout"
                busy={busy}
                onCancel={() => setDeletingSession(false)}
                onConfirm={() => void deleteWorkout()}
            />
        </YStack>
    );
}

function AddSetDialog({
    adding,
    busy,
    onAdd,
    onClose,
}: {
    adding: Adding | null;
    busy: boolean;
    onAdd: (kind: GymSetKind, values: GymSetValues) => void;
    onClose: () => void;
}) {
    const [kind, setKind] = useState<GymSetKind>("working");
    const [draft, setDraftState] = useState<SetDraft>(setDraft(null));
    useEffect(() => {
        if (!adding) return;
        setKind("working");
        setDraftState(setDraft(adding.prefill));
    }, [adding]);
    return (
        <GymDialog
            open={adding !== null}
            title={adding ? `Add a set of ${adding.name}` : ""}
            onClose={onClose}
            busy={busy}
        >
            <XStack gap="$2" role="group" aria-label="Kind of set">
                <Button
                    aria-pressed={kind === "working"}
                    selected={kind === "working"}
                    onPress={() => setKind("working")}
                >
                    Working set
                </Button>
                <Button
                    aria-pressed={kind === "warm_up"}
                    selected={kind === "warm_up"}
                    onPress={() => setKind("warm_up")}
                >
                    Warm-up
                </Button>
            </XStack>
            <SetFields
                draft={draft}
                onChange={setDraftState}
                onSubmit={() => onAdd(kind, setValues(draft))}
            />
            <XStack gap="$2" justify="flex-end">
                <Button busy={busy} onPress={onClose}>
                    Cancel
                </Button>
                <Button tone="accent" busy={busy} onPress={() => onAdd(kind, setValues(draft))}>
                    Add set
                </Button>
            </XStack>
        </GymDialog>
    );
}
