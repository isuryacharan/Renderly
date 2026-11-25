import js from "@eslint/js";
import tseslint from "typescript-eslint";
import prettier from "eslint-config-prettier";
import globals from "globals";

export default tseslint.config(
  { ignores: ["**/dist/**", "**/coverage/**", "docs/design-system/preview/**"] },
  js.configs.recommended,
  ...tseslint.configs.strict,
  {
    files: ["**/*.{js,mjs}"],
    languageOptions: { globals: globals.node },
  },
  {
    // The benchmark and storage runners send functions into the page; they run with browser globals.
    files: [
      "scripts/bench.mjs",
      "scripts/storage-e2e.mjs",
      "scripts/editor-e2e.mjs",
      "scripts/media-e2e.mjs",
    ],
    languageOptions: { globals: { ...globals.node, ...globals.browser } },
  },
  {
    // Tests index into arrays they just filled; a failed assertion is the right error there.
    files: ["**/*.test.ts"],
    rules: { "@typescript-eslint/no-non-null-assertion": "off" },
  },
  prettier,
);
