import assert from "node:assert/strict";
import { test } from "node:test";
import { createPressGuard } from "../../packages/shared/src/press.ts";

test("a quick double tap stays one action even when the local write has already finished", () => {
    const press = createPressGuard();
    assert.equal(press(0), true);
    assert.equal(press(100), false);
    assert.equal(press(399), false);
    assert.equal(press(400), true);
});
