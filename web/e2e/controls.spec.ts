import { expect, test } from "@playwright/test";
import { button, makeState, mockControl, mockState } from "./support";

test("initial control button states for idle engine", async ({ page }) => {
  await mockState(page, makeState());
  await mockControl(page);
  await page.goto("/");

  await expect(button(page, "Start")).toBeEnabled();
  await expect(button(page, "Stop")).toBeDisabled();
  await expect(button(page, "Reset kill switch")).toBeDisabled();
  await expect(button(page, "Kill switch")).toBeEnabled();
});

test("start posts to the control api and toasts the answer", async ({ page }) => {
  await mockState(page, makeState());
  await mockControl(page, { accepted: true, message: "control event published" });
  await page.goto("/");

  const req = page.waitForRequest((r) => r.url().includes("/api/v1/control/start") && r.method() === "POST");
  await button(page, "Start").click();
  await req;
  await expect(page.locator(".toast")).toHaveText("start: control event published");
});

test("running state swaps start/stop buttons", async ({ page }) => {
  await mockState(page, makeState({ strategy_running: true }));
  await mockControl(page);
  await page.goto("/");

  await expect(page.getByText("strategy running")).toBeVisible();
  await expect(button(page, "Start")).toBeDisabled();
  await expect(button(page, "Stop")).toBeEnabled();
});

test("busy while a control call is pending disables every button", async ({ page }) => {
  await mockState(page, makeState());
  await mockControl(page, { accepted: true, message: "ok" }, 700);
  await page.goto("/");
  await expect(button(page, "Start")).toBeEnabled();

  await button(page, "Start").click();
  await expect(button(page, "Start")).toBeDisabled();
  await expect(button(page, "Kill switch")).toBeDisabled();
  await expect(page.locator(".toast")).toBeVisible({ timeout: 5_000 });
  await expect(button(page, "Start")).toBeEnabled();
});

test("kill sends the prompted reason", async ({ page }) => {
  await mockState(page, makeState());
  await mockControl(page, { accepted: true, message: "kill switch engaged" });
  await page.goto("/");

  const req = page.waitForRequest((r) => r.url().includes("/api/v1/control/kill"));
  page.once("dialog", (d) => d.accept("ops drill"));
  await button(page, "Kill switch").click();

  const kill = await req;
  expect(kill.postDataJSON()).toEqual({ reason: "ops drill" });
  await expect(page.locator(".toast")).toHaveText("kill: kill switch engaged");
});

test("cancelling the kill prompt sends nothing", async ({ page }) => {
  await mockState(page, makeState());
  await mockControl(page);
  await page.goto("/");

  let killCalls = 0;
  await page.route("**/api/v1/control/kill", (route) => {
    killCalls += 1;
    return route.fulfill({ json: { accepted: true, message: "should not happen" } });
  });
  page.once("dialog", (d) => d.dismiss());
  await button(page, "Kill switch").click();

  await page.waitForTimeout(500);
  expect(killCalls).toBe(0);
  await expect(page.locator(".toast")).toHaveCount(0);
});

test("rejected control answers still toast the server message", async ({ page }) => {
  await mockState(page, makeState());
  // The real server answers 503 with the rejection message (e.g. engine not listening).
  await page.route("**/api/v1/control/*", (route) =>
    route.fulfill({ status: 503, json: { accepted: false, message: "engine not listening" } }),
  );
  await page.goto("/");

  await button(page, "Start").click();
  await expect(page.locator(".toast")).toContainText("start failed");
  await expect(page.locator(".toast")).toContainText("engine not listening");
});

test("http error responses toast as failures", async ({ page }) => {
  await mockState(page, makeState());
  await page.route("**/api/v1/control/*", (route) =>
    route.fulfill({ status: 409, json: { detail: "engine busy" } }),
  );
  await page.goto("/");

  await button(page, "Start").click();
  await expect(page.locator(".toast")).toContainText("start failed");
});

test("a later toast is not cleared early by an earlier action's timer", async ({ page }) => {
  await mockState(page, makeState());
  await mockControl(page, { accepted: true, message: "published" });
  await page.goto("/");

  page.once("dialog", (d) => d.accept("drill"));
  await button(page, "Kill switch").click();
  await expect(page.locator(".toast")).toHaveText("kill: published");

  await page.waitForTimeout(2_000);
  await button(page, "Start").click();
  await expect(page.locator(".toast")).toHaveText("start: published");

  // 4.5s after the first toast, the first action's 4s timer has fired;
  // the second toast must still be alive until its own 4s window ends (~6s).
  await page.waitForTimeout(2_500);
  await expect(page.locator(".toast")).toHaveText("start: published");
});
