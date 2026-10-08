import { expect, test } from "@playwright/test";
import { banner, button, msg } from "./support";

test("dashboard connects to the live api-server and can post a control", async ({ page }) => {
  await page.goto("/engine");

  await expect(banner(page)).toHaveCount(0, { timeout: 10_000 });
  await expect(page.getByRole("heading", { name: "Aegis" })).toBeVisible();
  await expect(button(page, "Start")).toBeEnabled({ timeout: 10_000 });
  await button(page, "Start").click();
  await expect(msg(page)).toContainText("start:");
});
