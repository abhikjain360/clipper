import {
    AlarmClock,
    CalendarClock,
    ChevronLeft,
    ChevronRight,
    Pencil,
    Play,
    Plus,
    RefreshCw,
    Square,
    Trash2,
} from "lucide-react";
import {
    cloneElement,
    isValidElement,
    useCallback,
    useEffect,
    useId,
    useMemo,
    useRef,
    useState,
    type ReactNode,
} from "react";
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
    ActualView,
    AppState,
    CalendarSourceView,
    OccurrenceView,
    Recurrence,
    ScheduleItem,
    ScheduleItemView,
    Weekday,
} from "@clipper/shared";
import { clipperBackend, formatBackendError, isTauriRuntime } from "./backend";
import { layoutDay, overlapsDay, spanMinutes } from "./schedule-layout";

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
    sources,
    running,
    onState,
    onError,
}: {
    items: ScheduleItemView[];
    sources: CalendarSourceView[];
    /** The timer currently running, if any. */
    running: ActualView | null;
    onState: (state: AppState) => void;
    onError: (error: string | null) => void;
}) {
    const [editing, setEditing] = useState<ScheduleItemView | null>(null);
    const [weekStart, setWeekStart] = useState(() => startOfWeek(new Date()));
    const [occurrences, setOccurrences] = useState<OccurrenceView[]>([]);
    const [actuals, setActuals] = useState<ActualView[]>([]);
    const [loading, setLoading] = useState(false);
    const loadGeneration = useRef(0);
    const [starting, setStarting] = useState(false);

    const weekEnd = useMemo(() => addDays(weekStart, 7), [weekStart]);

    const loadWeek = useCallback(async () => {
        const generation = ++loadGeneration.current;
        setLoading(true);
        try {
            const backend = await clipperBackend();
            const [expanded, logged] = await Promise.all([
                backend.expandSchedule(
                    weekStart.toISOString(),
                    weekEnd.toISOString(),
                    observerZone(),
                ),
                backend.actualsBetween(weekStart.toISOString(), weekEnd.toISOString()),
            ]);
            if (generation === loadGeneration.current) {
                setOccurrences(expanded);
                setActuals(logged);
            }
        } catch (caught) {
            if (generation === loadGeneration.current) onError(formatBackendError(caught));
        } finally {
            if (generation === loadGeneration.current) setLoading(false);
        }
    }, [weekStart, weekEnd, onError]);

    // Re-expand whenever the window moves or the series set changes. `items` is
    // the dependency that matters for the latter: a create or delete republishes
    // state, which re-renders this panel with a new array.
    useEffect(() => {
        void loadWeek();
        return () => {
            loadGeneration.current += 1;
        };
    }, [loadWeek, items, sources, running]);

    return (
        <YStack gap="$3">
            <RunningTimer running={running} onState={onState} onError={onError} />

            <ScheduleComposer
                editing={editing}
                onDone={() => setEditing(null)}
                onState={onState}
                onError={onError}
            />

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

                <WeekGrid
                    weekStart={weekStart}
                    occurrences={occurrences}
                    actuals={actuals}
                    onStart={async (occurrence) => {
                        if (starting) return;
                        setStarting(true);
                        onError(null);
                        try {
                            const backend = await clipperBackend();
                            await backend.startActual(
                                occurrence.item_id,
                                occurrence.occurrence_key,
                            );
                            onState(await backend.getState());
                        } catch (caught) {
                            onError(formatBackendError(caught));
                        } finally {
                            setStarting(false);
                        }
                    }}
                />
            </Card>

            <SeriesList items={items} onEdit={setEditing} onState={onState} onError={onError} />
            <CalendarSources sources={sources} onState={onState} onError={onError} />
        </YStack>
    );
}

