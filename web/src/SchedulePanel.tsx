import { CalendarClock, ChevronLeft, ChevronRight, Plus, Trash2 } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
    Button,
    Card,
    H2,
    Input,
    Label,
    Paragraph,
    Spinner,
    Text,
    XStack,
    YStack,
    type TamaguiElement,
} from "tamagui";
import type {
    AppState,
    OccurrenceView,
    Recurrence,
    ScheduleItem,
    ScheduleItemView,
    Weekday,
} from "@clipper/shared";
import { clipperBackend, formatBackendError } from "./backend";

// The grid is a view, not the storage format: blocks are stored as an interval
// and rasterized here (docs/schedule-plan.md, D5). SLOT_MINUTES is the snap the
// form applies to a new block; ingested meetings are not grid-aligned and are
// drawn wherever they actually fall.
const SLOT_MINUTES = 5;
const HOUR_HEIGHT = 44;
const DAY_MINUTES = 24 * 60;
const DAY_HEIGHT = (HOUR_HEIGHT * DAY_MINUTES) / 60;

const WEEKDAYS: Weekday[] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
const WEEKDAY_LABELS: Record<Weekday, string> = {
    mon: "Mon",
    tue: "Tue",
    wed: "Wed",
    thu: "Thu",
    fri: "Fri",
    sat: "Sat",
    sun: "Sun",
};

type RepeatChoice = "once" | "daily" | "weekly" | "weekdays" | "monthly";

/// The viewer's IANA zone. Floating and all-day blocks have no zone of their
/// own, so this is what resolves them (D5).
function observerZone(): string {
    try {
        return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
    } catch {
        return "UTC";
    }
}

export function SchedulePanel({
    items,
    onState,
    onError,
}: {
    items: ScheduleItemView[];
    onState: (state: AppState) => void;
    onError: (error: string | null) => void;
}) {
    const [weekStart, setWeekStart] = useState(() => startOfWeek(new Date()));
    const [occurrences, setOccurrences] = useState<OccurrenceView[]>([]);
    const [loading, setLoading] = useState(false);

    const weekEnd = useMemo(() => addDays(weekStart, 7), [weekStart]);

    const loadWeek = useCallback(async () => {
        setLoading(true);
        try {
            const backend = await clipperBackend();
            setOccurrences(
                await backend.expandSchedule(
                    weekStart.toISOString(),
                    weekEnd.toISOString(),
                    observerZone(),
                ),
            );
        } catch (caught) {
            onError(formatBackendError(caught));
        } finally {
            setLoading(false);
        }
    }, [weekStart, weekEnd, onError]);

    // Re-expand whenever the window moves or the series set changes. `items` is
    // the dependency that matters for the latter: a create or delete republishes
    // state, which re-renders this panel with a new array.
    useEffect(() => {
        void loadWeek();
    }, [loadWeek, items]);

    return (
        <YStack gap="$3">
            <ScheduleComposer onState={onState} onError={onError} />

            <Card bg="#171a1d" p="$3" gap="$3" style={{ borderColor: "#252b31", borderWidth: 1 }}>
                <XStack items="center" justify="space-between" gap="$2" flexWrap="wrap">
                    <XStack items="center" gap="$2">
                        <H2 size="$5">{weekLabel(weekStart)}</H2>
                        {loading && <Spinner size="small" />}
                    </XStack>
                    <XStack gap="$2">
                        <Button
                            size="$2"
                            icon={<ChevronLeft size={16} />}
                            onPress={() => setWeekStart(addDays(weekStart, -7))}
                            aria-label="Previous week"
                        />
                        <Button size="$2" onPress={() => setWeekStart(startOfWeek(new Date()))}>
                            Today
                        </Button>
                        <Button
                            size="$2"
                            icon={<ChevronRight size={16} />}
                            onPress={() => setWeekStart(addDays(weekStart, 7))}
                            aria-label="Next week"
                        />
                    </XStack>
                </XStack>

                <WeekGrid weekStart={weekStart} occurrences={occurrences} />
            </Card>

            <SeriesList items={items} onState={onState} onError={onError} />
        </YStack>
    );
}

