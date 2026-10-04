import { Button } from "./Button";
import { useEffect, useRef, useState } from "react";
import { H2, ScrollView, Spinner, Text, XStack, YStack } from "tamagui";
import type { GymExercise, GymOneRepMax } from "@clipper/mobile-bridge";
import { formatDay, formatKg } from "@clipper/shared";
import { formatBackendError } from "../backend";
import { gym } from "./gymClient";
import { colors, ExercisePicker, GymCard, LineChart, Muted } from "./GymUi";

export function Progress({ onError }: { onError: (error: string | null) => void }) {
  const [exercises, setExercises] = useState<GymExercise[]>([]);
  const [selected, setSelected] = useState<GymExercise | null | undefined>(undefined);
  const [points, setPoints] = useState<GymOneRepMax[] | null>(null);
  const [picking, setPicking] = useState(false);
  const loadGeneration = useRef(0);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const list = await gym().gymExercises();
        const sessions = await gym().gymSessions();
        const recent = sessions.find((session) => session.workingSets > 0);
        const detail = recent ? await gym().gymSession(recent.id) : undefined;
        const loggedId = detail?.exercises.find((exercise) => exercise.sets.length > 0)?.exerciseId;
        if (cancelled) return;
        setExercises(list);
        setSelected(
          (current) =>
            current ??
            list.find((exercise) => exercise.id === loggedId) ??
            list.find((exercise) => !exercise.archived) ??
            null,
        );
      } catch (caught) {
        if (cancelled) return;
        onError(formatBackendError(caught));
        setSelected((current) => current ?? null);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [onError]);

  useEffect(() => {
    const generation = ++loadGeneration.current;
    setPoints(null);
    if (!selected) return;
    void (async () => {
      try {
        const progress = await gym().gymOneRepMaxProgress(selected.id);
        if (generation === loadGeneration.current) setPoints(progress);
      } catch (caught) {
        if (generation !== loadGeneration.current) return;
        onError(formatBackendError(caught));
        setPoints([]);
      }
    })();
    return () => {
      loadGeneration.current += 1;
    };
  }, [selected, onError]);

  if (selected === undefined) {
    return (
      <YStack flex={1} items="center" justify="center">
        <Spinner />
      </YStack>
    );
  }

  if (selected === null) {
    return (
      <YStack gap="$3" pb="$8">
        <H2 size="$6">Progress</H2>
        <Muted>No exercises yet. Add one in the Library, then log a workout.</Muted>
      </YStack>
    );
  }

  const best = points?.reduce((top, point) => Math.max(top, point.kg), 0) ?? 0;

  return (
    <ScrollView flex={1} keyboardShouldPersistTaps="always">
      <YStack gap="$3" pb="$8">
        <H2 size="$6">Progress</H2>
        <XStack items="center" gap="$2">
          <Text flex={1} fontWeight="600" numberOfLines={2}>
            {selected.name}
          </Text>
          <Button size="$3" onPress={() => setPicking(true)}>
            Change exercise
          </Button>
        </XStack>
        <GymCard>
          <YStack gap="$2">
            <Text color={colors.muted}>Best estimated one-rep max per workout</Text>
            {points === null ? (
              <Spinner />
            ) : points.length === 0 ? (
              <Muted>No working sets with weight and reps logged for this exercise yet</Muted>
            ) : (
              <>
                <LineChart
                  points={points.map((point) => ({ x: point.startedAtMillis, y: point.kg }))}
                  formatY={(value) => formatKg(Math.round(value))}
                  formatX={(value) =>
                    new Date(value).toLocaleDateString(undefined, {
                      month: "short",
                      day: "numeric",
                    })
                  }
                />
                <Muted>{`Best: ${formatKg(Math.round(best * 10) / 10)} kg`}</Muted>
              </>
            )}
          </YStack>
        </GymCard>
        {points && points.length > 0 && (
          <YStack gap="$1">
            {points.toReversed().map((point) => (
              <XStack key={point.sessionId} justify="space-between" px="$2" py="$1">
                <Text color={colors.muted}>{formatDay(point.startedAtMillis)}</Text>
                <Text>{`${formatKg(Math.round(point.kg * 10) / 10)} kg`}</Text>
              </XStack>
            ))}
          </YStack>
        )}
      </YStack>
      <ExercisePicker
        visible={picking}
        exercises={exercises}
        onClose={() => setPicking(false)}
        onPick={(exercise) => {
          setPicking(false);
          setSelected(exercise);
        }}
      />
    </ScrollView>
  );
}
