import { Button } from "../tamagui.config";
import { palette, fatigueColors } from "@clipper/shared";
import { useCallback, useEffect, useRef, useState } from "react";
import { AppState as NativeAppState } from "react-native";
import { H2, ScrollView, Spinner, Text, XStack, YStack } from "tamagui";
import { FatigueBand, MuscleGroup, type GymMuscleFatigue } from "@clipper/mobile-bridge";
import { formatBackendError } from "../backend";
import { gym } from "./gymClient";
import { GymCard, Muted, Stepper } from "./GymUi";

const bandLabels: Record<FatigueBand, string> = {
  [FatigueBand.Recovered]: "Recovered",
  [FatigueBand.Low]: "Low",
  [FatigueBand.Moderate]: "Moderate",
  [FatigueBand.High]: "High",
  [FatigueBand.VeryHigh]: "Very high",
};

const bandColors = {
  [FatigueBand.Recovered]: fatigueColors.recovered,
  [FatigueBand.Low]: fatigueColors.low,
  [FatigueBand.Moderate]: fatigueColors.moderate,
  [FatigueBand.High]: fatigueColors.high,
  [FatigueBand.VeryHigh]: fatigueColors.veryHigh,
} as const satisfies Record<FatigueBand, string>;

const groups: { group: MuscleGroup; label: string }[] = [
  { group: MuscleGroup.Push, label: "Push" },
  { group: MuscleGroup.Pull, label: "Pull" },
  { group: MuscleGroup.Legs, label: "Legs" },
  { group: MuscleGroup.Core, label: "Core" },
];

export function Fatigue({ onError }: { onError: (error: string | null) => void }) {
  const [muscles, setMuscles] = useState<GymMuscleFatigue[] | null>(null);
  const [editing, setEditing] = useState(false);
  const busyRef = useRef(false);

  const load = useCallback(async () => {
    try {
      setMuscles(await gym().gymFatigue());
    } catch (caught) {
      onError(formatBackendError(caught));
      setMuscles([]);
    }
  }, [onError]);

  useEffect(() => {
    void load();
    const timer = setInterval(() => void load(), 60_000);
    const subscription = NativeAppState.addEventListener("change", (nextState) => {
      if (nextState === "active") void load();
    });
    return () => {
      clearInterval(timer);
      subscription.remove();
    };
  }, [load]);

  async function setDays(muscle: GymMuscleFatigue, days: number) {
    if (busyRef.current) return;
    busyRef.current = true;
    onError(null);
    try {
      await gym().gymSetRecoveryDays(muscle.muscle, days);
    } catch (caught) {
      onError(formatBackendError(caught));
    } finally {
      busyRef.current = false;
      await load();
    }
  }

  if (muscles === null) {
    return (
      <YStack flex={1} items="center" justify="center">
        <Spinner />
      </YStack>
    );
  }

  return (
    <ScrollView flex={1} keyboardShouldPersistTaps="always">
      <YStack gap="$3" pb="$8">
        <XStack items="center" justify="space-between">
          <H2 size="$6">Fatigue</H2>
          <Button size="$3" selected={editing} onPress={() => setEditing(!editing)}>
            {editing ? "Done" : "Recovery days"}
          </Button>
        </XStack>
        <Muted>From working sets in the last two weeks, by muscle share and reps in reserve.</Muted>
        {groups.map(({ group, label }) => (
          <GymCard key={label}>
            <YStack gap="$3">
              <Text fontWeight="600">{label}</Text>
              {muscles
                .filter((muscle) => muscle.group === group)
                .map((muscle) => (
                  <YStack key={muscle.displayName} gap="$1">
                    <XStack justify="space-between">
                      <Text>{muscle.displayName}</Text>
                      <Text color={bandColors[muscle.band]}>
                        {`${bandLabels[muscle.band]} · ${muscle.score}%`}
                      </Text>
                    </XStack>
                    <YStack height={8} rounded={4} bg={palette.pageFill} overflow="hidden">
                      <YStack
                        height={8}
                        width={`${Math.max(muscle.score, 1)}%`}
                        bg={bandColors[muscle.band]}
                      />
                    </YStack>
                    {editing && (
                      <YStack gap="$1">
                        <Stepper
                          label={`Recovery days${muscle.recoveryDays === muscle.defaultRecoveryDays ? " (default)" : ""}`}
                          value={muscle.recoveryDays}
                          step={0.5}
                          min={0.5}
                          max={14}
                          format={(days) => `${days} d`}
                          onChange={(days) => void setDays(muscle, days)}
                        />
                        {muscle.recoveryDays !== muscle.defaultRecoveryDays && (
                          <Button
                            size="$2"
                            self="flex-end"
                            onPress={() => void setDays(muscle, muscle.defaultRecoveryDays)}
                          >
                            {`Reset to ${muscle.defaultRecoveryDays} d`}
                          </Button>
                        )}
                      </YStack>
                    )}
                  </YStack>
                ))}
            </YStack>
          </GymCard>
        ))}
      </YStack>
    </ScrollView>
  );
}
