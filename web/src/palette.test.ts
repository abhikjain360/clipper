import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { test } from "node:test";
import {
    palette,
    buttonThemes,
    createDarkThemes,
    darkDefaults,
    scheduleColors,
    statusSurfaces,
    fatigueColors,
    cursorColors,
} from "../../packages/shared/src/palette.ts";

function luminance(hex: string): number {
    const channels = [1, 3, 5].map((offset) => {
        const value = Number.parseInt(hex.slice(offset, offset + 2), 16) / 255;
        return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
    });
    return channels[0]! * 0.2126 + channels[1]! * 0.7152 + channels[2]! * 0.0722;
}

function ratio(first: string, second: string): number {
    const a = luminance(first);
    const b = luminance(second);
    return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
}

function contrast(name: string, foreground: string, background: string, minimum: number) {
    assert.match(foreground, /^#[a-f0-9]{6}$/i);
    assert.match(background, /^#[a-f0-9]{6}$/i);
    const measured = ratio(foreground, background);
    assert.ok(measured >= minimum, `${name}: ${measured.toFixed(2)}:1, requires ${minimum}:1`);
}

const surfaces = [palette.page, palette.surface, palette.raised];

test("body, secondary and status text meet 4.5:1 on every surface", () => {
    for (const background of [...surfaces, ...Object.values(statusSurfaces)]) {
        for (const name of [
            "text",
            "secondary",
            "accent",
            "danger",
            "warning",
            "success",
        ] as const) {
            contrast(name, palette[name], background, 4.5);
        }
        contrast("card and input edges", palette.border, background, 3);
    }
});

test("button text, boundaries and selected states meet contrast targets", () => {
    assert.equal(darkDefaults.Button.borderWidth, 1);
    assert.equal(darkDefaults.Button.borderColor, "$borderColor");
    for (const [name, theme] of Object.entries(buttonThemes)) {
        for (const state of ["", "Hover", "Press", "Focus"] as const) {
            const background = theme[`background${state}`];
            contrast(`${name} ${state} text`, theme[`color${state}`], background, 4.5);
            for (const adjacent of surfaces) {
                contrast(`${name} ${state} boundary`, theme[`borderColor${state}`], adjacent, 3);
                assert.notEqual(background, adjacent);
                if (name !== "default") contrast(`${name} selected fill`, background, adjacent, 3);
            }
            if (name === "default") {
                contrast("button inner edge", theme[`borderColor${state}`], background, 3);
                contrast("button secondary text", palette.secondary, background, 4.5);
                for (const color of [
                    palette.accent,
                    palette.danger,
                    palette.warning,
                    palette.success,
                ]) {
                    contrast("button status icon", color, background, 4.5);
                }
            }
        }
    }
});

test("both apps' stock component subthemes receive the accessible dark colours", () => {
    const require = createRequire(import.meta.url);
    const { defaultConfig } = require("@tamagui/config/v4") as {
        defaultConfig: { themes: Record<string, Record<string, string>> };
    };
    const themes = createDarkThemes(defaultConfig.themes);
    for (const [name, theme] of Object.entries(themes)) {
        if (name !== "dark" && !name.startsWith("dark_")) continue;
        for (const state of ["", "Hover", "Press", "Focus"] as const) {
            contrast(
                `${name} ${state}`,
                theme[`color${state}`]!,
                theme[`background${state}`]!,
                4.5,
            );
            for (const adjacent of surfaces) {
                contrast(`${name} ${state} edge`, theme[`borderColor${state}`]!, adjacent, 3);
            }
        }
        if (name.endsWith("_Input") || name.endsWith("_TextArea")) {
            contrast(`${name} placeholder`, theme.placeholderColor!, theme.background!, 4.5);
        }
        if (name.endsWith("_Switch")) {
            for (const adjacent of surfaces)
                contrast(`${name} selected track`, theme.backgroundActive!, adjacent, 3);
            contrast(`${name} selected thumb`, palette.page, theme.backgroundActive!, 3);
        }
    }
});

test("calendar kinds, fatigue bands and editor cursor labels remain readable", () => {
    for (const [name, colors] of Object.entries(scheduleColors)) {
        contrast(`${name} title`, palette.text, colors.fill, 4.5);
        contrast(`${name} detail`, palette.secondary, colors.fill, 4.5);
        contrast(`${name} recipe link`, palette.accent, colors.fill, 4.5);
        contrast(`${name} edge`, colors.accent, colors.fill, 3);
        for (const adjacent of surfaces) contrast(`${name} outer edge`, colors.accent, adjacent, 3);
    }
    for (const color of Object.values(fatigueColors)) {
        for (const background of surfaces) contrast("fatigue text", color, background, 4.5);
    }
    for (const color of cursorColors) contrast("remote cursor label", palette.page, color, 4.5);
    for (const background of surfaces) contrast("editor keyword", cursorColors[4], background, 4.5);
});
