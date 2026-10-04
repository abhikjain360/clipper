export const palette = {
  page: "#101418",
  surface: "#1b222a",
  raised: "#303c49",
  border: "#91a3b5",
  text: "#f4f7fb",
  secondary: "#c4ced9",
  accent: "#8bc5ff",
  danger: "#ff9a9a",
  warning: "#f7cf72",
  success: "#8de0ad",
} as const;

export const statusSurfaces = {
  warning: "#352b16",
  success: "#223328",
} as const;

export const scheduleColors = {
  cancelled: { fill: "#2a2226", accent: "#b08f8f" },
  overridden: { fill: "#3d3320", accent: "#d0a33a" },
  imported: { fill: "#1d3330", accent: "#4dbfa5" },
  planned: { fill: "#1f3350", accent: "#4d8fd6" },
  allDay: { fill: "#243a2c", accent: "#7bd88f" },
} as const;

export const fatigueColors = {
  recovered: "#4ade80",
  low: "#a3e635",
  moderate: "#facc15",
  high: "#fb923c",
  veryHigh: palette.danger,
  warmUp: "#f0a35e",
} as const;

export const cursorColors = [
  "#30bced",
  "#6eeb83",
  "#ffbc42",
  "#ee6352",
  "#c4a5ff",
  "#f15bb5",
  "#00bbf9",
  "#fee440",
] as const;

function blend(from: string, to: string, amount: number): string {
  return `#${[1, 3, 5]
    .map((offset) => {
      const start = Number.parseInt(from.slice(offset, offset + 2), 16);
      const end = Number.parseInt(to.slice(offset, offset + 2), 16);
      return Math.round(start + (end - start) * amount)
        .toString(16)
        .padStart(2, "0");
    })
    .join("")}`;
}

const darkTheme = {
  background: palette.page,
  backgroundHover: palette.surface,
  backgroundPress: palette.raised,
  backgroundFocus: palette.surface,
  backgroundActive: palette.accent,
  borderColor: palette.border,
  borderColorHover: palette.accent,
  borderColorPress: palette.border,
  borderColorFocus: palette.accent,
  borderColorActive: palette.accent,
  color: palette.text,
  colorHover: palette.text,
  colorPress: palette.text,
  colorFocus: palette.text,
  colorActive: palette.page,
  placeholderColor: palette.secondary,
  outlineColor: palette.accent,
  accentBackground: palette.accent,
  accentColor: palette.page,
  color1: palette.page,
  color2: palette.surface,
  color3: palette.raised,
  color4: palette.raised,
  color5: palette.raised,
  color6: palette.secondary,
  color7: palette.border,
  color8: palette.border,
  color9: palette.accent,
  color10: palette.accent,
  color11: palette.secondary,
  color12: palette.text,
};

function buttonTheme(fill: string, color: string, border: string) {
  return {
    ...darkTheme,
    background: fill,
    backgroundHover: blend(fill, palette.text, 0.08),
    backgroundPress: blend(fill, palette.page, 0.2),
    backgroundFocus: blend(fill, palette.text, 0.08),
    borderColor: border,
    borderColorHover: border,
    borderColorPress: border,
    borderColorFocus: palette.accent,
    borderColorActive: border,
    color,
    colorHover: color,
    colorPress: color,
    colorFocus: color,
    color11: color,
    color12: color,
  };
}

export const buttonThemes = {
  default: buttonTheme(blend(palette.raised, palette.page, 0.2), palette.text, palette.border),
  accent: buttonTheme(palette.accent, palette.page, palette.accent),
  danger: buttonTheme(palette.danger, palette.page, palette.danger),
  warning: buttonTheme(palette.warning, palette.page, palette.warning),
  success: buttonTheme(palette.success, palette.page, palette.success),
};

export const darkDefaults = {
  Button: { borderWidth: 1, borderColor: "$borderColor" },
  Card: { borderWidth: 1, borderColor: "$borderColor" },
} as const;

export function createDarkThemes<T extends Record<string, Record<string, string>>>(themes: T): T {
  return Object.fromEntries(
    Object.entries(themes).map(([name, theme]) => {
      if (name !== "dark" && !name.startsWith("dark_")) return [name, theme];
      const parts = name.split("_");
      const kind = parts.includes("red")
        ? "danger"
        : parts.includes("yellow")
          ? "warning"
          : parts.includes("green")
            ? "success"
            : parts.includes("blue") || parts.includes("accent")
              ? "accent"
              : "default";
      let colors: { [K in keyof typeof darkTheme]: string } = { ...darkTheme };
      if (name.endsWith("_Button") || parts.at(-1) === "accent") {
        colors = { ...buttonThemes[kind] };
      } else if (name.endsWith("_Card") || name.includes("Tooltip") || name.endsWith("_ListItem")) {
        colors.background = palette.surface;
      } else if (name.endsWith("_SwitchThumb") || name.endsWith("_SliderThumb")) {
        colors = { ...buttonThemes.accent };
      } else if (name.endsWith("_ProgressIndicator") || name.endsWith("_SliderTrackActive")) {
        colors = { ...buttonThemes.accent };
      }
      return [name, { ...theme, ...colors }];
    }),
  ) as T;
}
