import { Trash2 } from "lucide-react-native";
import { useCallback, useEffect, useRef, useState } from "react";
import { Alert, Keyboard } from "react-native";
import { Button, H2, ScrollView, Text, XStack, YStack } from "tamagui";
import type { GymBodyWeight, GymWeeklyBodyWeight } from "@clipper/mobile-bridge";
import { formatBackendError } from "../backend";
import { formatDay, formatKg, formatTime, gym, parseWeight } from "./gymClient";
import { colors, GymCard, LineChart, Muted, NumberEntry } from "./GymUi";

export function BodyWeight({ onError }: { onError: (error: string | null) => void }) {
  const [entries, setEntries] = useState<GymBodyWeight[]>([]);
  const [weeks, setWeeks] = useState<GymWeeklyBodyWeight[]>([]);
  const [weight, setWeight] = useState("");
  const [busy, setBusy] = useState(false);
  const busyRef = useRef(false);
  const seeded = useRef(false);

  const load = useCallback(async () => {
    try {
      const [list, weekly] = await Promise.all([
        gym().gymBodyWeights(),
        gym().gymWeeklyBodyWeight(),
      ]);
      setEntries(list);
      setWeeks(weekly);
      const latest = list[0];
      if (!seeded.current && latest) {
        seeded.current = true;
        setWeight(formatKg(latest.kg));
      }
    } catch (caught) {
      onError(formatBackendError(caught));
    }
  }, [onError]);

  useEffect(() => {
    void load();
  }, [load]);

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

  const kg = parseWeight(weight);

  return (
    <ScrollView flex={1} keyboardShouldPersistTaps="always">
      <YStack gap="$3" pb="$8">
        <H2 size="$6">Body weight</H2>
        <GymCard>
          <YStack gap="$3">
            <NumberEntry
              label="Weight (kg)"
              value={weight}
              onChange={setWeight}
              step={0.1}
              decimal
            />
            <Button
              theme="green"
              size="$5"
              disabled={busy || kg === undefined}
              onPress={() => kg !== undefined && void run(() => gym().gymAddBodyWeight(kg))}
            >
              Log weight now
            </Button>
          </YStack>
        </GymCard>

        <GymCard>
          <YStack gap="$2">
            <Text fontWeight="600">Weekly average</Text>
            {weeks.length === 0 ? (
              <Muted>Log a weight to see the weekly average</Muted>
            ) : (
              <>
                <LineChart
                  points={weeks.map((week) => ({
                    x: Date.parse(`${week.weekStart}T00:00:00Z`),
                    y: week.averageKg,
                  }))}
                  formatY={(value) => `${formatKg(Math.round(value * 10) / 10)}`}
                  formatX={(value) =>
                    new Date(value).toLocaleDateString(undefined, {
                      month: "short",
                      day: "numeric",
                    })
                  }
                />
                {weeks.toReversed().map((week) => (
                  <XStack key={week.weekStart} justify="space-between">
                    <Text color={colors.muted}>{`Week of ${week.weekStart}`}</Text>
                    <Text>
                      {`${formatKg(Math.round(week.averageKg * 10) / 10)} kg`}
                      {week.changeKg === undefined
                        ? ""
                        : ` (${week.changeKg >= 0 ? "+" : ""}${formatKg(Math.round(week.changeKg * 10) / 10)})`}
                    </Text>
                  </XStack>
                ))}
              </>
            )}
          </YStack>
        </GymCard>

        <YStack gap="$2">
          <Text fontWeight="600">Entries</Text>
          {entries.length === 0 && <Muted>No weigh-ins yet</Muted>}
          {entries.map((entry) => (
            <XStack
              key={entry.id}
              items="center"
              justify="space-between"
              gap="$2"
              px="$3"
              py="$2"
              rounded="$3"
              bg={colors.card}
            >
              <YStack flex={1}>
                <Text>{`${formatKg(entry.kg)} kg`}</Text>
                <Muted>{`${formatDay(entry.timeMillis)} · ${formatTime(entry.timeMillis)}`}</Muted>
              </YStack>
              <Button
                size="$3"
                aria-label="Delete weigh-in"
                icon={<Trash2 size={16} color={colors.bad} />}
                disabled={busy}
                onPress={() =>
                  Alert.alert("Delete this weigh-in?", undefined, [
                    { text: "Cancel", style: "cancel" },
                    {
                      text: "Delete",
                      style: "destructive",
                      onPress: () => void run(() => gym().gymDeleteBodyWeight(entry.id)),
                    },
                  ])
                }
              />
            </XStack>
          ))}
        </YStack>
      </YStack>
    </ScrollView>
  );
}
