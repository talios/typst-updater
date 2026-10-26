// @ts-check
import { defineConfig } from "astro/config";

// Served from GitHub Pages as a project site: https://talios.github.io/typst-updater/
export default defineConfig({
  site: "https://talios.github.io",
  base: "/typst-updater",
  // Compression drops whitespace between inline tags, which breaks the <pre> terminal output
  compressHTML: false,
  markdown: {
    shikiConfig: {
      themes: { light: "github-light", dark: "github-dark" },
    },
  },
});
