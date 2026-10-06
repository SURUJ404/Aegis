import { expect, test } from "@playwright/test";

test("dashboard connects to the live api-server and can post a control", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByText("api connected")).toBeVisible({ timeout: 10_000 });
  await expect(page.getByText("api offline")).toHaveCount(0);

  await page.getByRole("button", { name: "Start", exact: true }).click();
  await expect(page.locator(".toast")).toContainText("start:");
});