function WeekGrid({
    weekStart,
    occurrences,
    actuals,
    onStart,
}: {
    weekStart: Date;
    occurrences: OccurrenceView[];
    actuals: ActualView[];
    onStart: (occurrence: OccurrenceView) => void;
}) {
    const days = useMemo(
        () => Array.from({ length: 7 }, (_, index) => addDays(weekStart, index)),
        [weekStart],
    );
    const allDay = occurrences.filter((occurrence) => occurrence.all_day);
    const timed = occurrences.filter((occurrence) => !occurrence.all_day);
    const today = startOfDay(new Date()).getTime();
    const scroller = useRef<TamaguiElement | null>(null);
    // The grid scrolls vertically and the rows above it do not, so on any
    // platform with classic (non-overlay) scrollbars the grid is narrower than
    // the header and every day column drifts. Measure the difference and
    // reserve it above rather than guessing a width.
    const [gutter, setGutter] = useState(0);

    // Open on the week's earliest block rather than at midnight, which is eight
    // hours of empty grid before anything a person scheduled. A running timer
    // wins outright: it is the one thing happening right now, and scrolling to
    // a 07:00 block would hide it below the fold.
    const running = actuals.find((actual) => actual.running);
    const [, tick] = useState(0);
    useEffect(() => {
        if (!running) return;
        const timer = setInterval(() => tick((value) => value + 1), 1000);
        return () => clearInterval(timer);
    }, [running]);
    const firstMinute = useMemo(() => {
        if (running) {
            const start = new Date(running.start);
            return start.getHours() * 60 + start.getMinutes();
        }
        const starts = timed.map((occurrence) => {
            const start = new Date(occurrence.start);
            return start.getHours() * 60 + start.getMinutes();
        });
        return starts.length > 0 ? Math.min(...starts) : 8 * 60;
    }, [timed, running]);

    useEffect(() => {
        const node = scroller.current;
        if (!(node instanceof HTMLElement)) return;
        // A little headroom above the first block so it does not sit flush
        // against the top edge.
        node.scrollTop = Math.max(0, ((firstMinute - 30) / 60) * HOUR_HEIGHT);
        setGutter(node.offsetWidth - node.clientWidth);
    }, [firstMinute, occurrences.length]);

    return (
        // Horizontal scroll lives here rather than on the page: a seven-day grid
        // on a narrow window must not make the whole app scroll sideways.
        <YStack style={{ overflowX: "auto" }}>
            <YStack minW={720}>
                <XStack pr={gutter}>
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
                    <XStack pr={gutter} style={{ borderTopColor: "#252b31", borderTopWidth: 1 }}>
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
                            {layoutDay(timed, day).map(({ span: occurrence, lane, lanes }) => (
                                <TimedBlock
                                    key={`${occurrence.item_id}-${occurrence.start}`}
                                    occurrence={occurrence}
                                    day={day}
                                    onStart={onStart}
                                    lane={lane}
                                    lanes={lanes}
                                />
                            ))}
                            {actuals
                                .filter((actual) => overlapsDay(actual, day))
                                .map((actual) => (
                                    <ActualBlock key={actual.id} actual={actual} day={day} />
                                ))}
                        </YStack>
                    ))}
                </XStack>
            </YStack>
        </YStack>
    );
}

/// Time actually spent, drawn as a narrow band down the right of the column.
///
/// Beside the plan rather than over it: the entire point of D2 is being able to
/// see the difference, which a single merged block would hide.
function ActualBlock({ actual, day }: { actual: ActualView; day: Date }) {
    const { top, height } = bandGeometry(actual, day);
    return (
        <YStack
            style={{
                position: "absolute",
                top,
                height,
                right: 2,
                width: 6,
                borderRadius: 3,
                backgroundColor: actual.running ? "#d0a33a" : "#7bd88f",
                opacity: 0.85,
            }}
            aria-label={`${actual.title}, actual${actual.running ? ", running" : ""}`}
        />
    );
}

