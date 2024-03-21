import {defineConfig} from "vite";

export default defineConfig({
    server: {
        port: 8080,
        strictPort: true,
        headers: {
            "Cross-Origin-Opener-Policy": "same-origin",
            "Cross-Origin-Embedder-Policy": "require-corp",
        },
        cors: false
    }
});
