import { expect, test } from "@playwright/test";
import { expectNoBrokenNumbers, resultsText, waitResults, watchErrors } from "./terminal-support";

test.describe("failure and timing", () => {
  test("an API error shows a banner, an offline preview and a working Retry", async ({
    page,
  }) => {
    let down = true;
    await page.route("**/backtest", (route) =>
      down ? route.fulfill({ status: 500, body: "boom" }) : route.continue(),
    );
    const errors = watchErrors(page, true);

    await page.goto("/");
    const error = page.getByTestId("error");
    await expect(error).toBeVisible({ timeout: 15_000 });
    await expect(error).toContainText("The API returned an error (500).");
    await expect(page.getByTestId("retry")).toBeVisible();

    // The page keeps working: a local preview fills in for the API.
    await expect(page.getByTestId("offline-badge")).toBeVisible();
    await expect(page.locator('[data-testid="results"]')).toHaveAttribute(
      "data-source",
      "offline",
    );
    await expect(page.getByTestId("metrics")).toBeVisible();
    expectNoBrokenNumbers(await resultsText(page));

    down = false;
    await page.getByTestId("retry").click();
    await expect(page.locator('[data-testid="results"]')).toHaveAttribute(
      "data-source",
      "api",
      { timeout: 15_000 },
    );
    await expect(error).toHaveCount(0);
    await expect(page.getByTestId("offline-badge")).toHaveCount(0);

    expect(errors).toEqual([]);
  });

  test("with the whole API down the terminal still renders, with no JS errors", async ({
    page,
  }) => {
    for (const pattern of ["**/health*", "**/symbols*", "**/backtest", "**/prices*", "**/screen*"]) {
      await page.route(pattern, (route) => route.abort());
    }
    const errors = watchErrors(page, true);

    await page.goto("/");
    await expect(page.getByTestId("health")).toHaveText("API unreachable");
    await expect(page.getByTestId("offline-badge")).toBeVisible();
    await expect(page.locator('[data-testid="results"]')).toHaveAttribute(
      "data-source",
      "offline",
    );
    await expect(page.getByTestId("metrics")).toBeVisible();
    await expect(page.getByTestId("error")).toContainText("Could not reach the API");

    const text = await resultsText(page);
    expectNoBrokenNumbers(text);
    // Missing data is shown as an em dash, never as a raw sentinel.
    expect(text).not.toMatch(/null/i);

    expect(errors).toEqual([]);
  });

  test("the wake-up hint appears when the server is slow to answer", async ({ page }) => {
    await page.route("**/backtest", async (route) => {
      await new Promise((r) => setTimeout(r, 4_000));
      try {
        await route.continue();
      } catch {
        /* test finished first */
      }
    });

    await page.goto("/");
    await expect(page.getByTestId("waking")).toHaveText(
      "Waking the server, this can take a minute…",
      { timeout: 10_000 },
    );
    await expect(page.locator('[data-testid="results"]')).toHaveAttribute(
      "data-source",
      "api",
      { timeout: 20_000 },
    );
    await expect(page.getByTestId("waking")).toHaveCount(0);
    await expect(page.getByTestId("error")).toHaveCount(0);
  });

  test("a request that never answers fails at the timeout instead of hanging", async ({
    page,
  }) => {
    await page.route("**/backtest", async (route) => {
      await new Promise((r) => setTimeout(r, 25_000));
      try {
        await route.continue();
      } catch {
        /* the client gave up first */
      }
    });

    await page.goto("/");
    const error = page.getByTestId("error");
    await expect(error).toBeVisible({ timeout: 25_000 });
    await expect(error).toContainText("The API did not answer within 15s.");
    await expect(page.getByTestId("offline-badge")).toBeVisible();
    expectNoBrokenNumbers(await resultsText(page));
  });

  test("prices failing on its own leaves the backtest and an empty market tab", async ({
    page,
  }) => {
    await page.route("**/prices*", (route) => route.abort());
    const errors = watchErrors(page, true);

    await page.goto("/");
    const results = await waitResults(page);
    await expect(page.getByTestId("candles")).toHaveCount(0);
    await expect(page.getByTestId("equity")).toBeVisible();
    await expect(page.getByTestId("error")).toHaveCount(0);
    await expect(results).toHaveAttribute("data-source", "api");
    expectNoBrokenNumbers(await resultsText(page));

    await page.getByRole("tab", { name: "Market" }).click();
    await expect(page.locator('[aria-label="Market"]')).toContainText("0 bars from /prices");
    await expect(page.locator('[aria-label="Market"] .empty')).toHaveText(
      "No bars loaded for this ticker.",
    );

    expect(errors).toEqual([]);
  });
});