function WeekGrid({
    weekStart,
    occurrences,
}: {
    weekStart: Date;
    occurrences: OccurrenceView[];
}) {
    const days = useMemo(
        () => Array.from({ length: 7 }, (_, index) => addDays(weekStart, index)),
        [weekStart],
    );
    const allDay = occurrences.filter((occurrence) => occurrence.all_day);
    const timed = occurrences.filter((occurrence) => !occurrence.all_day);
    const today = startOfDay(new Date()).getTime();
    const scroller = useRef<TamaguiElement | null>(null);

    // Open on the week's earliest block rather than at midnight, which is eight
    // hours of empty grid before anything a person scheduled.
    const firstMinute = useMemo(() => {
        const starts = timed.map((occurrence) => {
            const start = new Date(occurrence.start);
            return start.getHours() * 60 + start.getMinutes();
        });
        return starts.length > 0 ? Math.min(...starts) : 8 * 60;
    }, [timed]);

    useEffect(() => {
        const node = scroller.current;
        if (!(node instanceof HTMLElement)) return;
        // A little headroom above the first block so it does not sit flush
        // against the top edge.
        node.scrollTop = Math.max(0, ((firstMinute - 30) / 60) * HOUR_HEIGHT);
    }, [firstMinute]);

    return (
        // Horizontal scroll lives here rather than on the page: a seven-day grid
        // on a narrow window must not make the whole app scroll sideways.
        <YStack style={{ overflowX: "auto" }}>
            <YStack minW={720}>
                <XStack>
                    <YStack width={56} />
                    {days.map((day) => (
                        <YStack
                            key={day.toISOString()}
                            flex={1}
                            items="center"
                            py="$1"
                            style={{
                                borderLeftColor: "#252b31",
                                borderLeftWidth: 1,
                                backgroundColor:
                                    startOfDay(day).getTime() === today ? "#1d2329" : undefined,
                            }}
                        >
                            <Text fontSize={12} color="#8b949e">
                                {weekdayLabel(day)}
                            </Text>
                            <Text fontSize={14}>{day.getDate()}</Text>
                        </YStack>
                    ))}
                </XStack>

                {allDay.length > 0 && (
                    <XStack style={{ borderTopColor: "#252b31", borderTopWidth: 1 }}>
                        <YStack width={56} items="flex-end" pr="$2" py="$1">
                            <Text fontSize={11} color="#8b949e">
                                all day
                            </Text>
                        </YStack>
                        {days.map((day) => (
                            <YStack
                                key={day.toISOString()}
                                flex={1}
                                gap={2}
                                p={2}
                                style={{ borderLeftColor: "#252b31", borderLeftWidth: 1 }}
                            >
                                {allDay
                                    .filter((occurrence) => overlapsDay(occurrence, day))
                                    .map((occurrence) => (
                                        <OccurrenceChip
                                            key={`${occurrence.item_id}-${occurrence.start}`}
                                            occurrence={occurrence}
                                        />
                                    ))}
                            </YStack>
                        ))}
                    </XStack>
                )}

                <XStack
                    ref={scroller}
                    style={{
                        borderTopColor: "#252b31",
                        borderTopWidth: 1,
                        maxHeight: 520,
                        overflowY: "auto",
                    }}
                >
                    <YStack width={56}>
                        {Array.from({ length: 24 }, (_, hour) => (
                            <YStack key={hour} height={HOUR_HEIGHT} items="flex-end" pr="$2">
                                <Text fontSize={11} color="#8b949e">
                                    {String(hour).padStart(2, "0")}:00
                                </Text>
                            </YStack>
                        ))}
                    </YStack>
                    {days.map((day) => (
                        <YStack
                            key={day.toISOString()}
                            flex={1}
                            height={DAY_HEIGHT}
                            style={{
                                position: "relative",
                                borderLeftColor: "#252b31",
                                borderLeftWidth: 1,
                            }}
                        >
                            {Array.from({ length: 24 }, (_, hour) => (
                                <YStack
                                    key={hour}
                                    height={HOUR_HEIGHT}
                                    style={{ borderTopColor: "#1c2126", borderTopWidth: 1 }}
                                />
                            ))}
                            {timed
                                .filter((occurrence) => overlapsDay(occurrence, day))
                                .map((occurrence) => (
                                    <TimedBlock
                                        key={`${occurrence.item_id}-${occurrence.start}`}
                                        occurrence={occurrence}
                                        day={day}
                                    />
                                ))}
                        </YStack>
                    ))}
                </XStack>
            </YStack>
        </YStack>
    );
}

