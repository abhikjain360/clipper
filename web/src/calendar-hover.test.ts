import assert from "node:assert/strict";
import { after, before, test } from "node:test";
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { Window } from "happy-dom";
import { CalendarHoverContext, useCalendarHover, useEventHover } from "./calendar-hover.ts";

const browser = new Window();
const handlers = new Map<string, ReturnType<typeof useEventHover>>();
let root: Root;
let refresh: unknown = {};
let clicks = 0;
let close: () => void;

function Block({ name }: { name: string }) {
    const hover = useEventHover();
    handlers.set(name, hover);
    return createElement(
        "button",
        {
            id: name,
            onPointerEnter: hover.onPointerEnter,
            onPointerLeave: hover.onPointerLeave,
            onPointerCancel: hover.onPointerCancel,
            onClick: () => clicks++,
        },
        name,
        hover.open && createElement("span", { role: "tooltip" }, name),
    );
}

function Calendar({ names }: { names: string[] }) {
    const hover = useCalendarHover(refresh);
    close = hover.close;
    return createElement(
        CalendarHoverContext.Provider,
        { value: hover },
        names.map((name) => createElement(Block, { key: name, name })),
    );
}

function render(names = ["Sleep", "Work", "Sleep tomorrow"]) {
    act(() => root.render(createElement(Calendar, { names })));
}

function enter(name: string) {
    const block = browser.document.getElementById(name)!;
    act(() =>
        block.dispatchEvent(
            new browser.PointerEvent("pointerover", { bubbles: true, pointerType: "mouse" }),
        ),
    );
}

function cards() {
    return Array.from(
        browser.document.querySelectorAll('[role="tooltip"]'),
        (card) => card.textContent,
    );
}

before(() => {
    Object.assign(globalThis, {
        window: browser,
        document: browser.document,
        IS_REACT_ACT_ENVIRONMENT: true,
    });
    const container = browser.document.createElement("div");
    browser.document.body.append(container);
    root = createRoot(container as unknown as HTMLElement);
    render();
});

after(() => {
    act(() => root.unmount());
    browser.happyDOM.abort();
});

test("only the most recently entered block has a card, including repeated occurrences", () => {
    for (const name of ["Sleep", "Work", "Sleep tomorrow", "Sleep"]) {
        enter(name);
        assert.deepEqual(cards(), [name]);
    }
    act(() => handlers.get("Work")!.onPointerLeave());
    assert.deepEqual(cards(), ["Sleep"]);
});

test("pointer leave closes the card and clicking still runs the block action", () => {
    enter("Work");
    const block = browser.document.getElementById("Work")!;
    act(() => block.dispatchEvent(new browser.MouseEvent("click", { bubbles: true })));
    assert.equal(clicks, 1);
    act(() =>
        block.dispatchEvent(
            new browser.PointerEvent("pointerout", {
                bubbles: true,
                pointerType: "mouse",
                relatedTarget: browser.document.body,
            }),
        ),
    );
    assert.deepEqual(cards(), []);
    act(() => handlers.get("Work")!.onOpenChange(true));
    assert.deepEqual(cards(), []);
});

test("refresh closes the card while the block remains mounted", () => {
    enter("Sleep");
    const block = browser.document.getElementById("Sleep");
    refresh = {};
    render();
    assert.equal(browser.document.getElementById("Sleep"), block);
    assert.deepEqual(cards(), []);
    enter("Work");
    act(() => close());
    assert.deepEqual(cards(), []);
});

test("a card closes when its block is removed and stays closed after remount", () => {
    enter("Sleep");
    render(["Work"]);
    assert.deepEqual(cards(), []);
    render();
    assert.deepEqual(cards(), []);
});

test("inner calendar scrolling and window blur close the card", () => {
    enter("Work");
    act(() => browser.document.getElementById("Work")!.dispatchEvent(new browser.Event("scroll")));
    assert.deepEqual(cards(), []);
    enter("Sleep");
    act(() => browser.dispatchEvent(new browser.Event("blur")));
    assert.deepEqual(cards(), []);
});

test("hiding the document closes the card", () => {
    enter("Work");
    Object.defineProperty(browser.document, "hidden", { configurable: true, value: true });
    act(() => browser.document.dispatchEvent(new browser.Event("visibilitychange")));
    assert.deepEqual(cards(), []);
    Object.defineProperty(browser.document, "hidden", { configurable: true, value: false });
});

test("pointer cancellation closes the card and touch does not open one", () => {
    enter("Work");
    act(() =>
        browser.document
            .getElementById("Work")!
            .dispatchEvent(new browser.PointerEvent("pointercancel", { bubbles: true })),
    );
    assert.deepEqual(cards(), []);
    act(() =>
        browser.document
            .getElementById("Sleep")!
            .dispatchEvent(
                new browser.PointerEvent("pointerover", { bubbles: true, pointerType: "touch" }),
            ),
    );
    assert.deepEqual(cards(), []);
});
