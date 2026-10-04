export const palette = {
  pageFill: "#101418",
  cardFill: "#1b222a",
  buttonFill: "#3c4c5b",
  buttonHover: "#405061",
  buttonPressed: "#354453",
  inputFill: "#101418",
  selectedFill: "#253d55",
  selectedBorder: "#8bc5ff",
  accentFill: "#8bc5ff",
  onAccent: "#101418",
  text: "#f4f7fb",
  secondary: "#c4ced9",
  accent: "#8bc5ff",
  danger: "#ff9a9a",
  warning: "#f7cf72",
  success: "#8de0ad",
} as const;

export const calendarBorder = "#91a3b5";

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

function blend(from: string, to: string, amount: number): `#${string}` {
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
  background: palette.pageFill,
  backgroundHover: palette.cardFill,
  backgroundPress: palette.cardFill,
  backgroundFocus: palette.pageFill,
  backgroundActive: palette.selectedFill,
  borderColor: "transparent",
  borderColorHover: "transparent",
  borderColorPress: "transparent",
  borderColorFocus: "transparent",
  borderColorActive: palette.selectedBorder,
  color: palette.text,
  colorHover: palette.text,
  colorPress: palette.text,
  colorFocus: palette.text,
  colorActive: palette.text,
  placeholderColor: palette.secondary,
  outlineColor: palette.selectedBorder,
  accentBackground: palette.accentFill,
  accentColor: palette.onAccent,
  color1: palette.pageFill,
  color2: palette.cardFill,
  color3: palette.buttonFill,
  color4: palette.buttonHover,
  color5: palette.buttonPressed,
  color6: palette.secondary,
  color7: palette.buttonFill,
  color8: palette.selectedBorder,
  color9: palette.accentFill,
  color10: palette.accentFill,
  color11: palette.secondary,
  color12: palette.text,
};

function buttonTheme(fill: `#${string}`, color: `#${string}`) {
  return {
    ...darkTheme,
    background: fill,
    backgroundHover: blend(fill, palette.text, 0.08),
    backgroundPress: blend(fill, palette.pageFill, 0.1),
    backgroundFocus: fill,
    color,
    colorHover: color,
    colorPress: color,
    colorFocus: color,
    color11: color,
    color12: color,
  };
}

export const buttonThemes = {
  default: {
    ...buttonTheme(palette.buttonFill, palette.text),
    backgroundHover: palette.buttonHover,
    backgroundPress: palette.buttonPressed,
  },
  accent: buttonTheme(palette.accentFill, palette.onAccent),
  danger: buttonTheme(palette.danger, palette.onAccent),
  warning: buttonTheme(palette.warning, palette.onAccent),
  success: buttonTheme(palette.success, palette.onAccent),
};

function buttonStyle(theme: ReturnType<typeof buttonTheme>) {
  return {
    bg: theme.background,
    color: theme.color,
    borderWidth: 0,
    borderColor: "transparent",
    hoverStyle: { bg: theme.backgroundHover },
    pressStyle: { bg: theme.backgroundPress },
    focusVisibleStyle: {
      outlineColor: palette.selectedBorder,
      outlineWidth: 2,
      outlineStyle: "solid",
    },
  } as const;
}

export const buttonStyles = {
  ...buttonStyle(buttonThemes.default),
  variants: {
    tone: {
      accent: buttonStyle(buttonThemes.accent),
      danger: buttonStyle(buttonThemes.danger),
      warning: buttonStyle(buttonThemes.warning),
      success: buttonStyle(buttonThemes.success),
    },
    selected: {
      true: {
        bg: palette.selectedFill,
        color: palette.text,
        borderWidth: 1,
        borderColor: palette.selectedBorder,
        hoverStyle: { bg: palette.selectedFill },
        pressStyle: { bg: palette.selectedFill },
      },
    },
  },
} as const;

export const inputStyles = {
  bg: palette.inputFill,
  color: palette.text,
  borderWidth: 0,
  hoverStyle: { bg: palette.inputFill, borderWidth: 0 },
  focusStyle: { bg: palette.inputFill, borderWidth: 0 },
  focusVisibleStyle: {
    outlineColor: palette.selectedBorder,
    outlineWidth: 2,
    outlineStyle: "solid",
  },
} as const;

export const darkDefaults = {
  Button: { borderWidth: 0 },
  Card: { borderWidth: 0 },
  Input: { borderWidth: 0 },
  TextArea: { borderWidth: 0 },
} as const;

export function createDarkThemes<T extends Record<string, Record<string, string>>>(themes: T): T {
  return Object.fromEntries(
    Object.entries(themes).map(([name, theme]) => {
      if (name !== "dark" && !name.startsWith("dark_")) return [name, theme];
      let colors: { [K in keyof typeof darkTheme]: string } = { ...darkTheme };
      if (name.endsWith("_Button")) {
        colors = { ...buttonThemes.default };
      } else if (name.endsWith("_Card") || name.includes("Tooltip") || name.endsWith("_ListItem")) {
        colors.background = palette.cardFill;
      } else if (name.endsWith("_Input") || name.endsWith("_TextArea")) {
        colors.background = palette.inputFill;
        colors.backgroundHover = palette.inputFill;
        colors.backgroundPress = palette.inputFill;
        colors.backgroundFocus = palette.inputFill;
      } else if (
        name.endsWith("_SwitchThumb") ||
        name.endsWith("_SliderThumb") ||
        name.endsWith("_ProgressIndicator") ||
        name.endsWith("_SliderTrackActive")
      ) {
        colors = { ...buttonThemes.accent };
      }
      return [name, { ...theme, ...colors }];
    }),
  ) as T;
}