function TimedBlock({ occurrence, day }: { occurrence: OccurrenceView; day: Date }) {
    const dayStart = startOfDay(day).getTime();
    const start = new Date(occurrence.start).getTime();
    const end = new Date(occurrence.end).getTime();
    // An occurrence can begin the previous day or run past midnight; clamp it to
    // this column so a long block draws as a band rather than escaping the grid.
    const fromMinutes = Math.max(0, (start - dayStart) / 60000);
    const toMinutes = Math.min(DAY_MINUTES, (end - dayStart) / 60000);
    const top = (fromMinutes / 60) * HOUR_HEIGHT;
    const height = Math.max(14, ((toMinutes - fromMinutes) / 60) * HOUR_HEIGHT);

    return (
        <YStack
            px={4}
            py={1}
            style={{
                position: "absolute",
                top,
                height,
                left: 2,
                right: 2,
                overflow: "hidden",
                borderRadius: 4,
                backgroundColor: occurrence.overridden ? "#3d3320" : "#1f3350",
                borderLeftColor: occurrence.overridden ? "#d0a33a" : "#4d8fd6",
                borderLeftWidth: 3,
            }}
            aria-label={`${occurrence.title}, ${clockRange(occurrence)}`}
        >
            <Text fontSize={11} lineHeight={13} numberOfLines={1}>
                {occurrence.title}
            </Text>
            {height >= 34 && (
                <Text fontSize={10} lineHeight={12} color="#8b949e" numberOfLines={1}>
                    {clockRange(occurrence)}
                </Text>
            )}
        </YStack>
    );
}

function OccurrenceChip({ occurrence }: { occurrence: OccurrenceView }) {
    return (
        <YStack
            px={4}
            py={1}
            style={{
                borderRadius: 4,
                backgroundColor: occurrence.overridden ? "#3d3320" : "#243a2c",
            }}
        >
            <Text fontSize={11} numberOfLines={1}>
                {occurrence.title}
            </Text>
        </YStack>
    );
}

function SeriesList({
    items,
    onState,
    onError,
}: {
    items: ScheduleItemView[];
    onState: (state: AppState) => void;
    onError: (error: string | null) => void;
}) {
    const [deleting, setDeleting] = useState<string | null>(null);

    async function remove(id: string) {
        setDeleting(id);
        onError(null);
        try {
            const backend = await clipperBackend();
            await backend.deleteScheduleObject(id);
            onState(await backend.getState());
        } catch (caught) {
            onError(formatBackendError(caught));
        } finally {
            setDeleting(null);
        }
    }

    if (items.length === 0) {
        return (
            <Card
                bg="#171a1d"
                p="$4"
                items="center"
                gap="$2"
                style={{ borderColor: "#252b31", borderWidth: 1 }}
            >
                <CalendarClock size={28} color="#8b949e" />
                <Paragraph color="#8b949e">Nothing scheduled yet</Paragraph>
            </Card>
        );
    }

    return (
        <YStack gap="$2">
            {items.map((item) => (
                <Card
                    key={item.id}
                    bg="#171a1d"
                    p="$3"
                    style={{ borderColor: "#252b31", borderWidth: 1 }}
                >
                    <XStack items="center" justify="space-between" gap="$3" flexWrap="wrap">
                        <YStack flex={1} minW={200}>
                            <Text>{item.title || "Untitled"}</Text>
                            <Text fontSize={12} color="#8b949e">
                                {item.recurrence} · {item.time_summary}
                            </Text>
                        </YStack>
                        <Button
                            size="$2"
                            icon={
                                deleting === item.id ? <Spinner /> : <Trash2 size={14} />
                            }
                            disabled={deleting === item.id}
                            onPress={() => void remove(item.id)}
                        >
                            Delete
                        </Button>
                    </XStack>
                </Card>
            ))}
        </YStack>
    );
}

