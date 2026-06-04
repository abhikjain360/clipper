import react from "@vitejs/plugin-react";
import { tamaguiPlugin } from "@tamagui/vite-plugin";
import { defineConfig } from "vite";

export default defineConfig({
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
