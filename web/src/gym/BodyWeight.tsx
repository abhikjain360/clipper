import { Trash2 } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { Spinner, Text, XStack, YStack } from "tamagui";
import { Button } from "../tamagui.config";
import {
    deviceZone,
    formatDay,
    formatKg,
    formatShortDate,
    formatTime,
    localDate,
    palette,
    parseWeight,
    type GymBackend,
    type GymBodyWeight,
} from "@clipper/shared";
import {
    CardTitle,
    Column,
    Columns,
    ConfirmDialog,
    GymCard,
    LineChart,
    Muted,
    NumberField,
    useGymChange,
    useGymData,
    type ErrorHandler,
} from "./GymUi";

export function BodyWeight({
    backend,
    state,
    onError,
}: {
    backend: GymBackend;
    state: unknown;
    onError: ErrorHandler;
}) {
    const load = useCallback(
        () => Promise.all([backend.bodyWeights(), backend.weeklyBodyWeight(deviceZone())]),
        [backend],
    );
    const { value, reload } = useGymData(load, state, onError);
    const { busy, run } = useGymChange(reload, onError);
    const [weight, setWeight] = useState("");
    const [deleting, setDeleting] = useState<GymBodyWeight | null>(null);
    const seeded = useRef(false);
    const latest = value?.[0][0];

    useEffect(() => {
        if (seeded.current || !latest) return;
        seeded.current = true;
        setWeight(formatKg(latest.kg));
    }, [latest]);

    if (value === undefined) return <Spinner />;
    const [entries, weeks] = value;
    const kg = parseWeight(weight);
    function logWeight() {
        if (kg !== undefined) void run(() => backend.change({ change: "add_body_weight", kg }));
    }

    return (
        <Columns>
            <Column grow={2} minWidth={300}>
                <GymCard>
                    <CardTitle>Log body weight</CardTitle>
                    <NumberField
                        label="Weight (kg)"
                        value={weight}
                        onChange={setWeight}
                        step={0.1}
                        decimal
                        onSubmit={logWeight}
                    />
                    <XStack>
                        <Button
                            tone="success"
                            disabled={busy || kg === undefined}
                            onPress={logWeight}
                        >
                            Log weight now
                        </Button>
                    </XStack>
                </GymCard>
                <GymCard>
                    <CardTitle>Entries</CardTitle>
                    {entries.length === 0 && <Muted>No weigh-ins yet</Muted>}
                    <YStack gap="$1">
                        {entries.map((entry) => (
                            <XStack
                                key={entry.id}
                                items="center"
                                justify="space-between"
                                gap="$2"
                                px="$3"
                                py="$2"
                                rounded="$3"
                                bg={palette.pageFill}
                            >
                                <Text minW={80} fontWeight="600">{`${formatKg(entry.kg)} kg`}</Text>
                                <Text flex={1} color={palette.secondary}>
                                    {`${formatDay(entry.time_millis)} · ${formatTime(entry.time_millis)}`}
                                </Text>
                                <Button
                                    size="$2"
                                    chromeless
                                    aria-label={`Delete the weigh-in of ${formatDay(entry.time_millis)}`}
                                    icon={<Trash2 size={14} />}
                                    disabled={busy}
                                    onPress={() => setDeleting(entry)}
                                />
                            </XStack>
                        ))}
                    </YStack>
                </GymCard>
            </Column>
            <Column grow={3} minWidth={420}>
                <GymCard>
                    <CardTitle>Weekly average</CardTitle>
                    {weeks.length === 0 ? (
                        <Muted>Log a weight to see the weekly average</Muted>
                    ) : (
                        <>
                            <LineChart
                                label="Weekly average body weight"
                                points={weeks.map((week) => ({
                                    x: localDate(week.week_start).getTime(),
                                    y: week.average_kg,
                                }))}
                                formatTick={(average) => formatKg(Math.round(average * 10) / 10)}
                                formatValue={(average) =>
                                    `${formatKg(Math.round(average * 10) / 10)} kg`
                                }
                                formatX={(millis) => `Week of ${formatShortDate(new Date(millis))}`}
                            />
                            <YStack gap="$1">
                                <XStack gap="$3" px="$3">
                                    <Text
                                        flex={2}
                                        flexBasis={0}
                                        fontSize={12}
                                        color={palette.secondary}
                                    >
                                        Week
                                    </Text>
                                    <Text
                                        flex={1}
                                        flexBasis={0}
                                        fontSize={12}
                                        color={palette.secondary}
                                    >
                                        Average
                                    </Text>
                                    <Text
                                        flex={1}
                                        flexBasis={0}
                                        fontSize={12}
                                        color={palette.secondary}
                                    >
                                        Change
                                    </Text>
                                    <Text
                                        flex={1}
                                        flexBasis={0}
                                        fontSize={12}
                                        color={palette.secondary}
                                    >
                                        Weigh-ins
                                    </Text>
                                </XStack>
                                {weeks.toReversed().map((week) => (
                                    <XStack
                                        key={week.week_start}
                                        gap="$3"
                                        px="$3"
                                        py="$1.5"
                                        rounded="$3"
                                        bg={palette.pageFill}
                                    >
                                        <Text flex={2} flexBasis={0}>
                                            {`Week of ${formatShortDate(localDate(week.week_start))}`}
                                        </Text>
                                        <Text flex={1} flexBasis={0}>
                                            {`${formatKg(Math.round(week.average_kg * 10) / 10)} kg`}
                                        </Text>
                                        <Text flex={1} flexBasis={0} color={palette.secondary}>
                                            {week.change_kg === null
                                                ? "–"
                                                : `${week.change_kg >= 0 ? "+" : ""}${formatKg(Math.round(week.change_kg * 10) / 10)}`}
                                        </Text>
                                        <Text flex={1} flexBasis={0} color={palette.secondary}>
                                            {String(week.measurements)}
                                        </Text>
                                    </XStack>
                                ))}
                            </YStack>
                        </>
                    )}
                </GymCard>
            </Column>
            <ConfirmDialog
                open={deleting !== null}
                title="Delete this weigh-in?"
                description={
                    deleting
                        ? `${formatKg(deleting.kg)} kg on ${formatDay(deleting.time_millis)}.`
                        : ""
                }
                confirmLabel="Delete"
                busy={busy}
                onCancel={() => setDeleting(null)}
                onConfirm={() => {
                    const entry = deleting;
                    setDeleting(null);
                    if (entry)
                        void run(() =>
                            backend.change({ change: "delete_body_weight", id: entry.id }),
                        );
                }}
            />
        </Columns>
    );
}
