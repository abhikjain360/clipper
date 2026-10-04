import assert from "node:assert/strict";
import { after, before, test } from "node:test";
import { act, createElement, type ComponentType } from "react";
import { createRoot, type Root } from "react-dom/client";
import { Window } from "happy-dom";
import { build } from "vite";
import type { ActualView, AppState, OccurrenceView, ScheduleItemView } from "@clipper/shared";

const browser = new Window();
const day = new Date();
day.setHours(9, 0, 0, 0);
const series = "series-id";
const item: ScheduleItemView = {
    id: "object-id",
    revision: 1,
    title: "Work",
    recurrence: "Once",
    time_summary: "09:00",
    all_day: false,
    has_alarm: false,
    created_at: day.toISOString(),
    definition_json: JSON.stringify({
        id: series,
        title: "Work",
        span: {
            kind: "timed",
            start: { kind: "floating", at: "2026-10-09T09:00:00" },
            duration: 60,
        },
        recurrence: { kind: "once" },
    }),
};
const occurrence: OccurrenceView = {
    item_id: series,
    occurrence_key: "first",
    plan_context: "plan",
    title: "Work",
    start: day.toISOString(),
    end: new Date(day.getTime() + 3_600_000).toISOString(),
    all_day: false,
    overridden: false,
    source: null,
    cancelled: false,
};
let starts = 0;
let expanded = [occurrence];
let root: Root;
let Panel: ComponentType<Record<string, unknown>>;
const errors: unknown[] = [];
const backend = {
    expandSchedule: async () => expanded,
    actualsBetween: async () => [],
    queryAppData: async () => [],
    startActual: async () => starts++,
    getState: async () => ({}),
};
const notifications = {
    desktop: false,
    allowed: false as boolean | null,
    failed: false,
    checks: 0,
    settings: 0,
    async invoke(command: string) {
        if (command === "notification_permission") {
            this.checks++;
            if (this.failed) throw new Error("Missing permission response");
            return this.allowed;
        }
        assert.equal(command, "open_notification_settings");
        this.settings++;
    },
};

const ui = `
import { Children, createElement as h, cloneElement, createContext, useContext } from "react";
export function Box({ children, ...props }) {
    const dom = Object.fromEntries(Object.entries(props).filter(([key]) =>
        key.startsWith("aria-") || ["onClick", "onKeyDown", "onPointerEnter", "onPointerLeave", "onPointerCancel", "id", "role", "tabIndex", "ref", "htmlFor"].includes(key)));
    return h("div", dom, ...Children.toArray(children));
}
export const Card = Box, H2 = Box, Label = Box, Paragraph = Box, Spinner = Box,
    Text = Box, Switch = Box, XStack = Box, YStack = Box;
export function Button({ children, onPress, onClick, disabled, ...props }) {
    return h("button", { onClick: onClick ?? onPress, disabled, "aria-label": props["aria-label"] }, children);
}
export function Input({ onChangeText, ...props }) {
    return h("input", { id: props.id, value: props.value, onChange: event => onChangeText?.(event.target.value) });
}
export function Dialog({ open, children }) { return open ? h("section", { role: "dialog" }, children) : null; }
for (const key of ["Portal", "Overlay", "Content", "Title", "Description"]) Dialog[key] = Box;
export const Select = Object.assign(Box, Object.fromEntries(
    ["Trigger", "Value", "Content", "Viewport", "Group", "Label", "Item", "ItemText", "ItemIndicator"].map(key => [key, Box])));
const Group = createContext(null);
export function ToggleGroup({ onValueChange, children }) { return h(Group.Provider, { value: onValueChange }, children); }
ToggleGroup.Item = function Item({ value, children }) {
    const change = useContext(Group);
    return cloneElement(children, { onClick: () => change(value) });
};
`;

