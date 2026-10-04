import { useCallback, useEffect, useState } from "react";
import { H2, Spinner, Text, XStack, YStack } from "tamagui";
import { Button } from "../tamagui.config";
import {
    fatigueColors,
    palette,
    type GymBackend,
    type GymFatigueBand,
    type GymMuscleFatigue,
} from "@clipper/shared";
import {
    CardTitle,
    GymCard,
    Muted,
    muscleGroups,
    Stepper,
    useGymChange,
    useGymData,
    type ErrorHandler,
} from "./GymUi";

const bandLabels: Record<GymFatigueBand, string> = {
    recovered: "Recovered",
    low: "Low",
    moderate: "Moderate",
    high: "High",
    very_high: "Very high",
};

const bandColors = {
    recovered: fatigueColors.recovered,
    low: fatigueColors.low,
    moderate: fatigueColors.moderate,
    high: fatigueColors.high,
    very_high: fatigueColors.veryHigh,
} as const satisfies Record<GymFatigueBand, string>;

export function Fatigue({
    backend,
    state,
    onError,
}: {
    backend: GymBackend;
    state: unknown;
    onError: ErrorHandler;
}) {
    const load = useCallback(() => backend.fatigue(), [backend]);
    const { value: muscles, reload } = useGymData(load, state, onError);
    const { busy, run } = useGymChange(reload, onError);
    const [editing, setEditing] = useState(false);

    useEffect(() => {
        const timer = setInterval(reload, 60_000);
        return () => clearInterval(timer);
    }, [reload]);

    if (muscles === undefined) return <Spinner />;

    function setDays(muscle: GymMuscleFatigue, days: number) {
        void run(() =>
            backend.change({
                change: "set_recovery_days",
                muscle: muscle.muscle,
                recovery_days: days,
            }),
        );
    }

    return (
        <YStack gap="$4">
            <XStack items="center" justify="space-between" gap="$3" flexWrap="wrap">
                <YStack>
                    <H2 size="$6">Fatigue</H2>
                    <Muted>
                        From working sets in the last two weeks, by muscle share and reps in
                        reserve.
                    </Muted>
                </YStack>
                <Button
                    aria-pressed={editing}
                    theme={editing ? "blue" : undefined}
                    onPress={() => setEditing(!editing)}
                >
                    {editing ? "Done" : "Recovery days"}
                </Button>
            </XStack>
            <XStack gap="$4" flexWrap="wrap" items="flex-start">
                {muscleGroups.map(({ group, label }) => (
                    <YStack key={group} grow={1} flexBasis={420} minW={320}>
                        <GymCard>
                            <CardTitle>{label}</CardTitle>
                            {muscles
                                .filter((muscle) => muscle.group === group)
                                .map((muscle) => (
                                    <YStack key={muscle.muscle} gap="$1.5">
                                        <XStack justify="space-between" items="center" gap="$2">
                                            <Text>{muscle.display_name}</Text>
                                            <XStack items="center" gap="$2">
                                                <YStack
                                                    width={10}
                                                    height={10}
                                                    rounded={5}
                                                    bg={bandColors[muscle.band]}
                                                />
                                                <Text color={palette.secondary}>
                                                    {`${bandLabels[muscle.band]} · ${muscle.score}%`}
                                                </Text>
                                            </XStack>
                                        </XStack>
                                        <YStack
                                            height={8}
                                            rounded={4}
                                            bg={palette.raised}
                                            overflow="hidden"
                                            role="meter"
                                            aria-label={`${muscle.display_name} fatigue`}
                                            aria-valuenow={muscle.score}
                                            aria-valuemin={0}
                                            aria-valuemax={100}
                                        >
                                            <YStack
                                                height={8}
                                                width={`${Math.max(muscle.score, 1)}%`}
                                                bg={bandColors[muscle.band]}
                                            />
                                        </YStack>
                                        {editing && (
                                            <XStack items="center" gap="$2">
                                                <YStack flex={1}>
                                                    <Stepper
                                                        label={`Recovery days${muscle.recovery_days === muscle.default_recovery_days ? " (default)" : ""}`}
                                                        value={muscle.recovery_days}
                                                        step={0.5}
                                                        min={0.5}
                                                        max={14}
                                                        format={(days) => `${days} d`}
                                                        onChange={(days) => setDays(muscle, days)}
                                                    />
                                                </YStack>
                                                {muscle.recovery_days !==
                                                    muscle.default_recovery_days && (
                                                    <Button
                                                        size="$2"
                                                        disabled={busy}
                                                        onPress={() =>
                                                            setDays(
                                                                muscle,
                                                                muscle.default_recovery_days,
                                                            )
                                                        }
                                                    >
                                                        {`Reset to ${muscle.default_recovery_days} d`}
                                                    </Button>
                                                )}
                                            </XStack>
                                        )}
                                    </YStack>
                                ))}
                        </GymCard>
                    </YStack>
                ))}
            </XStack>
        </YStack>
    );
}
