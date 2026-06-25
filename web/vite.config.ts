import react from "@vitejs/plugin-react";
import { tamaguiPlugin } from "@tamagui/vite-plugin";
import { defineConfig } from "vite";

export default defineConfig({
    define: {
        // Build-time server URL, injected from the build environment so hosted
        // bundles point at the deployed API. Dev builds leave VITE_SERVER_URL
        // unset → null → the app falls back to the wasm default (localhost).
        "import.meta.env.VITE_SERVER_URL": JSON.stringify(
            process.env.VITE_SERVER_URL ?? null,
        ),
    },
    plugins: [
        react(),
        tamaguiPlugin({
            components: ["tamagui"],
            config: "./src/tamagui.config.ts",
        }),
    ],
    server: {
        host: "127.0.0.1",
        port: 53880,
        strictPort: false,
    },
    preview: {
        host: "127.0.0.1",
        port: 53880,
        strictPort: false,
    },
});
