import { expect, test } from "@playwright/test";
import { button, makeState, mockControl, mockState, msg } from "./support";

test("initial control button states for idle engine", async ({ page }) => {
  await mockState(page, makeState());
  await mockControl(page);
  await page.goto("/engine");

  await expect(button(page, "Start")).toBeEnabled();
  await expect(button(page, "Stop")).toBeDisabled();
  await expect(button(page, "Reset")).toBeEnabled();
  await expect(button(page, "Engage kill switch")).toBeEnabled();
});

test("start posts to the control api and shows the answer", async ({ page }) => {
  await mockState(page, makeState());
  await mockControl(page, { accepted: true, message: "control event published" });
  await page.goto("/engine");

  const req = page.waitForRequest((r) => r.url().includes("/api/v1/control/start") && r.method() === "POST");
  await button(page, "Start").click();
  await req;
  await expect(msg(page)).toHaveText("start: control event published");
});

test("running state swaps start/stop buttons", async ({ page }) => {
  await mockState(page, makeState({ strategy_running: true }));
  await mockControl(page);
  await page.goto("/engine");

  await expect(page.getByText("Running", { exact: true })).toBeVisible();
  await expect(button(page, "Start")).toBeDisabled();
  await expect(button(page, "Stop")).toBeEnabled();
});

test("busy while a control call is pending disables every button", async ({ page }) => {
  await mockState(page, makeState());
  await mockControl(page, { accepted: true, message: "ok" }, 700);
  await page.goto("/engine");
  await expect(button(page, "Start")).toBeEnabled();

  await button(page, "Start").click();
  await expect(button(page, "Start")).toBeDisabled();
  await expect(button(page, "Engage kill switch")).toBeDisabled();
  await expect(msg(page)).toBeVisible({ timeout: 5_000 });
  await expect(button(page, "Start")).toBeEnabled();
});

test("kill sends the prompted reason", async ({ page }) => {
  await mockState(page, makeState());
  await mockControl(page, { accepted: true, message: "kill switch engaged" });
  await page.goto("/engine");

  const req = page.waitForRequest((r) => r.url().includes("/api/v1/control/kill"));
  page.once("dialog", (d) => d.accept("ops drill"));
  await button(page, "Engage kill switch").click();

  const kill = await req;
  expect(kill.postDataJSON()).toEqual({ reason: "ops drill" });
  await expect(msg(page)).toHaveText("kill: kill switch engaged");
});

test("cancelling the kill prompt sends nothing", async ({ page }) => {
  await mockState(page, makeState());
  await mockControl(page);
  await page.goto("/engine");

  let killCalls = 0;
  await page.route("**/api/v1/control/kill", (route) => {
    killCalls += 1;
    return route.fulfill({ json: { accepted: true, message: "should not happen" } });
  });
  page.once("dialog", (d) => d.dismiss());
  await button(page, "Engage kill switch").click();

  await page.waitForTimeout(500);
  expect(killCalls).toBe(0);
  await expect(msg(page)).toHaveCount(0);
});

test("rejected control answers still show the server message", async ({ page }) => {
  await mockState(page, makeState());
  // The real server answers 200/5xx with the rejection message (e.g. engine not listening).
  await page.route("**/api/v1/control/*", (route) =>
    route.fulfill({ status: 503, json: { accepted: false, message: "engine not listening" } }),
  );
  await page.goto("/engine");

  await button(page, "Start").click();
  await expect(msg(page)).toContainText("start failed");
  await expect(msg(page)).toContainText("engine not listening");
});

test("http error responses show as failures", async ({ page }) => {
  await mockState(page, makeState());
  await page.route("**/api/v1/control/*", (route) =>
    route.fulfill({ status: 409, json: { detail: "engine busy" } }),
  );
  await page.goto("/engine");

  await button(page, "Start").click();
  await expect(msg(page)).toContainText("start failed");
});

test("a later message is not cleared early by an earlier action's timer", async ({ page }) => {
  await mockState(page, makeState());
  await mockControl(page, { accepted: true, message: "published" });
  await page.goto("/engine");

  page.once("dialog", (d) => d.accept("drill"));
  await button(page, "Engage kill switch").click();
  await expect(msg(page)).toHaveText("kill: published");

  await page.waitForTimeout(2_000);
  await button(page, "Start").click();
  await expect(msg(page)).toHaveText("start: published");

  // 4.5s after the first message, the first action's 5s timer has been
  // re-armed by the second; the new message must still be alive (~2.5s old).
  await page.waitForTimeout(2_500);
  await expect(msg(page)).toHaveText("start: published");
});
