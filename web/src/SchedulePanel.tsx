import { CalendarDatePicker } from "./CalendarDatePicker";
import { calendarWindow, movePeriod, periodStart, type CalendarView } from "./calendar-view";
import { EventHover } from "./EventHover";
import { nextStarts, orderByNextStart } from "./schedule-order";
import {
    AlarmClock,
    CalendarClock,
    ChevronLeft,
    ChevronRight,
    Check,
    ChevronDown,
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
    type CSSProperties,
} from "react";
import {
    Button,
    Card,
    Dialog,
    Select,
    H2,
    Input,
    Label,
    Paragraph,
    Spinner,
    Text,
    ToggleGroup,
    XStack,
    YStack,
    type TamaguiElement,
} from "tamagui";
import type {
    ActualView,
    AppState,
    CalendarSourceView,
    OccurrenceView,
    ScheduleItem,
    ScheduleItemView,
    Weekday,
} from "@clipper/shared";
import { clipperBackend, formatBackendError, isTauriRuntime } from "./backend";
import { layoutDay, overlapsDay, spanMinutes } from "./schedule-layout";
import {
    buildRecurrence,
    repeatChoiceOf,
    repeatLabel,
    weekdaysOf,
    type RepeatSelection,
} from "./schedule-recurrence";

// The grid is a view, not the storage format: blocks are stored as an interval
// and rasterized here. SLOT_MINUTES is the snap the
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

/// The viewer's IANA zone. Floating and all-day blocks have no zone of their
/// own, so this is what resolves them.
function observerZone(): string {
    try {
        return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
    } catch {
        return "UTC";
    }
}

