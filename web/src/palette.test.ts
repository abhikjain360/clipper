import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { test } from "node:test";
import {
    palette,
    buttonThemes,
    buttonStyles,
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

function contrast(name: string, foreground: string, background: string, minimum: number) {
    assert.match(foreground, /^#[a-f0-9]{6}$/i);
    assert.match(background, /^#[a-f0-9]{6}$/i);
    const a = luminance(foreground);
    const b = luminance(background);
    const measured = (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
    assert.ok(measured >= minimum, `${name}: ${measured.toFixed(2)}:1, requires ${minimum}:1`);
}

const surfaces = [palette.pageFill, palette.cardFill];

test("body and secondary text meet 4.5:1 on every neutral and selected fill", () => {
    for (const background of [
        ...surfaces,
        palette.inputFill,
        palette.buttonFill,
        palette.buttonHover,
        palette.buttonPressed,
        palette.selectedFill,
        ...Object.values(statusSurfaces),
    ]) {
        contrast("body", palette.text, background, 4.5);
        contrast("secondary", palette.secondary, background, 4.5);
    }
    for (const background of surfaces) {
        for (const color of [palette.accent, palette.danger, palette.warning, palette.success]) {
            contrast("status text", color, background, 4.5);
        }
    }
});

test("fill hierarchy separates buttons, cards and fields without normal borders", () => {
    assert.ok(luminance(palette.pageFill) < luminance(palette.cardFill));
    assert.ok(luminance(palette.inputFill) < luminance(palette.cardFill));
    assert.ok(luminance(palette.buttonHover) > luminance(palette.buttonFill));
    assert.ok(luminance(palette.buttonPressed) < luminance(palette.buttonFill));
    assert.equal(darkDefaults.Button.borderWidth, 0);
    assert.equal(darkDefaults.Card.borderWidth, 0);
    assert.equal(darkDefaults.Input.borderWidth, 0);
    assert.equal(buttonStyles.borderWidth, 0);
    for (const fill of [palette.buttonFill, palette.buttonHover, palette.buttonPressed]) {
        contrast("button against card", fill, palette.cardFill, 1.6);
    }
    for (const adjacent of [...surfaces, palette.selectedFill, palette.buttonHover]) {
        contrast("selected border", palette.selectedBorder, adjacent, 3);
    }
    for (const [name, theme] of Object.entries(buttonThemes)) {
        for (const state of ["", "Hover", "Press", "Focus"] as const) {
            contrast(
                `${name} ${state} text`,
                theme[`color${state}`],
                theme[`background${state}`],
                4.5,
            );
            assert.equal(theme[`borderColor${state}`], "transparent");
        }
    }
});

test("both apps' stock component subthemes use readable text and borderless fields", () => {
    const require = createRequire(import.meta.url);
    const { defaultConfig } = require("@tamagui/config/v4") as {
        defaultConfig: { themes: Record<string, Record<string, string>> };
    };
    for (const [name, theme] of Object.entries(createDarkThemes(defaultConfig.themes))) {
        if (name !== "dark" && !name.startsWith("dark_")) continue;
        for (const state of ["", "Hover", "Press", "Focus"] as const) {
            contrast(
                `${name} ${state}`,
                theme[`color${state}`]!,
                theme[`background${state}`]!,
                4.5,
            );
            assert.equal(theme[`borderColor${state}`], "transparent");
        }
        if (name.endsWith("_Input") || name.endsWith("_TextArea")) {
            assert.equal(theme.background, palette.inputFill);
            contrast(`${name} placeholder`, theme.placeholderColor!, theme.background!, 4.5);
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
    for (const color of cursorColors) contrast("remote cursor label", palette.onAccent, color, 4.5);
    for (const background of surfaces) contrast("editor keyword", cursorColors[4], background, 4.5);
});
