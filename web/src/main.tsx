import "@tamagui/core/reset.css";
import "./styles.css";

import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { TamaguiProvider } from "tamagui";
import App from "./App";
import tamaguiConfig from "./tamagui.config";

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
