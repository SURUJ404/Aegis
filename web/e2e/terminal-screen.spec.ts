import { expect, test } from "@playwright/test";
import type { ScreenRow } from "../src/quantApi";
import {
  apiJson,
  columnNumbers,
  expectNoBrokenNumbers,
  num,
  pct,
  waitResults,
} from "./terminal-support";

async function openScreener(page: import("@playwright/test").Page) {
  await page.getByRole("tab", { name: "Screener" }).click();
  await expect(page.getByTestId("screener")).toBeVisible({ timeout: 15_000 });
}

test.describe("screener and market", () => {
  test("the screener shows every field of every /screen row", async ({ page, request }) => {
    const rows = await apiJson<ScreenRow[]>(request, "/screen?strategy=ma&a=20&b=60");
    await page.goto("/");
    await openScreener(page);

    await expect(page.locator(".screener-panel .trow").first()).toContainText(
      `${rows.length} rows`,
    );
    await expect(page.locator(".screener-panel .trow").first()).toContainText(
      "ma · a 20 · b 60",
    );
    await expect(
      page.locator('[data-testid="screener"] thead th, [data-testid="screener"] thead button'),
    ).toContainText(["Symbol", "Price", "1M return", "RSI 14", "SMA 50", "Signal"]);

    await expect(page.locator('[data-testid="screener"] tbody tr')).toHaveCount(rows.length);
    for (const row of rows) {
      const tr = page.locator(`[data-testid="screener"] tbody tr[data-symbol="${row.symbol}"]`);
      await expect(tr).toBeVisible();
      await expect(tr.locator('[data-col="symbol"]')).toHaveText(row.symbol);
      await expect(tr.locator('[data-col="price"]')).toHaveText(num(row.price as number));
      await expect(tr.locator('[data-col="ret_1m"]')).toHaveText(pct(row.ret_1m as number));
      await expect(tr.locator('[data-col="rsi14"]')).toHaveText(num(row.rsi14 as number));
      // The API returns the 50-day average close itself, so it is a price.
      await expect(tr.locator('[data-col="vs_sma50"]')).toHaveText(num(row.vs_sma50 as number));
      await expect(tr.locator('[data-col="signal"]')).toHaveText(row.signal);
      await expect(tr.locator('[data-col="signal"] .pill')).toHaveClass(
        new RegExp(`sig-${row.signal}`),
      );
    }

    // Row click affordances the design promises.
    const first = page.locator('[data-testid="screener"] tbody tr').first();
    await expect(first).toHaveAttribute("tabindex", "0");
    await expect(first).toHaveAttribute("aria-selected", "false");
    expectNoBrokenNumbers(await page.locator(".screener-panel").innerText());
  });

  test("every column sorts both ways with aria-sort tracking the state", async ({ page }) => {
    await page.goto("/");
    await openScreener(page);

    const symbolTh = page.locator('th:has(button[data-col="symbol"])');
    await expect(symbolTh).toHaveAttribute("aria-sort", "ascending");

    const priceTh = page.locator('th:has(button[data-col="price"])');
    await expect(priceTh).toHaveAttribute("aria-sort", "none");
    await priceTh.locator("button").click();
    await expect(priceTh).toHaveAttribute("aria-sort", "ascending");
    await expect(symbolTh).toHaveAttribute("aria-sort", "none");
    const ascending = await columnNumbers(page, "price");
    expect(ascending).toEqual([...ascending].sort((x, y) => x - y));

    await priceTh.locator("button").click();
    await expect(priceTh).toHaveAttribute("aria-sort", "descending");
    const descending = await columnNumbers(page, "price");
    expect(descending).toEqual([...ascending].reverse());

    // Third click returns the header to the untouched state? No: the design
    // toggles forever, so only the direction may change.
    await priceTh.locator("button").click();
    await expect(priceTh).toHaveAttribute("aria-sort", "ascending");
    expect(await columnNumbers(page, "price")).toEqual(ascending);
  });

  test("clicking a row loads that ticker into the backtest", async ({ page }) => {
    await page.goto("/");
    await openScreener(page);

    const pending = page.waitForRequest(
      (r) =>
        r.method() === "POST" &&
        r.url().includes("/backtest") &&
        (r.postData() ?? "").includes("TSLA"),
    );
    await page.locator('[data-testid="screener"] tbody tr[data-symbol="TSLA"]').click();
    await pending;

    await expect(page.getByRole("tab", { name: "Backtest" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    await expect(page.getByTestId("ticker")).toHaveValue("TSLA");
    await expect(page.locator("header .pill.num")).toHaveText("TSLA");
    await expect(page.locator('[data-testid="results"] .trow').first()).toContainText("TSLA");
    await waitResults(page);
  });

  test("a failing screener shows an error and Retry recovers", async ({ page }) => {
    let down = true;
    await page.route("**/screen*", (route) =>
      down ? route.fulfill({ status: 500, body: "boom" }) : route.continue(),
    );

    await page.goto("/");
    await page.getByRole("tab", { name: "Screener" }).click();
    await expect(page.getByTestId("screen-error")).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId("screener")).toHaveCount(0);

    down = false;
    await page.getByTestId("screen-retry").click();
    await expect(page.getByTestId("screener")).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId("screen-error")).toHaveCount(0);
  });

  test("null and non-finite indicator values render as em dashes and sort last", async ({
    page,
  }) => {
    const rows: ScreenRow[] = [
      { symbol: "AAA", price: 10, ret_1m: null, rsi14: null, vs_sma50: null, signal: "flat" },
      { symbol: "BBB", price: 20, ret_1m: 0.05, rsi14: 55.5, vs_sma50: 19.5, signal: "long" },
      { symbol: "CCC", price: 30, ret_1m: "nan" as unknown as number, rsi14: 42, vs_sma50: 25, signal: "flat" },
    ];
    await page.route("**/screen*", (route) => route.fulfill({ json: rows }));

    await page.goto("/");
    await openScreener(page);

    await expect(page.locator('[data-testid="screener"] tbody tr')).toHaveCount(3);
    await expect(
      page.locator('[data-testid="screener"] tbody tr[data-symbol="AAA"] [data-col="price"]'),
    ).toHaveText("10.00");
    for (const col of ["ret_1m", "rsi14", "vs_sma50"]) {
      await expect(
        page.locator(`[data-testid="screener"] tbody tr[data-symbol="AAA"] [data-col="${col}"]`),
      ).toHaveText("—");
    }
    // The "nan" string from _scrub() must never reach the screen as text.
    await expect(
      page.locator('[data-testid="screener"] tbody tr[data-symbol="CCC"] [data-col="ret_1m"]'),
    ).toHaveText("—");

    // Missing values sort after every real number, in both directions.
    const retTh = page.locator('th:has(button[data-col="ret_1m"])');
    const symbols = () =>
      page
        .locator('[data-testid="screener"] tbody tr')
        .evaluateAll((els) => els.map((e) => e.getAttribute("data-symbol") ?? ""));

    await retTh.locator("button").click();
    const ascending = await symbols();
    expect(ascending[0]).toBe("BBB");
    expect(ascending.slice(1).sort()).toEqual(["AAA", "CCC"]);

    await retTh.locator("button").click();
    const descending = await symbols();
    expect(descending[0]).toBe("BBB");
    expect(descending.slice(1).sort()).toEqual(["AAA", "CCC"]);
    expectNoBrokenNumbers(await page.locator(".screener-panel").innerText());
  });

  test("the market tab lists the /prices bars for the active ticker", async ({ page, request }) => {
    const prices = await apiJson<{ symbol: string; bars: { t: string; c: number }[] }>(
      request,
      `/prices?symbol=AAPL&days=1000`,
    );

    await page.goto("/");
    await page.getByRole("tab", { name: "Market" }).click();

    const panel = page.locator('[aria-label="Market"]');
    const last = prices.bars[prices.bars.length - 1];
    await expect(panel.locator(".trow").first()).toContainText("AAPL");
    await expect(panel.locator(".trow").first()).toContainText(
      `${prices.bars.length} bars from /prices`,
    );
    await expect(panel.locator(".trow").first()).toContainText(`last close ${num(last.c)}`);

    await expect(panel.locator("table thead th")).toHaveText([
      "Date",
      "Open",
      "High",
      "Low",
      "Close",
    ]);
    await expect(panel.locator("tbody tr")).toHaveCount(12);
    await expect(panel.locator("tbody tr").first()).toContainText(last.t.slice(0, 10));
  });

  test("the screener requests follow the window sliders", async ({ page }) => {
    await page.goto("/");
    await openScreener(page);
    await expect(page.locator(".screener-panel .trow").first()).toContainText("8 rows");

    const seen: string[] = [];
    page.on("request", (r) => {
      if (r.url().includes("/screen")) seen.push(new URL(r.url()).search);
    });

    await page.getByRole("tab", { name: "Backtest" }).click();
    await page.locator('[data-testid="slider-a"] input[type="range"]').fill("50");
    const pending = page.waitForRequest(
      (r) => r.url().includes("/screen") && r.url().includes("a=50"),
    );
    await page.getByRole("tab", { name: "Screener" }).click();
    await pending;
    await expect(page.locator(".screener-panel .trow").first()).toContainText("a 50", {
      timeout: 15_000,
    });

    expect(seen.some((s) => s.includes("a=50"))).toBeTruthy();
    const fresh = seen[seen.length - 1];
    expect(fresh).toContain("strategy=ma");
    expect(fresh).toContain("a=50");
    expect(fresh).toContain("b=60");
  });
});
