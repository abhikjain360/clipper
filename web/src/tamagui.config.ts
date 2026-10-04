import { defaultConfig } from "@tamagui/config/v4";
import {
    Button as TamaguiButton,
    Input as TamaguiInput,
    TextArea as TamaguiTextArea,
    createTamagui,
    styled,
} from "tamagui";
import { createDarkThemes, darkDefaults, buttonStyles, inputStyles } from "@clipper/shared";

const tamaguiConfig = createTamagui({
    ...defaultConfig,
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
