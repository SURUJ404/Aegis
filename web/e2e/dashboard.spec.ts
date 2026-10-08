import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";
import type { LogEntry, StateSummary } from "../src/types";
import {
  button,
  makeLog,
  makeLogEntry,
  makeMarket,
  makeState,
  meter,
  mockBook,
  mockLog,
  mockState,
} from "./support";

async function mockApi(page: Page, state: StateSummary, entries: LogEntry[] = []) {
  await mockState(page, state);
  await mockBook(page);
  await mockLog(page, makeLog(entries));
}

test("idle dashboard renders header, meters, ladder and log panels", async ({ page }) => {
  await mockApi(
    page,
    makeState({ market_state: [makeMarket()] }),
    [
      // Rest, fully fill (moves the position, leaves the book), rest again.
      makeLogEntry(1, "place_order", { side: "bid", price: "64000", qty: "0.50", tif: "gtc", id: "o-a" }),
      makeLogEntry(2, "fill", { side: "bid", price: "64000", qty: "0.50", id: "o-a" }),
      makeLogEntry(3, "place_order", { side: "bid", price: "64000", qty: "0.25", tif: "gtc", id: "o-b" }),
    ],
  );
  await page.goto("/engine");

  await expect(page.getByRole("heading", { name: "Aegis" })).toBeVisible();
  await expect(page.locator(".mkt")).toHaveText("BTC-USDT");
  await expect(page.locator(".mid")).toHaveText("64,000.5");
  await expect(page.getByText("Stopped", { exact: true })).toBeVisible();
  await expect(meter(page, "Position")).toHaveText("0.50");
  await expect(meter(page, "Open orders")).toHaveText("1");
  await expect(page.locator(".row.b.own")).toBeVisible();
  await expect(page.getByText("buy 0.50 @ 64000 GTC")).toBeVisible();
  await expect(page.getByText("fill 0.50 @ 64000")).toBeVisible();
  await expect(page.getByText("buy 0.25 @ 64000 GTC")).toBeVisible();
  await expect(page.getByText("Live: new entries stream in")).toBeVisible();
});

test("partially filled order counts as open", async ({ page }) => {
  await mockApi(
    page,
    makeState(),
    [
      // A rests and is partially filled (stays open); C rests and fills fully (closes).
      makeLogEntry(1, "place_order", { side: "bid", price: "64000", qty: "0.50", tif: "gtc", id: "o-a" }),
      makeLogEntry(2, "fill", { side: "bid", price: "64000", qty: "0.10", id: "o-a" }),
      makeLogEntry(3, "place_order", { side: "bid", price: "64100", qty: "0.25", tif: "gtc", id: "o-c" }),
      makeLogEntry(4, "fill", { side: "bid", price: "64100", qty: "0.25", id: "o-c" }),
    ],
  );
  await page.goto("/engine");

  await expect(meter(page, "Open orders")).toHaveText("1");
  await page.getByRole("tab", { name: "My orders" }).click();
  await expect(page.locator(".list .li")).toHaveCount(1);
  await expect(page.getByText("0.40 @ 64,000.0")).toBeVisible();
});

test("log, fills and my orders tabs reflect the order log", async ({ page }) => {
  await mockApi(
    page,
    makeState(),
    [
      makeLogEntry(1, "place_order", { side: "bid", price: "64000", qty: "0.30", tif: "gtc", id: "o-x" }),
      makeLogEntry(2, "fill", { side: "bid", price: "64000", qty: "0.10", id: "o-x" }),
    ],
  );
  await page.goto("/engine");

  // Log tab: every WAL row as text.
  await expect(page.getByText("buy 0.30 @ 64000 GTC")).toBeVisible();
  await expect(page.getByText("fill 0.10 @ 64000")).toBeVisible();

  // Fills tab: just the execution.
  await page.getByRole("tab", { name: "Fills" }).click();
  await expect(page.locator(".list .li")).toHaveCount(1);
  await expect(page.getByText("0.10 @ 64000.0")).toBeVisible();

  // My orders tab: the surviving remainder.
  await page.getByRole("tab", { name: "My orders" }).click();
  await expect(page.locator(".list .li")).toHaveCount(1);
  await expect(page.getByText("0.20 @ 64,000.0")).toBeVisible();
});

test("empty engine shows every empty state cleanly", async ({ page }) => {
  await mockApi(page, makeState(), []);
  await page.goto("/engine");

  await expect(page.locator(".mkt")).toHaveText("–");
  await expect(page.getByText("Stopped", { exact: true })).toBeVisible();
  await expect(meter(page, "Position")).toHaveText("0.00");
  await expect(meter(page, "Open orders")).toHaveText("0");
  await expect(page.getByText("Nothing here yet.")).toBeVisible();
  await expect(page.getByText("Live depth from /api/v1/book")).toBeVisible();

  await page.getByRole("tab", { name: "My orders" }).click();
  await expect(page.getByText("No open orders.")).toBeVisible();

  await page.getByRole("tab", { name: "Fills" }).click();
  await expect(page.getByText("Nothing here yet.")).toBeVisible();
});

test("demo replay canvas draws the order log", async ({ page }) => {
  await page.goto("/engine?demo");

  await expect(page.getByText("Demo data")).toBeVisible();
  await expect(page.locator(".mkt")).toHaveText("BTC-USDT-PERP");
  await expect(page.getByRole("slider", { name: "Replay position in the order log" })).toBeVisible({
    timeout: 10_000,
  });
});

test("halted risk state surfaces the reason and swaps controls", async ({ page }) => {
  await mockApi(
    page,
    makeState({ risk: { armed: true, halted: true, halt_reason: "drill", updated_at: 1_790_000_000_000 } }),
    [],
  );
  await page.goto("/engine");

  await expect(page.getByText("Halted: drill")).toBeVisible();
  await expect(page.getByText("Release kill switch")).toBeVisible();
  await expect(button(page, "Engage kill switch")).toHaveCount(0);
  await expect(button(page, "Start")).toBeDisabled();
  await expect(button(page, "Reset")).toBeEnabled();
  await expect(button(page, "Stop")).toBeDisabled();
});
