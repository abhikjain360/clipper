import assert from "node:assert/strict";
import { test } from "node:test";
import { Window } from "happy-dom";
import { cardEvents } from "./card-events.ts";
import { clipboardImage, clipboardBytes } from "../../packages/shared/src/clipboard.ts";

test("cards open from content and Enter while their actions stay separate", () => {
    const browser = new Window();
    const card = browser.document.createElement("div");
    card.setAttribute("role", "button");
    card.tabIndex = 0;
    const content = browser.document.createElement("span");
    card.append(content);
    let opens = 0;
    let actions = 0;
    const events = cardEvents(() => opens++);
    card.addEventListener("click", events.onClick);
    card.addEventListener("keydown", events.onKeyDown);
    browser.document.body.append(card);
    content.click();
    card.dispatchEvent(new browser.KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    assert.equal(opens, 2);
    for (const name of ["Copy", "Download", "Delete"]) {
        const button = browser.document.createElement("button");
        button.textContent = name;
        const icon = browser.document.createElement("span");
        button.append(icon);
        button.addEventListener("click", () => actions++);
        card.append(button);
        icon.click();
        button.dispatchEvent(new browser.KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
        assert.equal(opens, 2);
        button.disabled = true;
        icon.dispatchEvent(new browser.MouseEvent("click", { bubbles: true }));
        assert.equal(opens, 2);
    }
    assert.equal(actions, 6);
    browser.happyDOM.abort();
});

test("image and binary viewers keep the full bytes", () => {
    for (const bytes of [
        new Uint8Array([]),
        new Uint8Array([0]),
        new Uint8Array([0, 255]),
        new Uint8Array([0, 255, 128, 12]),
    ]) {
        const url = clipboardImage("image/png", bytes)!;
        assert.deepEqual(new Uint8Array(Buffer.from(url.split(",")[1]!, "base64")), bytes);
    }
    assert.equal(clipboardImage("application/octet-stream", new Uint8Array([0])), null);
    assert.equal(clipboardBytes(new Uint8Array([0, 255, 128])), "00 ff 80");
});