function TimedBlock({
    occurrence,
    day,
    onStart,
    lane,
    lanes,
}: {
    occurrence: OccurrenceView;
    day: Date;
    onStart: (occurrence: OccurrenceView) => void;
    lane: number;
    lanes: number;
}) {
    const { top, height } = bandGeometry(occurrence, day);

    return (
        <YStack
            px={4}
            py={1}
            style={{
                position: "absolute",
                top,
                height,
                left: `calc(${(lane / lanes) * 100}% + 2px)`,
                width: `calc(${100 / lanes}% - 4px)`,
                overflow: "hidden",
                borderRadius: 4,
                backgroundColor: blockColor(occurrence).fill,
                borderLeftColor: blockColor(occurrence).accent,
                borderLeftWidth: 3,
                opacity: occurrence.cancelled ? 0.55 : 1,
            }}
            aria-label={`${occurrence.title}, ${clockRange(occurrence)}${
                occurrence.source ? `, from ${occurrence.source}` : ""
            }${occurrence.cancelled ? ", cancelled" : ""}`}
            onPress={() => onStart(occurrence)}
            cursor="pointer"
        >
            <Text
                fontSize={11}
                lineHeight={13}
                numberOfLines={1}
                textDecorationLine={occurrence.cancelled ? "line-through" : "none"}
            >
                {occurrence.title}
            </Text>
            {height >= 34 && (
                <Text fontSize={10} lineHeight={12} color="#8b949e" numberOfLines={1}>
                    {occurrence.source
                        ? `${clockRange(occurrence)} · ${occurrence.source}`
                        : clockRange(occurrence)}
                </Text>
            )}
        </YStack>
    );
}

/// Owned blocks, ingested events and moved occurrences should be
/// distinguishable at a glance, since only the first is editable.
function blockColor(occurrence: OccurrenceView): { fill: string; accent: string } {
    if (occurrence.cancelled) return { fill: "#2a2226", accent: "#8b6b6b" };
    if (occurrence.overridden) return { fill: "#3d3320", accent: "#d0a33a" };
    if (occurrence.source) return { fill: "#1d3330", accent: "#4dbfa5" };
    return { fill: "#1f3350", accent: "#4d8fd6" };
}

/// Where a span sits in a day column.
///
/// Clamped to the column: a block can begin the previous day or run past
/// midnight, and should draw as a band rather than escaping the grid.
function bandGeometry(
    span: { start: string; end: string },
    day: Date,
): { top: number; height: number } {
    const [fromMinutes, toMinutes] = spanMinutes(span, day);
    return {
        top: (fromMinutes / 60) * HOUR_HEIGHT,
        height: Math.max(6, ((toMinutes - fromMinutes) / 60) * HOUR_HEIGHT),
    };
}

/// The running timer, with what it is against and how long it has been going.
function RunningTimer({
    running,
    onState,
    onError,
}: {
    running: ActualView | null;
    onState: (state: AppState) => void;
    onError: (error: string | null) => void;
}) {
    const [now, setNow] = useState(() => Date.now());
    const [busy, setBusy] = useState(false);

    // Ticks the *display* only. The record itself is written twice and no more
    // — on start and on stop (D2) — because every write is a retained object.
    useEffect(() => {
        if (!running) return;
        const timer = setInterval(() => setNow(Date.now()), 1000);
        return () => clearInterval(timer);
    }, [running]);

    async function act(
        run: (backend: Awaited<ReturnType<typeof clipperBackend>>) => Promise<unknown>,
    ) {
        setBusy(true);
        onError(null);
        try {
            const backend = await clipperBackend();
            await run(backend);
            onState(await backend.getState());
        } catch (caught) {
            onError(formatBackendError(caught));
        } finally {
            setBusy(false);
        }
    }

    if (!running) {
        return (
            <XStack gap="$2">
                <Button
                    icon={<Play size={16} />}
                    disabled={busy}
                    onPress={() => void act((backend) => backend.startActual())}
                >
                    Start untracked time
                </Button>
            </XStack>
        );
    }

    const elapsed = Math.max(0, now - new Date(running.start).getTime());
    return (
        <Card bg="#2a2416" p="$3" style={{ borderColor: "#d0a33a", borderWidth: 1 }}>
            <XStack items="center" justify="space-between" gap="$3" flexWrap="wrap">
                <YStack>
                    <Text>{running.title}</Text>
                    <Text fontSize={12} color="#d0a33a">
                        Running · {formatElapsed(elapsed)}
                    </Text>
                </YStack>
                <Button
                    theme="yellow"
                    icon={busy ? <Spinner /> : <Square size={14} />}
                    disabled={busy}
                    onPress={() => void act((backend) => backend.stopActual(running.id))}
                >
                    Stop
                </Button>
            </XStack>
        </Card>
    );
}

