import { afterEach } from "vitest";
import "@testing-library/jest-dom/vitest";

declare global {
  // eslint-disable-next-line no-var
  var __TAURI__: Record<string, unknown> | undefined;
  // eslint-disable-next-line no-var
  var __TAURI_IPC__: unknown;
}

if (typeof window !== "undefined") {
  if (typeof window.__TAURI__ === "undefined") {
    window.__TAURI__ = {};
  }
}

afterEach(() => {
  if (typeof window !== "undefined") {
    delete window.__TAURI__;
    delete window.__TAURI_IPC__;
  }
});

