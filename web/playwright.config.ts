import { defineConfig, devices } from "@playwright/test";

// api-server.exe links the debug CRT dynamically; make sure the DLLs are on PATH
// even when this shell was never initialized by a Visual Studio developer prompt.
const VC143_CRT =
  "C:\\Program Files (x86)\\Microsoft Visual Studio\\2022\\BuildTools\\VC\\Redist\\MSVC\\14.44.35112\\x64\\Microsoft.VC143.CRT";

/**
 * Default run drives the Vite dev server. Set E2E_BASE_URL (and have the
 * production stack up, see docker-compose.yml) to test the built bundle served
 * by nginx instead — in that mode the terminal specs run against the
 * containerised API on :10000.
 */
const baseURL = process.env.E2E_BASE_URL ?? "http://localhost:5173";
const production = Boolean(process.env.E2E_BASE_URL);

export default defineConfig({
  testDir: "./e2e",
  timeout: 30_000,
  reporter: [["list"]],
  use: {
    baseURL,
    ...devices["Desktop Chrome"],
  },
  webServer: [
    {
      command: "..\\target\\debug\\api-server.exe",
      url: "http://127.0.0.1:8080/healthz",
      reuseExistingServer: true,
      timeout: 30_000,
      env: { PATH: `${VC143_CRT};${process.env.PATH ?? ""}` },
    },
    {
      command: "python -m uvicorn app.main:app --app-dir ..\\restapis --port 8000",
      url: "http://127.0.0.1:8000/health",
      reuseExistingServer: true,
      timeout: 60_000,
      env: { DATA_SOURCE: "synthetic", ALLOWED_ORIGINS: "http://localhost:5173" },
    },
    production
      ? {
          command: "npm run preview -- --port 4173 --strictPort",
          url: "http://localhost:4173",
          reuseExistingServer: true,
          timeout: 60_000,
        }
      : {
          command: "npm run dev",
          url: "http://localhost:5173",
          reuseExistingServer: true,
          timeout: 60_000,
        },
  ],
});
