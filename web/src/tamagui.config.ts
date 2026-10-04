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

function libronFont(font: typeof defaultConfig.fonts.body) {
    return createFont({
        ...font,
        family: "Libron, Georgia, serif",
        weight: Object.fromEntries(
            Object.entries(font.weight).map(([size, weight]) => [
                size,
                Number(weight) >= 600 ? "700" : "400",
            ]),
        ),
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
            family: "var(--clipper-monospace)",
            weight: Object.fromEntries(
                Object.entries(defaultConfig.fonts.body.weight).map(([size]) => [size, "400"]),
            ),
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
