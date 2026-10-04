import "@tamagui/core/reset.css";
import "./styles.css";

import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { TamaguiProvider } from "tamagui";
import App from "./App";
import tamaguiConfig from "./tamagui.config";
import { palette, buttonThemes } from "@clipper/shared";

const colors = {
    ...palette,
    button: buttonThemes.default.background,
    buttonHover: buttonThemes.default.backgroundHover,
    buttonPress: buttonThemes.default.backgroundPress,
    accentHover: buttonThemes.accent.backgroundHover,
    accentPress: buttonThemes.accent.backgroundPress,
};
for (const [name, color] of Object.entries(colors)) {
    document.documentElement.style.setProperty(`--clipper-${name}`, color);
}

const root = document.getElementById("root");

if (!root) {
    throw new Error("missing root element");
}

createRoot(root).render(
    <StrictMode>
        <TamaguiProvider config={tamaguiConfig} defaultTheme="dark">
            <App />
        </TamaguiProvider>
    </StrictMode>,
);
