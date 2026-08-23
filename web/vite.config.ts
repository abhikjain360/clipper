import react from "@vitejs/plugin-react";
import { tamaguiPlugin } from "@tamagui/vite-plugin";
import { createRequire } from "node:module";
import path from "node:path";
import { defineConfig } from "vite";

const require = createRequire(import.meta.url);

// The bundle must contain exactly one React. pnpm's hidden hoist dir points
// react-native-web at the mobile-context variant (react 19.2.3), and Tamagui's
// bare `react-native-web` imports (e.g. views/Anchor) resolve through it,
// bypassing the vite plugin's react-native alias — the build then ships two
// React instances and crashes at render ("Cannot read properties of null
// (reading 'useContext')"). Pin both specifiers to this app's own
// react-native-web copy, whose peer context matches our react 19.2.8.
const reactNativeWebDir = path.dirname(require.resolve("react-native-web/package.json"));

export default defineConfig({
    define: {
        // Build-time server URL, injected from the build environment so hosted
        // bundles point at the deployed API. Dev builds leave VITE_SERVER_URL
        // unset → null → the app falls back to the wasm default (localhost).
        "import.meta.env.VITE_SERVER_URL": JSON.stringify(process.env.VITE_SERVER_URL ?? null),
    },
    resolve: {
        alias: [
            { find: /^react-native$/, replacement: reactNativeWebDir },
            { find: /^react-native-web$/, replacement: reactNativeWebDir },
        ],
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
