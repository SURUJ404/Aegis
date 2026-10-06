import { expect, test } from "@playwright/test";
import { makeState } from "./support";

test("shows offline pill and banner while the api is unreachable", async ({ page }) => {
  await page.route("**/api/v1/state", (route) => route.abort());
  await page.goto("/");

  await expect(page.getByText("api offline")).toBeVisible();
  await expect(page.locator(".error-banner")).toContainText("API unreachable");
});

test("recovers the connection banner when the api comes back", async ({ page }) => {
  let down = true;
  await page.route("**/api/v1/state", (route) =>
    down ? route.abort() : route.fulfill({ json: makeState() }),
  );
  await page.goto("/");
  await expect(page.locator(".error-banner")).toBeVisible();

  down = false;
  await expect(page.getByText("api connected")).toBeVisible({ timeout: 5_000 });
  await expect(page.locator(".error-banner")).toHaveCount(0);
});

test("control failures toast instead of crashing", async ({ page }) => {
  await page.route("**/api/v1/state", (route) => route.fulfill({ json: makeState() }));
  await page.route("**/api/v1/control/*", (route) => route.fulfill({ status: 500, body: "boom" }));
  await page.goto("/");

  await page.getByRole("button", { name: "Start", exact: true }).click();
  await expect(page.locator(".toast")).toContainText("start failed");
});
