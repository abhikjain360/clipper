import { defaultConfig } from "@tamagui/config/v4";
import { Button as TamaguiButton, createTamagui, styled } from "tamagui";
import { createDarkThemes, darkDefaults } from "@clipper/shared";

const tamaguiConfig = createTamagui({
  ...defaultConfig,
  themes: createDarkThemes(defaultConfig.themes),
  defaultProps: darkDefaults,
});

export default tamaguiConfig;
export const Button = Object.assign(styled(TamaguiButton, darkDefaults.Button), {
  Text: TamaguiButton.Text,
  Icon: TamaguiButton.Icon,
});

export type AppTamaguiConfig = typeof tamaguiConfig;

declare module "tamagui" {
  interface TamaguiCustomConfig extends AppTamaguiConfig {}
}
