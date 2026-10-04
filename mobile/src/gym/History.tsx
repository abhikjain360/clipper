import { ArrowLeft, Trash2 } from "lucide-react-native";
import { useCallback, useEffect, useRef, useState } from "react";
import { Alert } from "react-native";
import { Button, H2, ScrollView, Spinner, Text, XStack, YStack } from "tamagui";
import {
  SetKind,
  type GymSession,
  type GymSessionSummary,
  type GymSet,
} from "@clipper/mobile-bridge";
import { formatBackendError } from "../backend";
import {
  formatClock,
  formatDay,
  formatKg,
  formatSet,
  formatTime,
  gym,
  setLabel,
} from "./gymClient";
import { colors, GymCard, Muted, SetEditor } from "./GymUi";
import { confirmDeleteSet } from "./LiveSession";
import { stopRestEnd } from "./restAlarm";

export function History({ onError }: { onError: (error: string | null) => void }) {
  const [sessions, setSessions] = useState<GymSessionSummary[] | null>(null);
  const [openId, setOpenId] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setSessions(await gym().gymSessions());
    } catch (caught) {
      onError(formatBackendError(caught));
      setSessions([]);
    }
  }, [onError]);

  useEffect(() => {
    if (openId === null) void load();
  }, [load, openId]);

  if (openId !== null) {
    return <SessionDetail sessionId={openId} onBack={() => setOpenId(null)} onError={onError} />;
  }
  if (sessions === null) {
    return (
      <YStack flex={1} items="center" justify="center">
        <Spinner />
      </YStack>
    );
  }

  const days: { day: string; sessions: GymSessionSummary[] }[] = [];
  for (const session of sessions) {
    const day = formatDay(session.startedAtMillis);
    const last = days[days.length - 1];
    if (last?.day === day) last.sessions.push(session);
    else days.push({ day, sessions: [session] });
  }

  return (
    <ScrollView flex={1}>
      <YStack gap="$3" pb="$8">
        <H2 size="$6">History</H2>
        {sessions.length === 0 && <Muted>No workouts logged yet</Muted>}
        {days.map(({ day, sessions: daySessions }) => (
          <YStack key={day} gap="$2">
            <Text fontWeight="600" color={colors.muted}>
              {day}
            </Text>
            {daySessions.map((session) => (
              <XStack
                key={session.id}
                onPress={() => setOpenId(session.id)}
                pressStyle={{ opacity: 0.6 }}
              >
                <YStack flex={1}>
                  <GymCard>
                    <YStack gap="$1">
                      <XStack justify="space-between" gap="$2">
                        <Text fontWeight="600">{session.name}</Text>
                        <Text
                          color={session.endedAtMillis === undefined ? colors.warm : colors.muted}
                        >
                          {session.endedAtMillis === undefined
                            ? "In progress"
                            : `${formatTime(session.startedAtMillis)} · ${formatClock(session.endedAtMillis - session.startedAtMillis)}`}
                        </Text>
                      </XStack>
                      <Muted>{session.exerciseNames.join(", ") || "No exercises"}</Muted>
                      <Muted>{`${session.workingSets} working sets`}</Muted>
                    </YStack>
                  </GymCard>
                </YStack>
              </XStack>
            ))}
          </YStack>
        ))}
      </YStack>
    </ScrollView>
  );
}

function SessionDetail({
  sessionId,
  onBack,
  onError,
}: {
  sessionId: string;
  onBack: () => void;
  onError: (error: string | null) => void;
}) {
  const [session, setSession] = useState<GymSession | null>(null);
  const [editing, setEditing] = useState<{ set: GymSet; number: number } | null>(null);
  const busyRef = useRef(false);

  const load = useCallback(async () => {
    try {
      setSession(await gym().gymSession(sessionId));
    } catch (caught) {
      onError(formatBackendError(caught));
      onBack();
    }
  }, [sessionId, onError, onBack]);

  useEffect(() => {
    void load();
  }, [load]);

  async function run(action: () => Promise<unknown>) {
    if (busyRef.current) return;
    busyRef.current = true;
    onError(null);
    try {
      await action();
    } catch (caught) {
      onError(formatBackendError(caught));
    } finally {
      busyRef.current = false;
      await load();
    }
  }

  function deleteWorkout(target: GymSession) {
    Alert.alert("Delete this workout?", "Its sets are deleted on every device.", [
      { text: "Cancel", style: "cancel" },
      {
        text: "Delete",
        style: "destructive",
        onPress: () =>
          void (async () => {
            try {
              await gym().gymDeleteSession(target.id);
              stopRestEnd(target.id);
              onBack();
            } catch (caught) {
              onError(formatBackendError(caught));
            }
          })(),
      },
    ]);
  }

  if (!session) {
    return (
      <YStack flex={1} items="center" justify="center">
        <Spinner />
      </YStack>
    );
  }

  return (
    <ScrollView flex={1}>
      <YStack gap="$3" pb="$8">
        <XStack items="center" gap="$2">
          <Button
            size="$3"
            aria-label="Back to history"
            icon={<ArrowLeft size={16} />}
            onPress={onBack}
          />
          <YStack flex={1}>
            <H2 size="$6" numberOfLines={1}>
              {session.name}
            </H2>
            <Muted>
              {`${formatDay(session.startedAtMillis)} · ${formatTime(session.startedAtMillis)}`}
              {session.endedAtMillis === undefined
                ? " · in progress"
                : ` · ${formatClock(session.endedAtMillis - session.startedAtMillis)}`}
            </Muted>
          </YStack>
          <Button
            size="$3"
            aria-label="Delete workout"
            icon={<Trash2 size={16} color={colors.bad} />}
            onPress={() => deleteWorkout(session)}
          />
        </XStack>
        {session.exercises
          .filter((exercise) => exercise.sets.length > 0 || !exercise.skipped)
          .map((exercise) => {
            const numbers = new Map<SetKind, number>();
            return (
              <GymCard key={exercise.exerciseId}>
                <YStack gap="$2">
                  <Text fontWeight="600">{exercise.name}</Text>
                  {exercise.sets.length === 0 && <Muted>No sets logged</Muted>}
                  {exercise.sets.map((set) => {
                    const number = (numbers.get(set.kind) ?? 0) + 1;
                    numbers.set(set.kind, number);
                    return (
                      <XStack
                        key={set.id}
                        items="center"
                        justify="space-between"
                        px="$2"
                        py="$2"
                        rounded="$2"
                        bg="#1f2428"
                        onPress={() => setEditing({ set, number })}
                        pressStyle={{ opacity: 0.6 }}
                        aria-label={`Edit ${setLabel(set.kind, number).toLowerCase()} of ${exercise.name}`}
                      >
                        <Text color={set.kind === SetKind.WarmUp ? colors.warmUp : undefined}>
                          {setLabel(set.kind, number)}
                        </Text>
                        <Text>{formatSet(set)}</Text>
                        <Text color={colors.faint} fontSize={12}>
                          {set.estimatedOneRepMaxKg === undefined || set.kind === SetKind.WarmUp
                            ? ""
                            : `e1RM ${formatKg(Math.round(set.estimatedOneRepMaxKg * 10) / 10)}`}
                        </Text>
                      </XStack>
                    );
                  })}
                </YStack>
              </GymCard>
            );
          })}
      </YStack>
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
