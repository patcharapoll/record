/** @type {import('tailwindcss').Config} */
export default {
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  theme: {
    extend: {
      // Same palette as xspector-duck so the apps look like one family.
      colors: {
        surface: { 0: "#0f1117", 1: "#181b22", 2: "#1e2230", 3: "#252a3a" },
        accent: { DEFAULT: "#4f8ef7", hover: "#3b7de3" },
        border: "#2d3347",
        muted: "#6b7280",
      },
      fontFamily: {
        sans: ["Inter", "system-ui", "sans-serif"],
        mono: ["JetBrains Mono", "Menlo", "monospace"],
      },
    },
  },
  plugins: [],
};
