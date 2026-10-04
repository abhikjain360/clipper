import { useCallback, useState } from "react";
import { Input, ScrollView, Spinner, Text, XStack, YStack } from "tamagui";
import {
    formatDay,
    formatKg,
    formatShortDate,
    palette,
    type GymBackend,
    type GymExercise,
} from "@clipper/shared";
import {
    CardTitle,
    Column,
    Columns,
    GymCard,
    LineChart,
    Muted,
    useGymData,
    type ErrorHandler,
} from "./GymUi";

export function Progress({
    backend,
    state,
    onError,
}: {
    backend: GymBackend;
    state: unknown;
    onError: ErrorHandler;
}) {
    const load = useCallback(async () => {
        const [exercises, sessions] = await Promise.all([backend.exercises(), backend.sessions()]);
        const recent = sessions.find((session) => session.working_sets > 0);
        const detail = recent ? await backend.session(recent.id) : undefined;
        const loggedId =
            detail?.exercises.find((exercise) => exercise.sets.length > 0)?.exercise_id ?? null;
        return { exercises, loggedId };
    }, [backend]);
    const { value } = useGymData(load, state, onError);
    const [selectedId, setSelectedId] = useState<string | null>(null);
    const [search, setSearch] = useState("");

    if (value === undefined) return <Spinner />;
    const { exercises, loggedId } = value;
    const selected =
        exercises.find((exercise) => exercise.id === selectedId) ??
        exercises.find((exercise) => exercise.id === loggedId) ??
        exercises.find((exercise) => !exercise.archived);
    if (!selected)
        return <Muted>No exercises yet. Add one in the Library, then log a workout.</Muted>;
    const query = search.trim().toLowerCase();
    const shown = exercises.filter(
        (exercise) =>
            (!exercise.archived || exercise.id === selected.id) &&
            exercise.name.toLowerCase().includes(query),
    );

    return (
        <Columns>
            <Column grow={1} minWidth={260}>
                <GymCard>
                    <CardTitle>Exercises</CardTitle>
                    <Input
                        value={search}
                        onChangeText={setSearch}
                        placeholder="Search exercises"
                        aria-label="Search exercises"
                    />
                    <ScrollView maxH="65vh">
                        <YStack gap="$1">
                            {shown.length === 0 && <Muted>No matching exercises</Muted>}
                            {shown.map((exercise) => (
                                <ExerciseRow
                                    key={exercise.id}
                                    exercise={exercise}
                                    selected={exercise.id === selected.id}
                                    onSelect={() => setSelectedId(exercise.id)}
                                />
                            ))}
                        </YStack>
                    </ScrollView>
                </GymCard>
            </Column>
            <Column grow={3} minWidth={440}>
                <ExerciseProgress
                    key={selected.id}
                    exercise={selected}
                    backend={backend}
                    state={state}
                    onError={onError}
                />
            </Column>
        </Columns>
    );
}

function ExerciseRow({
    exercise,
    selected,
    onSelect,
}: {
    exercise: GymExercise;
    selected: boolean;
    onSelect: () => void;
}) {
    return (
        <XStack
            px="$3"
            py="$2"
            rounded="$3"
            borderWidth={1}
            borderColor={selected ? palette.accent : "transparent"}
            bg={selected ? palette.raised : undefined}
            cursor="pointer"
            hoverStyle={{ bg: palette.raised }}
            pressStyle={{ bg: palette.raised }}
            role="button"
            aria-pressed={selected}
            onPress={onSelect}
        >
            <Text fontWeight={selected ? "600" : "400"}>{exercise.name}</Text>
        </XStack>
    );
}

function ExerciseProgress({
    exercise,
    backend,
    state,
    onError,
}: {
    exercise: GymExercise;
    backend: GymBackend;
    state: unknown;
    onError: ErrorHandler;
}) {
    const exerciseId = exercise.id;
    const load = useCallback(() => backend.oneRepMaxProgress(exerciseId), [backend, exerciseId]);
    const { value: points } = useGymData(load, state, onError);
    const best = points?.reduce((top, point) => Math.max(top, point.kg), 0) ?? 0;
    return (
        <GymCard>
            <YStack gap="$1">
                <CardTitle>{exercise.name}</CardTitle>
                <Muted>Best estimated one-rep max per workout</Muted>
            </YStack>
            {points === undefined ? (
                <Spinner />
            ) : points.length === 0 ? (
                <Muted>No working sets with weight and reps logged for this exercise yet</Muted>
            ) : (
                <>
                    <XStack gap="$2" items="baseline">
                        <Text fontSize={28} fontWeight="700">
                            {`${formatKg(Math.round(best * 10) / 10)} kg`}
                        </Text>
                        <Text color={palette.secondary}>best</Text>
                    </XStack>
                    <LineChart
                        label={`Best estimated one-rep max per workout for ${exercise.name}`}
                        points={points.map((point) => ({
                            x: point.started_at_millis,
                            y: point.kg,
                        }))}
                        formatTick={(value) => formatKg(Math.round(value))}
                        formatValue={(value) => `${formatKg(Math.round(value * 10) / 10)} kg`}
                        formatX={(value) => formatShortDate(new Date(value))}
                    />
                    <YStack gap="$1">
                        {points.toReversed().map((point) => (
                            <XStack
                                key={point.session_id}
                                justify="space-between"
                                px="$3"
                                py="$1.5"
                                rounded="$3"
                                bg={palette.page}
                            >
                                <Text color={palette.secondary}>
                                    {formatDay(point.started_at_millis)}
                                </Text>
                                <Text>{`${formatKg(Math.round(point.kg * 10) / 10)} kg`}</Text>
                            </XStack>
                        ))}
                    </YStack>
                </>
            )}
        </GymCard>
    );
}
