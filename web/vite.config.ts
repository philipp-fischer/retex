import { defineConfig } from "vite";
export default defineConfig({
  base: "./",
  publicDir: false,
  build: { outDir: "../dist/ui", emptyOutDir: true },
  server: {
    proxy: process.env.RETEX_RESOLVER
      ? { "/texlive": process.env.RETEX_RESOLVER }
      : undefined,
  },
});