export function SchedulePanel({
    items,
    warnings,
    sources,
    running,
    onState,
    onError,
}: {
    items: ScheduleItemView[];
    warnings: string[];
    sources: CalendarSourceView[];
    /** The timer currently running, if any. */
    running: ActualView | null;
    onState: (state: AppState) => void;
    onError: (error: string | null) => void;
}) {
    const workspace = useRef<HTMLDivElement>(null);
    const [sidebarWidth, setSidebarWidth] = useState(() => {
        try {
            const saved = Number(localStorage.getItem("clipper.schedule.sidebar-width"));
            return Number.isFinite(saved) && saved >= 300 ? Math.min(saved, 720) : 360;
        } catch {
            return 360;
        }
    });
    const [maxSidebarWidth, setMaxSidebarWidth] = useState(720);
    const [resizing, setResizing] = useState(false);
    const drag = useRef<{ x: number; width: number } | null>(null);
    const displayedWidth = Math.min(sidebarWidth, maxSidebarWidth);
    useEffect(() => {
        const node = workspace.current;
        if (!node) return;
        const observer = new ResizeObserver(() =>
            setMaxSidebarWidth(Math.max(300, Math.min(720, node.clientWidth - 560))),
        );
        observer.observe(node);
        return () => observer.disconnect();
    }, []);
    useEffect(() => {
        if (resizing) return;
        try {
            localStorage.setItem("clipper.schedule.sidebar-width", String(sidebarWidth));
        } catch {
            /* Layout still works when storage is unavailable. */
        }
    }, [sidebarWidth, resizing]);
    const resizeSidebar = (width: number) =>
        setSidebarWidth(Math.round(Math.max(300, Math.min(maxSidebarWidth, width))));
    const [editing, setEditing] = useState<ScheduleItemView | null>(null);
    const [view, setView] = useState<CalendarView>("week");
    const [selectedDate, setSelectedDate] = useState(() => startOfDay(new Date()));
    const { start: weekStart, end: weekEnd } = useMemo(
        () => calendarWindow(selectedDate, view),
        [selectedDate, view],
    );
    const [occurrences, setOccurrences] = useState<OccurrenceView[]>([]);
    const [actuals, setActuals] = useState<ActualView[]>([]);
    const [loading, setLoading] = useState(false);
    const loadGeneration = useRef(0);
    const [starting, setStarting] = useState(false);

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
            {warnings.length > 0 && (
                <YStack role="status" gap="$1">
                    {warnings.map((warning, index) => (
                        <Paragraph key={`${index}:${warning}`} color="#f3c969" size="$3">
                            {warning}
                        </Paragraph>
                    ))}
                </YStack>
            )}

            <div
                ref={workspace}
                className={`schedule-workspace${resizing ? " is-resizing" : ""}`}
                style={{ "--sidebar-width": `${displayedWidth}px` } as CSSProperties}
            >
                <Card
                    bg="#171a1d"
                    p="$3"
                    gap="$3"
                    style={{ borderColor: "#252b31", borderWidth: 1 }}
                >
                    <XStack items="center" justify="space-between" gap="$2" flexWrap="wrap">
                        <XStack items="center" gap="$2">
                            {loading && <Spinner size="small" />}
                        </XStack>
                        <XStack gap="$2" flexWrap="wrap" items="center" minW={0} maxW="100%">
                            <ToggleGroup
                                type="single"
                                value={view}
                                disableDeactivation
                                aria-label="Calendar view"
                                onValueChange={(value) => {
                                    if (value === "day" || value === "week" || value === "month")
                                        setView(value);
                                }}
                            >
                                <XStack>
                                    {(["day", "week", "month"] as const).map((mode) => (
                                        <ToggleGroup.Item
                                            // The child Button theme supplies the selected styling.
                                            activeStyle={{}}
                                            asChild
                                            key={mode}
                                            value={mode}
                                            aria-label={
                                                mode.slice(0, 1).toUpperCase() + mode.slice(1)
                                            }
                                        >
                                            <Button
                                                size="$2"
                                                theme={view === mode ? "blue" : undefined}
                                            >
                                                {mode.slice(0, 1).toUpperCase() + mode.slice(1)}
                                            </Button>
                                        </ToggleGroup.Item>
                                    ))}
                                </XStack>
                            </ToggleGroup>
                            <Button
                                size="$2"
                                icon={<ChevronLeft size={16} />}
                                onPress={() => setSelectedDate(movePeriod(selectedDate, view, -1))}
                                aria-label={`Previous ${view}`}
                            />
                            <CalendarDatePicker value={selectedDate} onChange={setSelectedDate}>
                                {view === "week"
                                    ? weekLabel(weekStart)
                                    : selectedDate.toLocaleDateString(
                                          undefined,
                                          view === "month"
                                              ? { month: "long", year: "numeric" }
                                              : {
                                                    weekday: "long",
                                                    month: "short",
                                                    day: "numeric",
                                                    year: "numeric",
                                                },
                                      )}
                            </CalendarDatePicker>
                            <Button
                                size="$2"
                                icon={<ChevronRight size={16} />}
                                onPress={() => setSelectedDate(movePeriod(selectedDate, view, 1))}
                                aria-label={`Next ${view}`}
                            />
                        </XStack>
                    </XStack>
                    {periodStart(selectedDate, view).getTime() !==
                        periodStart(new Date(), view).getTime() && (
                        <Button
                            size="$2"
                            self="flex-start"
                            onPress={() => setSelectedDate(startOfDay(new Date()))}
                        >
                            {view === "day" ? "Back to today" : `Back to this ${view}`}
                        </Button>
                    )}

                    {view === "month" ? (
                        <MonthGrid
                            start={weekStart}
                            end={weekEnd}
                            selectedMonth={selectedDate.getMonth()}
                            occurrences={occurrences}
                            actuals={actuals}
                            onDay={(day) => {
                                setSelectedDate(day);
                                setView("day");
                            }}
                        />
                    ) : (
                        <WeekGrid
                            dayCount={view === "day" ? 1 : 7}
                            weekStart={weekStart}
                            occurrences={occurrences}
                            actuals={actuals}
                            onStart={async (occurrence) => {
                                if (starting) return;
                                setStarting(true);
                                onError(null);
                                try {
                                    const backend = await clipperBackend();
                                    await backend.startActual(occurrence.plan_context);
                                    onState(await backend.getState());
                                } catch (caught) {
                                    onError(formatBackendError(caught));
                                } finally {
                                    setStarting(false);
                                }
                            }}
                        />
                    )}
                </Card>

                <div
                    className="schedule-divider"
                    role="separator"
                    aria-label="Resize event list"
                    aria-orientation="vertical"
                    aria-valuemin={300}
                    aria-valuemax={maxSidebarWidth}
                    aria-valuenow={displayedWidth}
                    tabIndex={0}
                    onPointerDown={(event) => {
                        if (event.button !== 0) return;
                        event.preventDefault();
                        event.currentTarget.setPointerCapture(event.pointerId);
                        drag.current = { x: event.clientX, width: displayedWidth };
                        setResizing(true);
                    }}
                    onPointerMove={(event) => {
                        if (drag.current)
                            resizeSidebar(drag.current.width + drag.current.x - event.clientX);
                    }}
                    onPointerUp={(event) => {
                        drag.current = null;
                        setResizing(false);
                        event.currentTarget.releasePointerCapture(event.pointerId);
                    }}
                    onLostPointerCapture={() => {
                        drag.current = null;
                        setResizing(false);
                    }}
                    onKeyDown={(event) => {
                        const step = event.shiftKey ? 50 : 10;
                        if (event.key === "ArrowLeft") resizeSidebar(displayedWidth + step);
                        else if (event.key === "ArrowRight") resizeSidebar(displayedWidth - step);
                        else if (event.key === "Home") resizeSidebar(300);
                        else if (event.key === "End") resizeSidebar(maxSidebarWidth);
                        else return;
                        event.preventDefault();
                    }}
                />
                <aside className="schedule-sidebar" aria-label="Schedule events">
                    <CalendarSources sources={sources} onState={onState} onError={onError} />
                    <H2 size="$5">Events</H2>
                    <ScheduleComposer
                        editing={null}
                        onDone={() => setEditing(null)}
                        onState={onState}
                        onError={onError}
                    />
                    <SeriesList
                        items={items}
                        onEdit={setEditing}
                        onState={onState}
                        onError={onError}
                    />
                </aside>
            </div>
            <Dialog
                modal
                open={editing !== null}
                onOpenChange={(open) => {
                    if (!open) setEditing(null);
                }}
            >
                <Dialog.Portal>
                    <Dialog.Overlay key="overlay" bg="rgba(0,0,0,0.65)" />
                    <Dialog.Content
                        key="content"
                        width="90vw"
                        maxW={640}
                        maxH="90vh"
                        p="$3"
                        style={{ overflowY: "auto" }}
                    >
                        <Dialog.Title fontSize={24}>Edit event</Dialog.Title>
                        <Dialog.Description>Update this scheduled event.</Dialog.Description>
                        {editing && (
                            <ScheduleComposer
                                key={editing.id}
                                editing={editing}
                                onDone={() => setEditing(null)}
                                onState={onState}
                                onError={onError}
                            />
                        )}
                    </Dialog.Content>
                </Dialog.Portal>
            </Dialog>
        </YStack>
    );
}