before(async () => {
    Object.assign(globalThis, {
        window: browser,
        document: browser.document,
        HTMLElement: browser.HTMLElement,
        ResizeObserver: browser.ResizeObserver,
        localStorage: browser.localStorage,
        IS_REACT_ACT_ENVIRONMENT: true,
    });
    Reflect.set(globalThis, Symbol.for("schedule-editor-backend"), backend);
    Reflect.set(globalThis, Symbol.for("schedule-notifications"), notifications);
    const mocks: Record<string, string> = {
        tamagui: ui,
        "./tamagui.config": ui,
        "./backend": `export const clipperBackend = async () => globalThis[Symbol.for("schedule-editor-backend")];
            export const formatBackendError = String; export const isTauriRuntime = () => globalThis[Symbol.for("schedule-notifications")].desktop;`,
        "@tauri-apps/api/core": `export const invoke = (command) => globalThis[Symbol.for("schedule-notifications")].invoke(command);`,
        "./CalendarDatePicker": `export { Box as CalendarDatePicker } from "tamagui";`,
        "./kitchen/ScheduleRecipes": `export const ScheduleRecipes = () => null;`,
        "lucide-react": `export { Box as AlarmClock, Box as CalendarClock, Box as ChevronLeft,
            Box as ChevronRight, Box as Check, Box as ChevronDown, Box as Play, Box as Plus,
            Box as RefreshCw, Box as Square, Box as Trash2 } from "tamagui";`,
        "./EventHover": `
            import { createElement as h } from "react";
            import { useEventHover } from ${JSON.stringify(new URL("./calendar-hover.ts", import.meta.url).pathname)};
            export function EventHover({ children, title }) {
                const hover = useEventHover();
                return h("div", { onPointerEnter: hover.onPointerEnter, onPointerLeave: hover.onPointerLeave },
                    children, hover.open && h("span", { role: "tooltip" }, title));
            }`,
    };
    const result = await build({
        configFile: false,
        logLevel: "silent",
        plugins: [
            {
                name: "schedule-editor-test",
                enforce: "pre",
                resolveId: (id) => (id in mocks ? `\0${id}` : undefined),
                load: (id) => mocks[id.slice(1)],
            },
        ],
        build: {
            ssr: new URL("./SchedulePanel.tsx", import.meta.url).pathname,
            write: false,
            rollupOptions: { external: ["react", "react/jsx-runtime"] },
        },
    });
    assert.ok(!Array.isArray(result) && "output" in result);
    const chunk = result.output.find((entry) => entry.type === "chunk");
    assert.ok(chunk?.type === "chunk");
    const code = chunk.code.replace(
        /from "(react(?:\/jsx-runtime)?)"/g,
        (_, id: string) => `from ${JSON.stringify(import.meta.resolve(id))}`,
    );
    Panel = (await import(`data:text/javascript;base64,${Buffer.from(code).toString("base64")}`))
        .SchedulePanel;
    const container = browser.document.createElement("div");
    browser.document.body.append(container);
    root = createRoot(container as unknown as HTMLElement);
});

after(() => {
    act(() => root?.unmount());
    Reflect.deleteProperty(globalThis, Symbol.for("schedule-editor-backend"));
    Reflect.deleteProperty(globalThis, Symbol.for("schedule-notifications"));
    browser.happyDOM.abort();
});

async function render(events: OccurrenceView[], view: string, running: ActualView | null = null) {
    act(() => root.render(null));
    starts = 0;
    errors.length = 0;
    expanded = events;
    browser.localStorage.setItem("clipper.schedule.mode::", "calendar");
    await act(async () =>
        root.render(
            createElement(Panel, {
                state: {} as AppState,
                items: [item],
                warnings: [],
                sources: [],
                running,
                onState: () => {},
                onError: (error: unknown) => {
                    if (error) errors.push(error);
                },
            }),
        ),
    );
    const tab = Array.from(browser.document.querySelectorAll("button")).find(
        (button) => button.textContent === view,
    )!;
    await act(async () => tab.click());
}

function block(prefix: string) {
    const node = browser.document.querySelector(`[aria-label^="${prefix}"]`);
    assert.ok(node);
    return node;
}

for (const view of ["Day", "Week", "Month"]) {
    for (const all_day of [false, true]) {
        test(`${view} ${all_day ? "all-day" : "timed"} block opens its series editor without starting a timer`, async () => {
            await render([{ ...occurrence, all_day }], view);
            const node = block("Edit Work,");
            act(() =>
                node.dispatchEvent(
                    new browser.PointerEvent("pointerover", {
                        bubbles: true,
                        pointerType: "mouse",
                    }),
                ),
            );
            assert.equal(browser.document.querySelectorAll('[role="tooltip"]').length, 1);
            await act(async () =>
                node.dispatchEvent(new browser.MouseEvent("click", { bubbles: true })),
            );
            const dialog = browser.document.querySelector('[role="dialog"]');
            assert.ok(dialog);
            assert.ok(dialog.textContent?.includes("Edit event"));
            assert.ok(
                Array.from(dialog.querySelectorAll("input")).some(
                    (input) => input.value === "Work",
                ),
            );
            assert.equal(browser.document.querySelectorAll('[role="tooltip"]').length, 0);
            act(() =>
                node.dispatchEvent(
                    new browser.PointerEvent("pointerover", {
                        bubbles: true,
                        pointerType: "mouse",
                    }),
                ),
            );
            assert.equal(browser.document.querySelectorAll('[role="tooltip"]').length, 0);
            assert.equal(
                browser.document.body.textContent?.includes("Start untracked time"),
                false,
            );
            assert.equal(starts, 0);
            assert.deepEqual(errors, []);
        });

        test(`${view} ${all_day ? "all-day" : "timed"} imported meeting does not open an editor or start a timer`, async () => {
            await render([{ ...occurrence, all_day, source: "Calendar" }], view);
            const node = block("Imported meeting: Work,");
            act(() =>
                node.dispatchEvent(
                    new browser.PointerEvent("pointerover", {
                        bubbles: true,
                        pointerType: "mouse",
                    }),
                ),
            );
            await act(async () =>
                node.dispatchEvent(new browser.MouseEvent("click", { bubbles: true })),
            );
            await act(async () =>
                node.dispatchEvent(
                    new browser.KeyboardEvent("keydown", { key: "Enter", bubbles: true }),
                ),
            );
            assert.equal(browser.document.querySelector('[role="dialog"]'), null);
            assert.equal(browser.document.querySelectorAll('[role="tooltip"]').length, 1);
            assert.equal(starts, 0);
            assert.deepEqual(errors, []);
        });
    }
}

