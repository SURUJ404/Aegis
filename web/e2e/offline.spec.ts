import { expect, test } from "@playwright/test";
import { banner, button, makeState, msg } from "./support";

test("shows the unreachable banner while the api is down", async ({ page }) => {
  await page.route("**/api/v1/state", (route) => route.abort());
  await page.goto("/engine");

  await expect(banner(page)).toContainText("is unreachable");
  await expect(page.getByRole("link", { name: "Open the demo data" })).toBeVisible();
});

test("recovers the dashboard when the api comes back", async ({ page }) => {
  let down = true;
  await page.route("**/api/v1/state", (route) =>
    down ? route.abort() : route.fulfill({ json: makeState() }),
  );
  await page.goto("/engine");
  await expect(banner(page)).toBeVisible();

  down = false;
  await expect(banner(page)).toHaveCount(0, { timeout: 10_000 });
  await expect(page.getByRole("heading", { name: "Aegis" })).toBeVisible();
});

test("control failures show as messages instead of crashing", async ({ page }) => {
  await page.route("**/api/v1/state", (route) => route.fulfill({ json: makeState() }));
  await page.route("**/api/v1/control/*", (route) => route.fulfill({ status: 500, body: "boom" }));
  await page.goto("/engine");

  await expect(button(page, "Start")).toBeEnabled();
  await button(page, "Start").click();
  await expect(msg(page)).toContainText("start failed");
  await expect(button(page, "Start")).toBeVisible();
});