function MonthGrid({
    start,
    end,
    selectedMonth,
    occurrences,
    actuals,
    onDay,
}: {
    start: Date;
    end: Date;
    selectedMonth: number;
    occurrences: OccurrenceView[];
    actuals: ActualView[];
    onDay: (day: Date) => void;
}) {
    const days: Date[] = [];
    for (let day = start; day < end; day = addDays(day, 1)) days.push(day);
    return (
        <div style={{ overflowX: "auto" }}>
            <div className="month-grid">
                {WEEKDAYS.map((day) => (
                    <div className="month-heading" key={day}>
                        {WEEKDAY_LABELS[day]}
                    </div>
                ))}
                {days.map((day) => (
                    <div
                        key={isoDate(day)}
                        className="month-cell"
                        data-outside={day.getMonth() !== selectedMonth}
                    >
                        <button
                            className="month-date"
                            onClick={() => onDay(day)}
                            aria-label={`View ${isoDate(day)}`}
                            aria-current={isoDate(day) === isoDate(new Date()) ? "date" : undefined}
                        >
                            {day.getDate()}
                        </button>
                        {occurrences
                            .filter((event) => overlapsDay(event, day))
                            .toSorted(
                                (a, b) =>
                                    Number(b.all_day) - Number(a.all_day) ||
                                    a.start.localeCompare(b.start),
                            )
                            .map((event) => (
                                <EventHover
                                    key={`${event.item_id}-${event.occurrence_key}`}
                                    title={event.title}
                                    detail={event.all_day ? "All day" : clockRange(event)}
                                >
                                    <button
                                        className="month-event"
                                        onClick={() => onDay(day)}
                                        style={{ background: blockColor(event).fill }}
                                    >
                                        {event.all_day ? "" : `${clock(event.start)} `}
                                        {event.title}
                                    </button>
                                </EventHover>
                            ))}
                        {actuals.some((actual) => overlapsDay(actual, day)) && (
                            <button className="month-actual" onClick={() => onDay(day)}>
                                Actual time logged
                            </button>
                        )}
                    </div>
                ))}
            </div>
        </div>
    );
}