test("Enter opens a focused calendar block's editor without starting a timer", async () => {
    await render([occurrence], "Day");
    const node = block("Edit Work,");
    assert.ok(node instanceof browser.HTMLElement);
    node.focus();
    await act(async () =>
        node.dispatchEvent(new browser.KeyboardEvent("keydown", { key: "Enter", bubbles: true })),
    );
    assert.ok(browser.document.querySelector('[role="dialog"]'));
    assert.equal(starts, 0);
});

test("Next disables only the running occurrence and shows its elapsed time", async () => {
    const now = Date.now();
    const event = {
        ...occurrence,
        start: new Date(now - 60_000).toISOString(),
        end: new Date(now + 3_600_000).toISOString(),
    };
    const running: ActualView = {
        id: "timer-id",
        item_id: event.item_id,
        occurrence_key: event.occurrence_key,
        title: "Timer title",
        start: new Date(now - 120_000).toISOString(),
        end: "",
        running: true,
    };
    await render(
        [
            event,
            {
                ...event,
                occurrence_key: "second",
                title: "Next repeat",
                start: new Date(now + 600_000).toISOString(),
            },
            { ...event, item_id: "other", title: "Other series" },
        ],
        "Next",
        running,
    );
    const buttons = Array.from(browser.document.querySelectorAll("button"));
    const active = buttons.find((button) => button.textContent?.startsWith("Running ·"));
    assert.ok(active?.disabled);
    assert.match(active.textContent!, /^Running · 2:0\d$/);
    assert.equal(active.parentElement?.querySelectorAll("button")[0], active);
    const other = buttons.filter((button) => button.textContent === "Start");
    assert.equal(other.length, 2);
    assert.ok(other.every((button) => !button.disabled));
    await act(async () => active.click());
    assert.equal(starts, 0);
    await act(async () => other[0]!.click());
    assert.equal(starts, 1);
    assert.deepEqual(errors, []);
    await render([{ ...event, cancelled: true }], "Next", running);
    const cancelled = Array.from(browser.document.querySelectorAll("button"));
    assert.ok(cancelled.find((button) => button.textContent?.startsWith("Running ·"))?.disabled);
    assert.equal(
        cancelled.some((button) => button.textContent === "Start"),
        false,
    );
});

const reminderNote = "Break reminders are off: allow notifications for Clipper in System Settings";
const reminderTimer: ActualView = {
    id: "reminder-timer",
    item_id: series,
    occurrence_key: "first",
    title: "Work",
    start: new Date(Date.now() - 120_000).toISOString(),
    end: "",
    running: true,
    break_reminders: true,
};

test("a denied break timer shows settings and clears the note after permission is granted", async () => {
    notifications.desktop = true;
    notifications.allowed = false;
    notifications.settings = 0;
    await render([occurrence], "Next", reminderTimer);
    assert.ok(browser.document.body.textContent?.includes(reminderNote));
    const settings = Array.from(browser.document.querySelectorAll("button")).find(
        (button) => button.textContent === "Open notification settings",
    );
    assert.ok(settings);
    await act(async () => settings.click());
    assert.equal(notifications.settings, 1);
    notifications.allowed = true;
    await act(async () => browser.dispatchEvent(new browser.Event("focus")));
    assert.equal(browser.document.body.textContent?.includes(reminderNote), false);
    assert.equal(browser.document.body.textContent?.includes("Open notification settings"), false);
    assert.deepEqual(errors, []);
});

test("missing permission recovers on the minute check without restarting the timer", async (t) => {
    act(() => root.render(null));
    t.mock.timers.enable({ apis: ["setInterval"] });
    notifications.desktop = true;
    notifications.failed = true;
    notifications.checks = 0;
    await render([occurrence], "Next", reminderTimer);
    assert.ok(browser.document.body.textContent?.includes(reminderNote));
    assert.equal(notifications.checks, 1);
    notifications.failed = false;
    notifications.allowed = true;
    await act(async () => t.mock.timers.tick(60_000));
    assert.equal(notifications.checks, 2);
    assert.equal(browser.document.body.textContent?.includes(reminderNote), false);
    notifications.allowed = false;
    await act(async () => t.mock.timers.tick(60_000));
    assert.ok(browser.document.body.textContent?.includes(reminderNote));
    act(() => root.render(null));
});

test("ordinary timers and browser timers do not show the notification note", async () => {
    notifications.desktop = true;
    notifications.allowed = false;
    await render([occurrence], "Next", { ...reminderTimer, break_reminders: false });
    assert.equal(browser.document.body.textContent?.includes(reminderNote), false);
    notifications.desktop = false;
    await render([occurrence], "Next", reminderTimer);
    assert.equal(browser.document.body.textContent?.includes(reminderNote), false);
});
