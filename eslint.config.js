// Flat ESLint config: JS + typescript-eslint recommended, with Prettier's
// config last so formatting is Prettier's job and linting stays semantic.
// (no-undef is off for TS files — the type-checker owns undefined names.)
import js from "@eslint/js";
import tseslint from "typescript-eslint";
import prettier from "eslint-config-prettier";

export default tseslint.config(
  { ignores: ["dist/", "src-tauri/", "node_modules/"] },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  prettier
);
