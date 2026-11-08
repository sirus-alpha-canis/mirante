import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

afterEach(() => cleanup());
import { clearDiffCache } from "./components/useDiff";
import { setLang } from "./lib/i18n";

// Tests read English, whatever the machine's language; no diff outlives its test.
afterEach(() => {
  setLang("en");
  clearDiffCache();
});
