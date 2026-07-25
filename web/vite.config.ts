import { defineConfig } from "vite";
import solid from "vite-plugin-solid";
import tailwindcss from "@tailwindcss/vite";

// Static, dependency-light roadmap site. Base is "./" so it can be served from
// any sub-path (e.g. GitHub Pages) without rewriting asset URLs.
export default defineConfig({
  base: "./",
  plugins: [solid(), tailwindcss()],
});
