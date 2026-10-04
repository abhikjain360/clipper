import { useGymChange } from "./actions";
import { Button } from "./Button";
import { Trash2 } from "lucide-react-native";
import { useCallback, useEffect, useRef, useState } from "react";
import { Alert } from "react-native";
import { H2, ScrollView, Text, XStack, YStack } from "tamagui";
import type { GymBodyWeight, GymWeeklyBodyWeight } from "@clipper/mobile-bridge";
import {
  deviceZone,
  formatDay,
  formatKg,
  formatShortDate,
  formatTime,
  localDate,
  parseWeight,
} from "@clipper/shared";
import { formatBackendError } from "../backend";
import { gym } from "./gymClient";
import { colors, GymCard, LineChart, Muted, NumberEntry } from "./GymUi";

export function BodyWeight({ onError }: { onError: (error: string | null) => void }) {
  const [entries, setEntries] = useState<GymBodyWeight[]>([]);
  const [weeks, setWeeks] = useState<GymWeeklyBodyWeight[]>([]);
  const [weight, setWeight] = useState("");
  const seeded = useRef(false);

  const load = useCallback(async () => {
    try {
      const [list, weekly] = await Promise.all([
        gym().gymBodyWeights(),
        gym().gymWeeklyBodyWeight(deviceZone()),
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

  const { busy, run } = useGymChange(load, onError);

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
              tone="success"
              size="$5"
              busy={busy}
              disabled={kg === undefined}
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
                    x: localDate(week.weekStart).getTime(),
                    y: week.averageKg,
                  }))}
                  formatY={(value) => `${formatKg(Math.round(value * 10) / 10)}`}
                  formatX={(value) => formatShortDate(new Date(value))}
                />
                {weeks.toReversed().map((week) => (
                  <XStack key={week.weekStart} justify="space-between">
                    <Text
                      color={colors.muted}
                    >{`Week of ${formatShortDate(localDate(week.weekStart))}`}</Text>
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
                busy={busy}
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