function formatElapsed(ms: number): string {
    const total = Math.floor(ms / 1000);
    const hours = Math.floor(total / 3600);
    const minutes = Math.floor((total % 3600) / 60);
    const seconds = total % 60;
    return hours > 0 ? `${hours}:${pad(minutes)}:${pad(seconds)}` : `${minutes}:${pad(seconds)}`;
}

function pad(value: number): string {
    return String(value).padStart(2, "0");
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

function CalendarSources({
    sources,
    onState,
    onError,
}: {
    sources: CalendarSourceView[];
    onState: (state: AppState) => void;
    onError: (error: string | null) => void;
}) {
    const [adding, setAdding] = useState(false);
    const [name, setName] = useState("");
    const [url, setUrl] = useState("");
    const [busy, setBusy] = useState<string | null>(null);
    const canSync = isTauriRuntime();

    async function add() {
        onError(null);
        setBusy("add");
        try {
            const backend = await clipperBackend();
            await backend.addCalendarSource(name.trim() || "Calendar", url.trim());
            onState(await backend.getState());
            setName("");
            setUrl("");
            setAdding(false);
        } catch (caught) {
            onError(formatBackendError(caught));
        } finally {
            setBusy(null);
        }
    }

    async function sync(id: string) {
        onError(null);
        setBusy(id);
        try {
            const backend = await clipperBackend();
            const report = await backend.syncCalendarSource(id);
            onState(await backend.getState());
            // Say what actually happened. A sync that silently does nothing is
            // indistinguishable from one that failed.
            const parts = [
                report.added > 0 ? `${report.added} added` : null,
                report.updated > 0 ? `${report.updated} updated` : null,
                report.tombstoned > 0 ? `${report.tombstoned} cancelled` : null,
                report.skipped.length > 0 ? `${report.skipped.length} unreadable` : null,
            ].filter(Boolean);
            onError(
                parts.length > 0
                    ? `Synced: ${parts.join(", ")}`
                    : `Synced: no changes (${report.unchanged} events)`,
            );
        } catch (caught) {
            onError(formatBackendError(caught));
        } finally {
            setBusy(null);
        }
    }

    async function remove(id: string) {
        onError(null);
        setBusy(id);
        try {
            const backend = await clipperBackend();
            await backend.deleteScheduleObject(id);
            onState(await backend.getState());
        } catch (caught) {
            onError(formatBackendError(caught));
        } finally {
            setBusy(null);
        }
    }

    return (
        <Card bg="#171a1d" p="$3" gap="$3" style={{ borderColor: "#252b31", borderWidth: 1 }}>
            <XStack items="center" justify="space-between" gap="$2">
                <H2 size="$5">Calendars</H2>
                {!adding && (
                    <Button size="$2" icon={<Plus size={14} />} onPress={() => setAdding(true)}>
                        Add feed
                    </Button>
                )}
            </XStack>

            {adding && (
                <YStack gap="$2">
                    <XStack gap="$2" flexWrap="wrap" items="flex-end">
                        <Field label="Name">
                            <Input
                                value={name}
                                onChangeText={setName}
                                placeholder="Work"
                                width={160}
                            />
                        </Field>
                        <Field label="iCalendar URL (secret address)">
                            <Input
                                value={url}
                                onChangeText={setUrl}
                                placeholder="https://calendar.google.com/calendar/ical/…/basic.ics"
                                width={380}
                            />
                        </Field>
                    </XStack>
                    <Paragraph fontSize={12} color="#8b949e">
                        The URL is stored encrypted — the server never sees it. Anyone holding it
                        can read the calendar, so treat it like a password. Feed refresh runs in the
                        desktop app.
                    </Paragraph>
                    <XStack gap="$2">
                        <Button
                            theme="blue"
                            disabled={busy === "add" || url.trim().length === 0}
                            icon={busy === "add" ? <Spinner /> : undefined}
                            onPress={() => void add()}
                        >
                            Add
                        </Button>
                        <Button onPress={() => setAdding(false)}>Cancel</Button>
                    </XStack>
                </YStack>
            )}

            {sources.length === 0 && !adding && (
                <Paragraph color="#8b949e">No calendars connected</Paragraph>
            )}
            {sources.length > 0 && !canSync && (
                <Paragraph fontSize={12} color="#8b949e">
                    Open the desktop app to refresh calendar feeds.
                </Paragraph>
            )}

            {sources.map((source) => (
                <XStack
                    key={source.id}
                    items="center"
                    justify="space-between"
                    gap="$3"
                    flexWrap="wrap"
                >
                    <YStack flex={1} minW={200}>
                        <Text>{source.name}</Text>
                        <Text fontSize={12} color="#8b949e">
                            {source.protocol} · {source.location} · {source.event_count} events
                        </Text>
                    </YStack>
                    <XStack gap="$2">
                        <Button
                            size="$2"
                            icon={busy === source.id ? <Spinner /> : <RefreshCw size={14} />}
                            disabled={!canSync || busy === source.id}
                            onPress={() => void sync(source.id)}
                        >
                            Sync
                        </Button>
                        <Button
                            size="$2"
                            icon={<Trash2 size={14} />}
                            disabled={busy === source.id}
                            onPress={() => void remove(source.id)}
                        >
                            Remove
                        </Button>
                    </XStack>
                </XStack>
            ))}
        </Card>
    );
}

function SeriesList({
    items,
    onEdit,
    onState,
    onError,
}: {
    items: ScheduleItemView[];
    onEdit: (item: ScheduleItemView) => void;
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
                            <XStack items="center" gap="$2">
                                <Text fontSize={12} color="#8b949e">
                                    {item.recurrence} · {item.time_summary}
                                </Text>
                                {item.has_alarm && <AlarmClock size={12} color="#d0a33a" />}
                            </XStack>
                        </YStack>
                        <XStack gap="$2">
                            <Button
                                size="$2"
                                icon={<Pencil size={14} />}
                                onPress={() => onEdit(item)}
                            >
                                Edit
                            </Button>
                            <Button
                                size="$2"
                                icon={deleting === item.id ? <Spinner /> : <Trash2 size={14} />}
                                disabled={deleting === item.id}
                                onPress={() => void remove(item.id)}
                            >
                                Delete
                            </Button>
                        </XStack>
                    </XStack>
                </Card>
            ))}
        </YStack>
    );
}