function ScheduleComposer({
    onState,
    onError,
}: {
    onState: (state: AppState) => void;
    onError: (error: string | null) => void;
}) {
    const [open, setOpen] = useState(false);
    const [busy, setBusy] = useState(false);
    const [title, setTitle] = useState("");
    const [date, setDate] = useState(() => isoDate(new Date()));
    const [time, setTime] = useState("09:00");
    const [duration, setDuration] = useState("30");
    const [allDay, setAllDay] = useState(false);
    const [floating, setFloating] = useState(false);
    const [repeat, setRepeat] = useState<RepeatChoice>("once");
    const [days, setDays] = useState<Weekday[]>(["mon", "wed", "fri"]);

    async function submit() {
        onError(null);
        const minutes = Number.parseInt(duration, 10);
        if (!allDay && (!Number.isFinite(minutes) || minutes <= 0)) {
            onError("Duration must be at least one minute");
            return;
        }
        if (repeat === "weekly" && days.length === 0) {
            onError("Pick at least one weekday");
            return;
        }

        setBusy(true);
        try {
            const item: ScheduleItem = {
                id: crypto.randomUUID(),
                title: title.trim() || "Untitled",
                span: allDay
                    ? { kind: "all_day", start: date, days: 1 }
                    : {
                          kind: "timed",
                          start: floating
                              ? { kind: "floating", at: `${date}T${time}:00` }
                              : {
                                    kind: "zoned",
                                    at: { local: `${date}T${time}:00`, zone: observerZone() },
                                },
                          duration: snapMinutes(minutes),
                      },
                recurrence: buildRecurrence(repeat, days, date),
                reference: null,
            };
            const backend = await clipperBackend();
            await backend.createScheduleItem(item);
            onState(await backend.getState());
            setTitle("");
            setOpen(false);
        } catch (caught) {
            onError(formatBackendError(caught));
        } finally {
            setBusy(false);
        }
    }

    if (!open) {
        return (
            <XStack>
                <Button icon={<Plus size={16} />} onPress={() => setOpen(true)}>
                    New block
                </Button>
            </XStack>
        );
    }

    return (
        <Card bg="#171a1d" p="$3" gap="$3" style={{ borderColor: "#252b31", borderWidth: 1 }}>
            <XStack gap="$2" flexWrap="wrap" items="flex-end">
                <Field label="Title">
                    <Input
                        value={title}
                        onChangeText={setTitle}
                        placeholder="Gym"
                        width={200}
                        autoFocus
                    />
                </Field>
                <Field label="Date">
                    <Input value={date} onChangeText={setDate} width={150} />
                </Field>
                {!allDay && (
                    <>
                        <Field label="Start">
                            <Input value={time} onChangeText={setTime} width={100} />
                        </Field>
                        <Field label={`Minutes (snaps to ${SLOT_MINUTES})`}>
                            <Input
                                value={duration}
                                onChangeText={setDuration}
                                width={110}
                                keyboardType="numeric"
                            />
                        </Field>
                    </>
                )}
            </XStack>

            <XStack gap="$2" flexWrap="wrap">
                <Toggle on={allDay} onPress={() => setAllDay(!allDay)}>
                    All day
                </Toggle>
                {!allDay && (
                    <Toggle on={floating} onPress={() => setFloating(!floating)}>
                        Floating time
                    </Toggle>
                )}
            </XStack>
            {floating && !allDay && (
                <Paragraph fontSize={12} color="#8b949e">
                    A floating block keeps its wall-clock time when you travel — 07:00 stays 07:00.
                    A zoned one stays pinned to {observerZone()}.
                </Paragraph>
            )}

            <XStack gap="$2" flexWrap="wrap">
                {(["once", "daily", "weekdays", "weekly", "monthly"] as RepeatChoice[]).map(
                    (choice) => (
                        <Toggle
                            key={choice}
                            on={repeat === choice}
                            onPress={() => setRepeat(choice)}
                        >
                            {repeatLabel(choice)}
                        </Toggle>
                    ),
                )}
            </XStack>

            {repeat === "weekly" && (
                <XStack gap="$2" flexWrap="wrap">
                    {WEEKDAYS.map((day) => (
                        <Toggle
                            key={day}
                            on={days.includes(day)}
                            onPress={() =>
                                setDays(
                                    days.includes(day)
                                        ? days.filter((existing) => existing !== day)
                                        : [...days, day],
                                )
                            }
                        >
                            {WEEKDAY_LABELS[day]}
                        </Toggle>
                    ))}
                </XStack>
            )}

            <XStack gap="$2">
                <Button
                    theme="blue"
                    disabled={busy}
                    icon={busy ? <Spinner /> : undefined}
                    onPress={() => void submit()}
                >
                    Create
                </Button>
                <Button disabled={busy} onPress={() => setOpen(false)}>
                    Cancel
                </Button>
            </XStack>
        </Card>
    );
}

function Toggle({
    on,
    onPress,
    children,
}: {
    on: boolean;
    onPress: () => void;
    children: ReactNode;
}) {
    return (
        <Button size="$2" theme={on ? "blue" : undefined} onPress={onPress}>
            {children}
        </Button>
    );
}