function WeekGrid({
    dayCount,
    weekStart,
    occurrences,
    actuals,
    onStart,
}: {
    dayCount: number;
    weekStart: Date;
    occurrences: OccurrenceView[];
    actuals: ActualView[];
    onStart: (occurrence: OccurrenceView) => void;
}) {
    const days = useMemo(
        () => Array.from({ length: dayCount }, (_, index) => addDays(weekStart, index)),
        [weekStart, dayCount],
    );
    const allDay = occurrences.filter((occurrence) => occurrence.all_day);
    const timed = occurrences.filter((occurrence) => !occurrence.all_day);
    const [currentTime, setClock] = useState(() => new Date());
    useEffect(() => {
        const update = () => setClock(new Date());
        const timer = setInterval(update, 15_000);
        document.addEventListener("visibilitychange", update);
        return () => {
            clearInterval(timer);
            document.removeEventListener("visibilitychange", update);
        };
    }, []);
    const today = startOfDay(currentTime).getTime();
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
    }, [firstMinute, occurrences.length]);

    useEffect(() => {
        const node = scroller.current;
        if (!(node instanceof HTMLElement)) return;
        const measure = () => setGutter(node.offsetWidth - node.clientWidth);
        const observer = new ResizeObserver(measure);
        observer.observe(node);
        measure();
        return () => observer.disconnect();
    }, []);

    return (
        // Horizontal scroll lives here rather than on the page: a seven-day grid
        // on a narrow window must not make the whole app scroll sideways.
        <YStack style={{ overflowX: "auto" }}>
            <YStack minW={dayCount === 1 ? 0 : 720}>
                <XStack pr={gutter}>
                    <YStack width={56} style={{ flexShrink: 0 }} />
                    {days.map((day) => (
                        <YStack
                            key={day.toISOString()}
                            flex={1}
                            flexBasis={0}
                            minW={0}
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
                        <YStack
                            width={56}
                            style={{ flexShrink: 0 }}
                            items="flex-end"
                            pr="$2"
                            py="$1"
                        >
                            <Text fontSize={11} color="#8b949e">
                                all day
                            </Text>
                        </YStack>
                        {days.map((day) => (
                            <YStack
                                key={day.toISOString()}
                                flex={1}
                                flexBasis={0}
                                minW={0}
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
                        maxHeight: "max(320px, calc(100dvh - 240px))",
                        overflowY: "auto",
                    }}
                >
                    <YStack width={56} style={{ flexShrink: 0 }}>
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
                            flexBasis={0}
                            minW={0}
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
                            {day.getTime() === today && (
                                <div
                                    className="calendar-now"
                                    aria-label={`Current time ${currentTime.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}`}
                                    style={{
                                        top:
                                            ((currentTime.getHours() * 60 +
                                                currentTime.getMinutes() +
                                                currentTime.getSeconds() / 60) /
                                                60) *
                                            HOUR_HEIGHT,
                                    }}
                                />
                            )}
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
/// Beside the plan rather than over it: the user must be able to
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
    const label = `${occurrence.title}, ${clockRange(occurrence)}${
        occurrence.source ? `, from ${occurrence.source}` : ""
    }${occurrence.cancelled ? ", cancelled" : ""}`;

    return (
        <EventHover
            title={occurrence.title}
            detail={`${clockRange(occurrence)}${occurrence.source ? ` · ${occurrence.source}` : ""}${occurrence.cancelled ? " · Cancelled" : ""}`}
        >
            <div
                style={{
                    position: "absolute",
                    display: "flex",
                    flexDirection: "column",
                    padding: "1px 4px",
                    cursor: "pointer",
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
                aria-label={label}
                role="button"
                tabIndex={0}
                onKeyDown={(event) => {
                    if (event.key === "Enter" || event.key === " ") {
                        event.preventDefault();
                        onStart(occurrence);
                    }
                }}
                onClick={() => onStart(occurrence)}
            >
                {/* One complete 13px line plus 1px padding above and below. */}
                {height >= 15 && (
                    <Text
                        fontSize={11}
                        lineHeight={13}
                        numberOfLines={1}
                        textDecorationLine={occurrence.cancelled ? "line-through" : "none"}
                    >
                        {occurrence.title}
                    </Text>
                )}
                {height >= 34 && (
                    <Text fontSize={10} lineHeight={12} color="#8b949e" numberOfLines={1}>
                        {occurrence.source
                            ? `${clockRange(occurrence)} · ${occurrence.source}`
                            : clockRange(occurrence)}
                    </Text>
                )}
            </div>
        </EventHover>
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
    // — on start and on stop — because every write is a retained object.
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
        <EventHover title={occurrence.title} detail="All day">
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
        </EventHover>
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
    const [pending, setPending] = useState<{
        source: CalendarSourceView;
        action: "sync" | "remove" | "raw";
    } | null>(null);

    async function deleteRaw(source: CalendarSourceView) {
        if (!source.raw_import_file_id) return;
        setBusy(source.id);
        onError(null);
        try {
            const backend = await clipperBackend();
            await backend.deleteFile(source.raw_import_file_id);
            onState(await backend.getState());
        } catch (caught) {
            onError(formatBackendError(caught));
        } finally {
            setBusy(null);
        }
    }

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
                report.tombstoned > 0 ? `${report.tombstoned} previous events removed` : null,
                report.skipped.length > 0 ? report.skipped.join("; ") : null,
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
                    <YStack gap="$2" minW={0} width="100%">
                        <Field label="Name">
                            <Input
                                value={name}
                                onChangeText={setName}
                                placeholder="Work"
                                width="100%"
                                minW={0}
                            />
                        </Field>
                        <Field label="iCalendar URL (secret address)">
                            <Input
                                value={url}
                                onChangeText={setUrl}
                                placeholder="https://calendar.google.com/calendar/ical/…/basic.ics"
                                width="100%"
                                minW={0}
                            />
                        </Field>
                    </YStack>
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

            {pending && (
                <YStack gap="$2" p="$3" bg="#242a31" rounded="$3" role="alert">
                    <Text fontWeight="600">{pending.source.name}</Text>
                    <Paragraph>
                        {pending.action === "raw"
                            ? "Permanently delete the original feed file? One-off events, supported recurring events, and recordings stay. Events with unsupported recurrence rules will be hidden with a warning because their original rule will no longer be available."
                            : pending.action === "remove"
                              ? "Remove this calendar and permanently delete its imported events and original feeds? Recordings and local overrides stay, but their old imported plans will be unavailable."
                              : "Replace this calendar with a new import? After a successful import, previous imported events and original feeds are permanently deleted. Recordings and local overrides stay, but their old imported plans will be unavailable. A failed import keeps the current calendar."}
                    </Paragraph>
                    <XStack gap="$2">
                        <Button
                            size="$2"
                            onPress={() => {
                                const choice = pending;
                                setPending(null);
                                if (choice.action === "sync") void sync(choice.source.id);
                                else if (choice.action === "remove") void remove(choice.source.id);
                                else void deleteRaw(choice.source);
                            }}
                        >
                            Confirm {pending.action === "sync" ? "replacement" : "deletion"}
                        </Button>
                        <Button size="$2" onPress={() => setPending(null)}>
                            Cancel
                        </Button>
                    </XStack>
                </YStack>
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
                            {source.raw_import_file_id && !source.raw_import_available
                                ? " · Original import unavailable"
                                : ""}
                        </Text>
                    </YStack>
                    <XStack gap="$2" flexWrap="wrap">
                        {source.raw_import_file_id && source.raw_import_available && (
                            <Button
                                size="$2"
                                disabled={busy !== null}
                                onPress={() => setPending({ source, action: "raw" })}
                            >
                                Delete original feed
                            </Button>
                        )}
                        <Button
                            size="$2"
                            icon={busy === source.id ? <Spinner /> : <RefreshCw size={14} />}
                            disabled={!canSync || busy !== null}
                            onPress={() => setPending({ source, action: "sync" })}
                        >
                            Sync
                        </Button>
                        <Button
                            size="$2"
                            icon={<Trash2 size={14} />}
                            disabled={busy !== null}
                            onPress={() => setPending({ source, action: "remove" })}
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
    const [order, setOrder] = useState("created");
    const [upcoming, setUpcoming] = useState<OccurrenceView[]>([]);
    const [sorting, setSorting] = useState(false);
    const [sortError, setSortError] = useState<string | null>(null);
    const [now, setNow] = useState(Date.now);

    useEffect(() => {
        if (order !== "next") return;
        setNow(Date.now());
        const timer = setInterval(() => setNow(Date.now()), 30_000);
        const foreground = () => {
            if (!document.hidden) setNow(Date.now());
        };
        document.addEventListener("visibilitychange", foreground);
        return () => {
            clearInterval(timer);
            document.removeEventListener("visibilitychange", foreground);
        };
    }, [order]);

    // Separate from the displayed calendar window: browsing last month must
    // not change what counts as this series' next occurrence.
    useEffect(() => {
        if (order !== "next") return;
        let cancelled = false;
        setSorting(true);
        setSortError(null);
        void (async () => {
            try {
                const from = new Date(now);
                const to = new Date(now + 366 * 24 * 60 * 60 * 1000);
                const backend = await clipperBackend();
                const expanded = await backend.expandSchedule(
                    from.toISOString(),
                    to.toISOString(),
                    observerZone(),
                );
                if (!cancelled) setUpcoming(expanded);
            } catch (error) {
                if (!cancelled) setSortError(formatBackendError(error));
            } finally {
                if (!cancelled) setSorting(false);
            }
        })();
        return () => {
            cancelled = true;
        };
    }, [order, items, now]);

    const starts = useMemo(() => {
        const seriesStarts = nextStarts(upcoming, now);
        const objectStarts = new Map<string, number>();
        for (const item of items) {
            const definition = parseDefinition(item.definition_json);
            const start = definition ? seriesStarts.get(definition.id) : undefined;
            if (start !== undefined) objectStarts.set(item.id, start);
        }
        return objectStarts;
    }, [upcoming, now, items]);
    const ordered = order === "next" && !sortError ? orderByNextStart(items, starts) : items;

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
            <YStack gap="$1">
                <Label htmlFor="event-order" fontSize={12} color="#8b949e">
                    Order events by
                </Label>
                <Select value={order} onValueChange={setOrder}>
                    <Select.Trigger
                        id="event-order"
                        aria-label="Order events by"
                        iconAfter={<ChevronDown size={16} />}
                    >
                        <Select.Value />
                    </Select.Trigger>
                    <Select.Content>
                        <Select.Viewport>
                            <Select.Group>
                                <Select.Label>Order events by</Select.Label>
                                {(
                                    [
                                        ["created", "Newest created"],
                                        ["next", "Next occurrence"],
                                    ] as const
                                ).map(([value, label], index) => (
                                    <Select.Item key={value} index={index} value={value}>
                                        <Select.ItemText>{label}</Select.ItemText>
                                        <Select.ItemIndicator marginLeft="auto">
                                            <Check size={16} />
                                        </Select.ItemIndicator>
                                    </Select.Item>
                                ))}
                            </Select.Group>
                        </Select.Viewport>
                    </Select.Content>
                </Select>
            </YStack>
            {order === "next" && (
                <Paragraph fontSize={12} color="#8b949e">
                    Upcoming starts within the next year. Refreshes every 30 seconds.
                    {sorting ? " Updating…" : ""}
                </Paragraph>
            )}
            {sortError && (
                <Paragraph color="#ff7b7b">
                    Could not order by next occurrence: {sortError}. Showing newest created.
                </Paragraph>
            )}
            {ordered.map((item) => (
                <Card
                    key={item.id}
                    className="event-card"
                    onClick={() => onEdit(item)}
                    bg="#171a1d"
                    p="$3"
                    style={{ borderColor: "#252b31", borderWidth: 1 }}
                >
                    <XStack items="flex-start" justify="space-between" gap="$3">
                        <YStack flex={1} minW={0} style={{ overflowWrap: "anywhere" }}>
                            <button
                                className="event-card-title"
                                onClick={(event) => {
                                    event.stopPropagation();
                                    onEdit(item);
                                }}
                                aria-label={`Edit ${item.title || "Untitled"}`}
                            >
                                {item.title || "Untitled"}
                            </button>
                            {order === "next" && !sortError && !sorting && (
                                <Text fontSize={12} color="#a9c8e6">
                                    {starts.has(item.id)
                                        ? `Next: ${new Date(starts.get(item.id)!).toLocaleString()}`
                                        : "No upcoming start found within the next year"}
                                </Text>
                            )}
                            <XStack items="center" gap="$2">
                                <Text fontSize={12} color="#8b949e">
                                    {item.recurrence} · {item.time_summary}
                                </Text>
                                {item.has_alarm && <AlarmClock size={12} color="#d0a33a" />}
                            </XStack>
                        </YStack>
                        <XStack gap="$2" style={{ flexShrink: 0 }}>
                            <Button
                                size="$2"
                                icon={deleting === item.id ? <Spinner /> : <Trash2 size={14} />}
                                disabled={deleting === item.id}
                                onPress={(event) => {
                                    event.stopPropagation();
                                    void remove(item.id);
                                }}
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
    const [repeat, setRepeat] = useState<RepeatSelection>("once");
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
    // the user had typed since opening it. Depending on the callbacks would be
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
                        : buildRecurrence(repeat, days, date, original.current?.recurrence ?? null),
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
                await backend.updateScheduleItem(editing.id, item, editing.revision);
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
                    <CalendarDatePicker
                        label="Choose event date"
                        value={new Date(`${date}T00:00:00`)}
                        onChange={(value) => {
                            setDate(isoDate(value));
                            setSpanChanged(true);
                        }}
                    >
                        {date}
                    </CalendarDatePicker>
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
                {(
                    [
                        // Only ever shown as the current state, never as an
                        // offer: reaching it means the stored rule is richer
                        // than this row, and the guard below makes pressing it
                        // inert.
                        ...(repeat === "custom" ? (["custom"] as const) : []),
                        "once",
                        "daily",
                        "weekdays",
                        "weekly",
                        "monthly",
                    ] as RepeatSelection[]
                ).map((choice) => (
                    <Toggle
                        key={choice}
                        on={repeat === choice}
                        onPress={() => {
                            // Pressing the pill that is already lit is not a
                            // change, and must not arm one. Without this, a
                            // series whose cadence this row cannot express
                            // would be flattened by a press that looks inert.
                            if (choice === repeat) return;
                            setRepeat(choice);
                            setRecurrenceChanged(true);
                        }}
                    >
                        {repeatLabel(choice)}
                    </Toggle>
                ))}
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
/// the user's block on save.
function parseDefinition(json: string): ScheduleItem | null {
    try {
        const parsed = JSON.parse(json) as ScheduleItem;
        return parsed.span && parsed.recurrence ? parsed : null;
    } catch {
        return null;
    }
}

/// Round to the grid. A block the user typed is snapped; one that arrived from a
/// calendar is not — a meeting that runs 09:07–09:23 is ordinary.
function snapMinutes(minutes: number): number {
    return Math.max(SLOT_MINUTES, Math.round(minutes / SLOT_MINUTES) * SLOT_MINUTES);
}

/// Monday-based label for a date. `Date.getDay()` is Sunday-based, so the shift
/// is what makes Monday index 0.
function weekdayLabel(date: Date): string {
    const day = WEEKDAYS[(date.getDay() + 6) % 7];
    return day ? WEEKDAY_LABELS[day] : "";
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
    if (weekStart.getFullYear() !== end.getFullYear()) {
        return `${month} ${weekStart.getDate()}, ${weekStart.getFullYear()} – ${endMonth} ${end.getDate()}, ${end.getFullYear()}`;
    }
    return sameMonth
        ? `${month} ${weekStart.getDate()}–${end.getDate()}, ${end.getFullYear()}`
        : `${month} ${weekStart.getDate()} – ${endMonth} ${end.getDate()}, ${end.getFullYear()}`;
}