function ScheduleComposer({
    editing,
    onDone,
    onState,
    onError,
}: {
    /** The block being edited, or null to compose a new one. */
    editing: ScheduleItemView | null;
    onDone: () => void;
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
    const [alarm, setAlarm] = useState(false);
    const [alarmLead, setAlarmLead] = useState("0");
    // The series id is preserved across an edit so overrides and logged time
    // keep pointing at the same series; only the object carrying it changes.
    const [seriesId, setSeriesId] = useState<string | null>(null);
    const original = useRef<ScheduleItem | null>(null);
    const [spanChanged, setSpanChanged] = useState(false);
    const [recurrenceChanged, setRecurrenceChanged] = useState(false);
    const [allDayDays, setAllDayDays] = useState("1");
    const [zone, setZone] = useState(observerZone);

    // Load an existing block into the form, once per block.
    //
    // Keyed on a ref rather than on the effect's dependencies: a sync push
    // re-renders this panel, and re-running the load would overwrite whatever
    // the owner had typed since opening it. Depending on the callbacks would be
    // worse still — their identity changes every render.
    const loadedObjectId = useRef<string | null>(null);
    useEffect(() => {
        if (!editing) {
            loadedObjectId.current = null;
            return;
        }
        if (loadedObjectId.current === editing.id) return;
        loadedObjectId.current = editing.id;

        const parsed = parseDefinition(editing.definition_json);
        if (!parsed) {
            onError("That block could not be opened for editing");
            onDone();
            return;
        }
        setSeriesId(parsed.id);
        original.current = parsed;
        setSpanChanged(false);
        setRecurrenceChanged(false);
        setZone(
            parsed.span.kind === "timed" && parsed.span.start.kind === "zoned"
                ? parsed.span.start.at.zone
                : observerZone(),
        );
        setTitle(parsed.title);
        setAlarm(parsed.alarm != null);
        setAlarmLead(String(parsed.alarm?.minutes_before ?? 0));
        setRepeat(repeatChoiceOf(parsed.recurrence));
        setDays(weekdaysOf(parsed.recurrence) ?? ["mon", "wed", "fri"]);
        if (parsed.span.kind === "all_day") {
            setAllDay(true);
            setDate(parsed.span.start);
            setAllDayDays(String(parsed.span.days));
        } else {
            setAllDay(false);
            setFloating(parsed.span.start.kind === "floating");
            const local =
                parsed.span.start.kind === "floating"
                    ? parsed.span.start.at
                    : parsed.span.start.at.local;
            setDate(local.slice(0, 10));
            setTime(local.slice(11, 16));
            setDuration(String(parsed.span.duration));
        }
        setOpen(true);
    }, [editing, onDone, onError]);
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
        if (allDay && (!/^\d+$/.test(allDayDays) || Number(allDayDays) < 1)) {
            onError("All-day duration must be at least one whole day");
            return;
        }

        setBusy(true);
        try {
            const item: ScheduleItem = {
                id: seriesId ?? crypto.randomUUID(),
                title: title.trim() || "Untitled",
                span:
                    original.current && !spanChanged
                        ? original.current.span
                        : allDay
                          ? { kind: "all_day", start: date, days: Number(allDayDays) }
                          : {
                                kind: "timed",
                                start: floating
                                    ? { kind: "floating", at: `${date}T${time}:00` }
                                    : {
                                          kind: "zoned",
                                          at: { local: `${date}T${time}:00`, zone },
                                      },
                                duration: snapMinutes(minutes),
                            },
                recurrence:
                    original.current && !recurrenceChanged
                        ? original.current.recurrence
                        : buildRecurrence(repeat, days, date),
                reference: original.current?.reference ?? null,
                alarm: alarm
                    ? { minutes_before: Math.max(0, Number.parseInt(alarmLead, 10) || 0) }
                    : null,
            };
            const backend = await clipperBackend();
            if (editing) {
                const current = (await backend.getState()).schedule_items.find(
                    (entry) => entry.id === editing.id,
                );
                if (!current || current.definition_json !== editing.definition_json) {
                    throw new Error(
                        "This block changed on another device. Cancel and reopen it before saving.",
                    );
                }
                await backend.updateScheduleItem(editing.id, item);
            } else {
                await backend.createScheduleItem(item);
            }
            onState(await backend.getState());
            reset();
        } catch (caught) {
            onError(formatBackendError(caught));
        } finally {
            setBusy(false);
        }
    }

    function reset() {
        setTitle("");
        setSeriesId(null);
        original.current = null;
        setSpanChanged(false);
        setRecurrenceChanged(false);
        setZone(observerZone());
        setAllDayDays("1");
        setOpen(false);
        onDone();
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
                    <Input
                        value={date}
                        onChangeText={(value) => {
                            setDate(value);
                            setSpanChanged(true);
                        }}
                        width={150}
                    />
                </Field>
                {!allDay && (
                    <>
                        <Field label="Start">
                            <Input
                                value={time}
                                onChangeText={(value) => {
                                    setTime(value);
                                    setSpanChanged(true);
                                }}
                                width={100}
                            />
                        </Field>
                        <Field label={`Minutes (snaps to ${SLOT_MINUTES})`}>
                            <Input
                                value={duration}
                                onChangeText={(value) => {
                                    setDuration(value);
                                    setSpanChanged(true);
                                }}
                                width={110}
                                keyboardType="numeric"
                            />
                        </Field>
                    </>
                )}
                {allDay && (
                    <Field label="Days">
                        <Input
                            value={allDayDays}
                            onChangeText={(value) => {
                                setAllDayDays(value);
                                setSpanChanged(true);
                            }}
                            width={100}
                        />
                    </Field>
                )}
            </XStack>

            <XStack gap="$2" flexWrap="wrap" items="flex-end">
                <Toggle on={alarm} onPress={() => setAlarm(!alarm)}>
                    <XStack items="center" gap="$2">
                        <AlarmClock size={14} />
                        <Text>Alarm</Text>
                    </XStack>
                </Toggle>
                {alarm && (
                    <Field label="Minutes before">
                        <Input value={alarmLead} onChangeText={setAlarmLead} width={110} />
                    </Field>
                )}
            </XStack>
            {alarm && (
                <Paragraph fontSize={12} color="#8b949e">
                    Alarms ring on Android only, where an exact alarm can survive a reboot and sound
                    through Do Not Disturb. Other devices show the block without ringing.
                </Paragraph>
            )}

            <XStack gap="$2" flexWrap="wrap">
                <Toggle
                    on={allDay}
                    onPress={() => {
                        setAllDay(!allDay);
                        setSpanChanged(true);
                    }}
                >
                    All day
                </Toggle>
                {!allDay && (
                    <Toggle
                        on={floating}
                        onPress={() => {
                            setFloating(!floating);
                            setSpanChanged(true);
                        }}
                    >
                        Floating time
                    </Toggle>
                )}
            </XStack>
            {floating && !allDay && (
                <Paragraph fontSize={12} color="#8b949e">
                    A floating block keeps its wall-clock time when you travel — 07:00 stays 07:00.
                    A zoned one stays pinned to {zone}.
                </Paragraph>
            )}
            {!floating && !allDay && (
                <Paragraph fontSize={12} color="#8b949e">
                    Times use {zone}.
                </Paragraph>
            )}
            {editing && !recurrenceChanged && (
                <Paragraph fontSize={12} color="#8b949e">
                    Saved recurrence: {editing.recurrence}. Kept unless you change repeat settings
                    below.
                </Paragraph>
            )}

            <XStack gap="$2" flexWrap="wrap">
                {(["once", "daily", "weekdays", "weekly", "monthly"] as RepeatChoice[]).map(
                    (choice) => (
                        <Toggle
                            key={choice}
                            on={repeat === choice}
                            onPress={() => {
                                setRepeat(choice);
                                setRecurrenceChanged(true);
                            }}
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
                            onPress={() => {
                                setRecurrenceChanged(true);
                                setDays(
                                    days.includes(day)
                                        ? days.filter((existing) => existing !== day)
                                        : [...days, day],
                                );
                            }}
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
                    {editing ? "Save" : "Create"}
                </Button>
                <Button disabled={busy} onPress={reset}>
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
    const id = useId();
    return (
        <YStack gap="$1">
            <Label htmlFor={id} fontSize={12} color="#8b949e">
                {label}
            </Label>
            {isValidElement<{ id?: string }>(children) ? cloneElement(children, { id }) : children}
        </YStack>
    );
}

/// Read back a stored record. A record this build cannot parse is reported
/// rather than silently replaced with a default, which would quietly rewrite
/// the owner's block on save.
function parseDefinition(json: string): ScheduleItem | null {
    try {
        const parsed = JSON.parse(json) as ScheduleItem;
        return parsed.span && parsed.recurrence ? parsed : null;
    } catch {
        return null;
    }
}

/// Map a stored recurrence back onto the form's coarser choices.
///
/// The form offers a handful of common cadences while the record can express
/// more. The original recurrence is retained until repeat settings are changed;
/// the composer also shows its full stored summary.
function repeatChoiceOf(recurrence: Recurrence): RepeatChoice {
    if (recurrence.kind !== "every" || recurrence.interval !== 1) return "once";
    switch (recurrence.frequency.unit) {
        case "daily":
            return "daily";
        case "weekly":
            return isWeekdaySet(recurrence.frequency.weekdays) ? "weekdays" : "weekly";
        case "monthly":
            return recurrence.frequency.by === "on_day" ? "monthly" : "once";
        default:
            return "once";
    }
}

function weekdaysOf(recurrence: Recurrence): Weekday[] | null {
    return recurrence.kind === "every" && recurrence.frequency.unit === "weekly"
        ? recurrence.frequency.weekdays
        : null;
}

function isWeekdaySet(days: Weekday[]): boolean {
    const workweek: Weekday[] = ["mon", "tue", "wed", "thu", "fri"];
    return days.length === workweek.length && workweek.every((day) => days.includes(day));
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