function Field({ label, children }: { label: string; children: ReactNode }) {
    return (
        <YStack gap="$1">
            <Label fontSize={12} color="#8b949e">
                {label}
            </Label>
            {children}
        </YStack>
    );
}

function buildRecurrence(choice: RepeatChoice, days: Weekday[], date: string): Recurrence {
    switch (choice) {
        case "once":
            return { kind: "once" };
        case "daily":
            return {
                kind: "every",
                frequency: { unit: "daily" },
                interval: 1,
                end: { when: "never" },
            };
        case "weekdays":
            return {
                kind: "every",
                frequency: {
                    unit: "weekly",
                    weekdays: ["mon", "tue", "wed", "thu", "fri"],
                    week_start: "mon",
                },
                interval: 1,
                end: { when: "never" },
            };
        case "weekly":
            return {
                kind: "every",
                frequency: { unit: "weekly", weekdays: days, week_start: "mon" },
                interval: 1,
                end: { when: "never" },
            };
        case "monthly":
            return {
                kind: "every",
                frequency: {
                    unit: "monthly",
                    by: "on_day",
                    from: "from_start",
                    // The day the block starts on, so "monthly" means "this date
                    // every month" without asking a second question.
                    day: Number.parseInt(date.slice(8, 10), 10) || 1,
                },
                interval: 1,
                end: { when: "never" },
            };
    }
}

function repeatLabel(choice: RepeatChoice): string {
    switch (choice) {
        case "once":
            return "Once";
        case "daily":
            return "Daily";
        case "weekdays":
            return "Weekdays";
        case "weekly":
            return "Weekly";
        case "monthly":
            return "Monthly";
    }
}

/// Round to the grid. A block the user typed is snapped; one that arrived from a
/// calendar is not (D5) — a meeting that runs 09:07–09:23 is ordinary.
function snapMinutes(minutes: number): number {
    return Math.max(SLOT_MINUTES, Math.round(minutes / SLOT_MINUTES) * SLOT_MINUTES);
}

/// Monday-based label for a date. `Date.getDay()` is Sunday-based, so the shift
/// is what makes Monday index 0.
function weekdayLabel(date: Date): string {
    const day = WEEKDAYS[(date.getDay() + 6) % 7];
    return day ? WEEKDAY_LABELS[day] : "";
}

function startOfWeek(date: Date): Date {
    const start = startOfDay(date);
    // getDay() is Sunday-based; the grid starts on Monday.
    start.setDate(start.getDate() - ((start.getDay() + 6) % 7));
    return start;
}

function startOfDay(date: Date): Date {
    const copy = new Date(date);
    copy.setHours(0, 0, 0, 0);
    return copy;
}

function addDays(date: Date, days: number): Date {
    const copy = new Date(date);
    copy.setDate(copy.getDate() + days);
    return copy;
}

function overlapsDay(occurrence: OccurrenceView, day: Date): boolean {
    const dayStart = startOfDay(day).getTime();
    const dayEnd = dayStart + DAY_MINUTES * 60000;
    const start = new Date(occurrence.start).getTime();
    const end = new Date(occurrence.end).getTime();
    // Half-open on both sides, so a block ending exactly at midnight belongs to
    // the day it started in and not to the next one.
    return start < dayEnd && end > dayStart;
}

function clockRange(occurrence: OccurrenceView): string {
    return `${clock(occurrence.start)}–${clock(occurrence.end)}`;
}

function clock(iso: string): string {
    const date = new Date(iso);
    return `${String(date.getHours()).padStart(2, "0")}:${String(date.getMinutes()).padStart(2, "0")}`;
}

function isoDate(date: Date): string {
    return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, "0")}-${String(date.getDate()).padStart(2, "0")}`;
}

function weekLabel(weekStart: Date): string {
    const end = addDays(weekStart, 6);
    const sameMonth = weekStart.getMonth() === end.getMonth();
    const month = weekStart.toLocaleDateString(undefined, { month: "short" });
    const endMonth = end.toLocaleDateString(undefined, { month: "short" });
    return sameMonth
        ? `${month} ${weekStart.getDate()}–${end.getDate()}, ${end.getFullYear()}`
        : `${month} ${weekStart.getDate()} – ${endMonth} ${end.getDate()}, ${end.getFullYear()}`;
}
