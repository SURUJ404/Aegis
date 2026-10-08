import { expect, test } from "@playwright/test";
import type { BacktestResult } from "../src/quantApi";
import {
  API,
  apiJson,
  expectNoBrokenNumbers,
  metricCells,
  num,
  pct,
  pctUnsigned,
  resultsText,
  waitResults,
  watchErrors,
} from "./terminal-support";

test.describe("trading terminal", () => {
  test("shell renders the header, health pill and a tab strip with two parked tabs", async ({
    page,
    request,
  }) => {
    const health = await apiJson<{ ok: boolean; data_source: string }>(request, "/health");
    const symbols = await apiJson<string[]>(request, "/symbols");

    await page.goto("/");
    await expect(page.getByRole("heading", { name: "Aegis" })).toBeVisible();
    await expect(page.locator(".tag")).toHaveText("Trading Terminal");
    await expect(page.locator('[data-testid="health"]')).toHaveText(
      `API ok · ${health.data_source}`,
    );
    await expect(page.locator("header .pill.num")).toHaveText("AAPL");

    await expect(page.getByRole("tab", { name: "Backtest" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    await expect(page.getByRole("tab", { name: "Screener" })).toBeEnabled();
    await expect(page.getByRole("tab", { name: "Market" })).toBeEnabled();

    const live = page.getByRole("tab", { name: /Live trading/ });
    const portfolio = page.getByRole("tab", { name: /Portfolio/ });
    await expect(live).toBeDisabled();
    await expect(live).toHaveAttribute("title", "not ported yet");
    await expect(portfolio).toBeDisabled();
    await expect(portfolio).toHaveAttribute("title", "not ported yet");

    // The ticker datalist is the symbol universe from /symbols.
    await expect(page.locator("#ticker-list option")).toHaveCount(symbols.length);
    await expect(page.locator("#ticker-list option").first()).toHaveAttribute(
      "value",
      symbols[0],
    );

    await expect(page.getByRole("link", { name: "Engine dashboard" })).toHaveAttribute(
      "href",
      "/engine",
    );
  });

  test("every /backtest field is on screen, formatted from the API payload", async ({
    page,
    request,
  }) => {
    const api = await request.post(`${API}/backtest`, {
      data: { symbol: "AAPL", strategy: "ma", a: 20, b: 60, cost_bps: 10 },
    });
    expect(api.ok()).toBeTruthy();
    const bt = (await api.json()) as BacktestResult;

    await page.goto("/");
    const results = await waitResults(page);

    // Header line: symbol, parameter echo, bar count.
    await expect(results.locator(".trow").first()).toContainText("AAPL");
    await expect(results.locator(".trow").first()).toContainText("ma · a 20 · b 60 · 10.0 bps");
    await expect(results.locator(".trow").first()).toContainText(
      `${bt.dates.length.toLocaleString("en-US")} bars`,
    );

    const cells = await metricCells(page);
    const inMarket = bt.position.filter((p) => p > 0.5).length;
    expect(cells).toEqual({
      "annual-return-strategy": pct(bt.strategy.cagr),
      "annual-return-buyhold": pct(bt.buy_hold.cagr),
      "sharpe-strategy": num(bt.strategy.sharpe),
      "sharpe-buyhold": num(bt.buy_hold.sharpe),
      "worst-drawdown-strategy": pct(bt.strategy.max_drawdown),
      "worst-drawdown-buyhold": pct(bt.buy_hold.max_drawdown),
      "trades-strategy": String(bt.trades),
      "trades-buyhold": "—",
      "time-in-market-strategy": pctUnsigned(bt.exposure),
      "time-in-market-buyhold": "—",
      "in-market-days-strategy": `${inMarket} / ${bt.position.length}`,
      "in-market-days-buyhold": "—",
      "bars-strategy": String(bt.dates.length),
      "bars-buyhold": "—",
    });

    // Cost drag: points of annual return plus the bps that were charged.
    const costLine = page.getByTestId("cost-message");
    await expect(costLine).toHaveAttribute(
      "data-points",
      (bt.cost_drag_cagr * 100).toFixed(4),
    );
    await expect(costLine).toHaveText(
      `Fees cost ${num(bt.cost_drag_cagr * 100)} points of annual return (cost 10.0 bps).`,
    );

    expectNoBrokenNumbers(await resultsText(page));
  });

  test("price and equity charts render with descriptive labels", async ({ page }) => {
    await page.goto("/");
    await waitResults(page);

    const candles = page.getByTestId("candles");
    const equity = page.getByTestId("equity");
    await expect(candles).toBeVisible();
    await expect(equity).toBeVisible();

    const size = await candles.evaluate((c) => ({ w: c.width, h: c.height }));
    expect(size.w).toBeGreaterThan(100);
    expect(size.h).toBeGreaterThan(100);

    await expect(candles).toHaveAttribute("aria-label", /AAPL daily: \d+ daily candles/);
    await expect(candles).toHaveAttribute("aria-label", /overlays MA 20 and MA 60/);
    await expect(equity).toHaveAttribute("aria-label", /equity from 1\.00 to /);

    // Both legends name what the reader is looking at.
    await expect(page.locator(".legend").first()).toContainText("up");
    await expect(page.locator(".legend").first()).toContainText("down");
    await expect(page.locator(".legend").first()).toContainText("MA 20");
    await expect(page.locator(".legend").last()).toContainText("strategy");
    await expect(page.locator(".legend").last()).toContainText("buy & hold");
    await expect(page.locator(".legend").last()).toContainText("in the market");
  });

  test("a healthy load and tab tour produces zero console or page errors", async ({
    page,
  }) => {
    const errors = watchErrors(page);
    await page.goto("/");
    await waitResults(page);

    await page.getByRole("tab", { name: "Screener" }).click();
    await expect(page.getByTestId("screener")).toBeVisible({ timeout: 15_000 });
    await page.getByRole("tab", { name: "Market" }).click();
    await expect(page.locator("table.bars")).toBeVisible();
    await page.getByRole("tab", { name: "Backtest" }).click();
    await expect(page.getByTestId("metrics")).toBeVisible();

    expect(errors).toEqual([]);
  });

  test("strategy switch rewrites the sliders and reruns the backtest", async ({ page }) => {
    await page.goto("/");
    await waitResults(page);

    await expect(page.getByTestId("slider-a")).toContainText("Fast MA (days)");
    await expect(page.getByTestId("slider-b")).toContainText("Slow MA (days)");

    await page.getByTestId("strategy").selectOption("mom");
    await expect(page.getByTestId("slider-a")).toContainText("Lookback (days)");
    await expect(page.getByTestId("slider-b-hidden")).toBeVisible();
    await page.locator('[data-testid="results"] .trow').first();
    await expect(page.locator('[data-testid="results"] .trow').first()).toContainText(
      "mom · a 20",
      { timeout: 15_000 },
    );
    await waitResults(page);

    await page.getByTestId("strategy").selectOption("rsi");
    await expect(page.getByTestId("slider-a")).toContainText("RSI period (days)");
    await expect(page.getByTestId("slider-b")).toContainText("Buy below RSI");
    await expect(page.locator('[data-testid="results"] .trow').first()).toContainText(
      "rsi · a 20 · b 60",
      { timeout: 15_000 },
    );
    await waitResults(page);
    expectNoBrokenNumbers(await resultsText(page));
  });

  test("Enter commits the ticker box and / jumps back to it", async ({ page }) => {
    await page.goto("/");
    await waitResults(page);

    const pending = page.waitForRequest(
      (r) =>
        r.method() === "POST" &&
        r.url().includes("/backtest") &&
        (r.postData() ?? "").includes("NVDA"),
    );
    await page.getByTestId("ticker").fill("NVDA");
    await page.getByTestId("ticker").press("Enter");
    await pending;
    await expect(page.locator('[data-testid="results"] .trow').first()).toContainText("NVDA");
    await waitResults(page);
    await expect(page.locator("header .pill.num")).toHaveText("NVDA");

    // `/` focuses the ticker box from anywhere that is not a form field.
    await page.getByRole("heading", { name: "Aegis" }).click();
    await page.keyboard.press("/");
    await expect(page.getByTestId("ticker")).toBeFocused();
    await expect(page.getByTestId("ticker")).toHaveValue("NVDA");
  });

  test("an impossible MA window blocks the run and the API rejects it too", async ({
    page,
    request,
  }) => {
    await page.goto("/");
    const results = await waitResults(page);
    const before = await metricCells(page);

    let posts = 0;
    page.on("request", (r) => {
      if (r.method() === "POST" && r.url().includes("/backtest")) posts += 1;
    });

    await page.locator('[data-testid="slider-a"] input[type="range"]').fill("80");
    await expect(page.getByTestId("blocked")).toHaveText(
      "Fast MA (80) must be below slow MA (60) — move a slider before running.",
    );
    await page.waitForTimeout(800); // well past the debounce
    expect(posts).toBe(0);
    expect(await metricCells(page)).toEqual(before);
    await expect(results).toHaveAttribute("data-source", "api");

    // The guard mirrors the server rule instead of inventing one.
    const rejected = await request.post(`${API}/backtest`, {
      data: { symbol: "AAPL", strategy: "ma", a: 80, b: 60, cost_bps: 10 },
    });
    expect(rejected.status()).toBe(422);
  });

  test("rapid slider movement collapses into a single debounced request", async ({
    page,
  }) => {
    await page.goto("/");
    await waitResults(page);

    const bodies: string[] = [];
    page.on("request", (r) => {
      if (r.method() === "POST" && r.url().includes("/backtest")) {
        bodies.push(r.postData() ?? "");
      }
    });

    const slider = page.locator('[data-testid="slider-a"] input[type="range"]');
    for (const value of ["21", "24", "28", "33"]) await slider.fill(value);
    await page.waitForTimeout(900);

    expect(bodies).toHaveLength(1);
    expect(JSON.parse(bodies[0])).toMatchObject({ symbol: "AAPL", a: 33 });
    await waitResults(page);
  });

  test("a slow answer can never overwrite a newer one", async ({ page, request }) => {
    const msft = await request.post(`${API}/backtest`, {
      data: { symbol: "MSFT", strategy: "ma", a: 20, b: 60, cost_bps: 10 },
    });
    const msftBt = (await msft.json()) as BacktestResult;

    await page.route("**/backtest", async (route) => {
      const body = JSON.parse(route.request().postData() ?? "{}") as { symbol?: string };
      const delay = body.symbol === "AAPL" ? 1_500 : 100;
      await new Promise((r) => setTimeout(r, delay));
      try {
        await route.continue();
      } catch {
        /* the caller already abandoned this answer */
      }
    });

    await page.goto("/");
    // The first (slow) answer is in flight when the user retargets the run.
    await expect(page.getByTestId("loading")).toBeVisible();
    const pending = page.waitForRequest(
      (r) => r.method() === "POST" && (r.postData() ?? "").includes('"MSFT"'),
    );
    await page.getByTestId("ticker").fill("MSFT");
    await page.getByTestId("ticker").press("Enter");
    await pending;
    await waitResults(page);

    // AAPL's answer lands later; the screen must stay on MSFT.
    await page.waitForTimeout(2_500);
    await expect(page.locator('[data-testid="results"] .trow').first()).toContainText("MSFT");
    const cells = await metricCells(page);
    expect(cells["annual-return-strategy"]).toBe(pct(msftBt.strategy.cagr));
    expect(cells["trades-strategy"]).toBe(String(msftBt.trades));
  });

  test("the tab strip is keyboard operable and skips the parked tabs", async ({ page }) => {
    await page.goto("/");
    await waitResults(page);

    const backtest = page.getByRole("tab", { name: "Backtest" });
    const screener = page.getByRole("tab", { name: "Screener" });
    const market = page.getByRole("tab", { name: "Market" });

    await backtest.focus();
    await page.keyboard.press("ArrowRight");
    await expect(screener).toBeFocused();
    await expect(screener).toHaveAttribute("aria-selected", "true");
    await expect(page.locator('[role="tabpanel"]')).toHaveAttribute(
      "aria-labelledby",
      "tab-screener",
    );

    await page.keyboard.press("ArrowRight");
    await expect(market).toBeFocused();
    await page.keyboard.press("ArrowRight"); // wraps, skipping Live trading/Portfolio
    await expect(backtest).toBeFocused();
    await page.keyboard.press("End");
    await expect(market).toBeFocused();
    await page.keyboard.press("Home");
    await expect(backtest).toBeFocused();
  });

  test("the theme switch follows the tokens and survives a reload", async ({ page }) => {
    await page.goto("/");
    await waitResults(page);

    await expect(page.locator("html")).not.toHaveAttribute("data-theme", "dark");
    const toggle = page.getByTestId("theme");
    await toggle.click();
    await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
    await expect(toggle).toHaveAttribute("aria-label", "Switch to light mode");

    await page.reload();
    await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");

    await page.getByTestId("theme").click();
    await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  });

  test("the cost slider is echoed back in the parameters and the cost line", async ({
    page,
    request,
  }) => {
    await page.goto("/");
    await waitResults(page);

    const pending = page.waitForRequest(
      (r) => r.method() === "POST" && (r.postData() ?? "").includes('"cost_bps":42'),
    );
    await page.locator('[data-testid="cost-slider"] input[type="range"]').fill("42");
    await pending;
    await expect(page.locator('[data-testid="results"] .trow').first()).toContainText(
      "42.0 bps",
    );
    await waitResults(page);

    const api = await request.post(`${API}/backtest`, {
      data: { symbol: "AAPL", strategy: "ma", a: 20, b: 60, cost_bps: 42 },
    });
    const bt = (await api.json()) as BacktestResult;
    await expect(page.getByTestId("cost-message")).toHaveAttribute(
      "data-points",
      (bt.cost_drag_cagr * 100).toFixed(4),
    );
    await expect(page.getByTestId("cost-message")).toContainText("cost 42.0 bps");
  });
});
