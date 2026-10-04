import { defaultConfig } from "@tamagui/config/v4";
import {
  Button as TamaguiButton,
  Input as TamaguiInput,
  TextArea as TamaguiTextArea,
  createFont,
  createTamagui,
  styled,
} from "tamagui";
import { createDarkThemes, darkDefaults, buttonStyles, inputStyles } from "@clipper/shared";

export const monospaceFont = "Fira Code";

function libronFont(font: typeof defaultConfig.fonts.body) {
  return createFont({
    ...font,
    family: "Libron",
    weight: Object.fromEntries(
      Object.entries(font.weight).map(([size, weight]) => [
        size,
        Number(weight) >= 600 ? "700" : "400",
      ]),
    ),
    face: {
      400: { normal: "Libron-Regular", italic: "Libron-Italic" },
      500: { normal: "Libron-Regular", italic: "Libron-Italic" },
      600: { normal: "Libron-Bold", italic: "Libron-BoldItalic" },
      700: { normal: "Libron-Bold", italic: "Libron-BoldItalic" },
    },
  });
}

const tamaguiConfig = createTamagui({
  ...defaultConfig,
  fonts: {
    ...defaultConfig.fonts,
    body: libronFont(defaultConfig.fonts.body),
    heading: libronFont(defaultConfig.fonts.heading),
    mono: createFont({
      ...defaultConfig.fonts.body,
      family: monospaceFont,
      weight: Object.fromEntries(
        Object.entries(defaultConfig.fonts.body.weight).map(([size]) => [size, "400"]),
      ),
      face: {
        400: { normal: "FiraCode-Regular" },
        500: { normal: "FiraCode-Medium" },
        600: { normal: "FiraCode-SemiBold" },
        700: { normal: "FiraCode-Bold" },
      },
    }),
  },
  themes: createDarkThemes(defaultConfig.themes),
  defaultProps: darkDefaults,
});

export default tamaguiConfig;
export const Button = Object.assign(styled(TamaguiButton, buttonStyles), {
  Text: TamaguiButton.Text,
  Icon: TamaguiButton.Icon,
});

export const Input = styled(TamaguiInput, inputStyles);
export const TextArea = styled(TamaguiTextArea, inputStyles);

export type AppTamaguiConfig = typeof tamaguiConfig;

declare module "tamagui" {
  interface TamaguiCustomConfig extends AppTamaguiConfig {}
}
