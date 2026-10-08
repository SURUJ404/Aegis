import { expect, type APIRequestContext, type Page } from "@playwright/test";

/**
 * The quant API the front end talks to (started by playwright.config.ts). In a
 * production run (`E2E_BASE_URL=http://localhost:4173`) point this at the same
 * container the browser uses so parity checks compare like with like.
 */
export const API = process.env.E2E_API_URL ?? "http://127.0.0.1:8000";

/**
 * The two formatters below are written independently of `src/format.ts` on
 * purpose: the tests re-derive what the screen must say from the raw API
 * payload, so a bug in the production formatter cannot hide itself.
 */
export function pct(v: number, digits = 2): string {
  const n = v * 100;
  if (!Number.isFinite(n)) return "—";
  const sign = n > 0 ? "+" : n < 0 ? "-" : "";
  return `${sign}${Math.abs(n).toLocaleString("en-US", {
    minimumFractionDigits: digits,
    maximumFractionDigits: digits,
  })}%`;
}

export function pctUnsigned(v: number, digits = 2): string {
  const n = v * 100;
  if (!Number.isFinite(n)) return "—";
  return `${Math.abs(n).toLocaleString("en-US", {
    minimumFractionDigits: digits,
    maximumFractionDigits: digits,
  })}%`;
}

export function num(v: number, digits = 2): string {
  if (!Number.isFinite(v)) return "—";
  return v.toLocaleString("en-US", {
    minimumFractionDigits: digits,
    maximumFractionDigits: digits,
  });
}

export async function apiJson<T>(
  request: APIRequestContext,
  path: string,
  init?: Parameters<APIRequestContext["post"]>[1],
): Promise<T> {
  const res = path.startsWith("/backtest")
    ? await request.post(`${API}${path}`, init)
    : await request.get(`${API}${path}`);
  expect(res.ok(), `${path} -> ${res.status()}`).toBeTruthy();
  return (await res.json()) as T;
}

/** The backtest panel once it is showing live API data. */
export async function waitResults(page: Page) {
  const results = page.locator('[data-testid="results"]');
  await expect(results).toHaveAttribute("data-source", "api", { timeout: 20_000 });
  return results;
}

/**
 * Collect every JS page error and every browser console error. Network-level
 * console noise (`Failed to load resource`) is only ignored when the test
 * itself breaks the network on purpose; JavaScript errors are never ignored.
 */
export function watchErrors(page: Page, ignoreNetwork = false): string[] {
  const errors: string[] = [];
  const push = (text: string) => {
    if (ignoreNetwork && /Failed to load resource|net::ERR_/i.test(text)) return;
    errors.push(text);
  };
  page.on("pageerror", (e) => push(`pageerror: ${e.message}`));
  page.on("console", (m) => {
    if (m.type() === "error") push(`console: ${m.text()}`);
  });
  return errors;
}

/** Snapshot of every rendered metric cell, keyed by `data-metric`. */
export async function metricCells(page: Page): Promise<Record<string, string>> {
  return page.locator("[data-metric]").evaluateAll((els) =>
    Object.fromEntries(
      els.map((e) => [e.getAttribute("data-metric") ?? "", e.textContent ?? ""]),
    ),
  );
}

/** Text of a numeric column of the screener, parsed back to numbers. */
export async function columnNumbers(page: Page, col: string): Promise<number[]> {
  const texts = await page
    .locator(`[data-testid="screener"] td[data-col="${col}"]`)
    .allTextContents();
  return texts.map((t) => Number(t.replace(/[^0-9.-]/g, "")));
}

/** Every visible string in the results panel; used for the NaN sweep. */
export async function resultsText(page: Page): Promise<string> {
  return page.locator('[data-testid="results"]').innerText();
}

export function expectNoBrokenNumbers(text: string): void {
  expect(text, "no NaN/undefined/Infinity may reach the screen").not.toMatch(
    /NaN|undefined|Infinity/i,
  );
}
