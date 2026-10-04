import assert from "node:assert/strict";
import { after, before, test } from "node:test";
import { act, createElement, useCallback } from "react";
import { createRoot, type Root } from "react-dom/client";
import { Window } from "happy-dom";
import { useGymChange, useGymData } from "./gym-actions.ts";

const browser = new Window();
let root: Root;
let actions: ReturnType<typeof useGymChange>;
let data: ReturnType<typeof useGymData<number>>;
let next = 0;
let load: () => Promise<number> = async () => next;
const errors: (string | null)[] = [];
const noop = () => {};
const onError = (message: string | null) => {
    errors.push(message);
};

function Screen() {
    const read = useCallback(() => load(), []);
    data = useGymData(read, null, onError);
    actions = useGymChange(data.reload, onError);
    return createElement(
        "button",
        { disabled: actions.busy, "aria-busy": actions.busy },
        String(data.value),
    );
}

before(async () => {
    Object.assign(globalThis, {
        window: browser,
        document: browser.document,
        IS_REACT_ACT_ENVIRONMENT: true,
    });
    const container = browser.document.createElement("div");
    browser.document.body.append(container);
    root = createRoot(container as unknown as HTMLElement);
    await act(async () => {
        root.render(createElement(Screen));
    });
});
after(() => {
    act(() => root.unmount());
    browser.happyDOM.abort();
});

test("a repeated press is blocked until the updated screen loads", async () => {
    let calls = 0;
    let releaseWrite: () => void = noop;
    let releaseRead: (value: number) => void = noop;
    const write = () => {
        calls += 1;
        return new Promise<void>((resolve) => {
            releaseWrite = resolve;
        });
    };
    load = () =>
        new Promise<number>((resolve) => {
            releaseRead = resolve;
        });
    let result: Promise<boolean>;
    act(() => {
        result = actions.run(write);
    });
    assert.equal(browser.document.querySelector("button")?.disabled, true);
    assert.equal(await actions.run(write), false);
    await act(async () => {
        releaseWrite();
    });
    assert.equal(actions.busy, true);
    assert.equal(await actions.run(write), false);
    await act(async () => {
        releaseRead(1);
        assert.equal(await result, true);
    });
    assert.equal(calls, 1);
    assert.equal(data.value, 1);
    assert.equal(actions.busy, false);
});

test("a failed action shows its error and releases the buttons", async () => {
    load = async () => next;
    await act(async () => {
        assert.equal(
            await actions.run(async () => {
                throw new Error("Weight must be positive");
            }),
            false,
        );
    });
    assert.equal(errors.at(-1), "Weight must be positive");
    assert.equal(actions.busy, false);
});

test("an older read cannot replace a newer screen", async () => {
    let release: (value: number) => void = noop;
    load = () =>
        new Promise<number>((resolve) => {
            release = resolve;
        });
    const older = data.reload();
    next = 3;
    load = async () => next;
    await act(async () => {
        await data.reload();
        release(2);
        await older;
    });
    assert.equal(data.value, 3);
});
