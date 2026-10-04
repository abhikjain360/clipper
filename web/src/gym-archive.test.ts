import assert from "node:assert/strict";
import { test } from "node:test";
import { activeGymItems } from "../../packages/shared/src/gym.ts";

test("start choices and exercise pickers exclude archived items without removing library data", () => {
    const old = { id: "old", name: "Older workout" };
    const active = { id: "active", name: "Upper", archived: false };
    const archived = { id: "archived", name: "Bench", archived: true };
    const library: { id: string; name: string; archived?: boolean }[] = [old, active, archived];
    assert.deepEqual(activeGymItems(library), [old, active]);
    assert.deepEqual(library, [old, active, archived]);
    archived.archived = false;
    assert.deepEqual(activeGymItems(library), library);
});
