import { expect, test } from "@playwright/test";
import {
  button,
  makeInventory,
  makeMarket,
  makeOrder,
  makePosition,
  makeState,
  mockState,
  statValue,
} from "./support";

test("idle dashboard renders stats, panels and live connection", async ({ page }) => {
  await mockState(
    page,
    makeState({
      positions: [makePosition()],
      inventory: [makeInventory()],
      orders: [
        makeOrder({ order_id: "order-old", created_at: 1_790_000_000_000 }),
        makeOrder({
          order_id: "order-new",
          status: "filled",
          created_at: 1_790_000_100_000,
          filled_quantity: "0.25",
          avg_fill_price: "64000.00",
        }),
      ],
      market_state: [makeMarket()],
    }),
  );
  await page.goto("/");

  await expect(page.getByText("api connected")).toBeVisible();
  await expect(page.getByText("strategy stopped")).toBeVisible();
  await expect(statValue(page, "Positions")).toHaveText("1");
  await expect(statValue(page, "Net inventory")).toHaveText("1.5");
  await expect(statValue(page, "Open orders")).toHaveText("1");
  await expect(page.locator('.stat:has-text("Open orders") .stat-sub')).toHaveText("2 total");
  await expect(statValue(page, "Quotable markets")).toHaveText("1");
  await expect(statValue(page, "Realized PnL")).toHaveText("250.00");
  await expect(statValue(page, "Mark price (BTC-USDT)")).toHaveText("64,000.50");

  // newest order first
  const idCells = page.locator('section.card:has-text("Orders") tbody td.mono');
  await expect(idCells.first()).toHaveText("order-ne");
  await expect(page.getByText("no positions yet")).toHaveCount(0);
  await expect(page.locator('.card:has-text("Market state")').getByText("BTC-USDT")).toBeVisible();
});

test("partially filled order counts as open", async ({ page }) => {
  await mockState(
    page,
    makeState({
      orders: [
        makeOrder({ status: "partially_filled", filled_quantity: "0.1" }),
        makeOrder({ status: "filled", order_id: "order-0002" }),
      ],
    }),
  );
  await page.goto("/");
  await expect(statValue(page, "Open orders")).toHaveText("1");
});

test("order status pills: terminal states dim, open states warn", async ({ page }) => {
  await mockState(
    page,
    makeState({
      orders: [
        makeOrder({ order_id: "o-fill", status: "filled" }),
        makeOrder({ order_id: "o-canc", status: "cancelled" }),
        makeOrder({ order_id: "o-ack", status: "acknowledged" }),
        makeOrder({ order_id: "o-part", status: "partially_filled" }),
      ],
    }),
  );
  await page.goto("/");

  const card = page.locator("section.card").filter({ hasText: "Orders (4)" });
  await expect(card.locator("tr", { hasText: "o-fill" }).locator(".pill")).toHaveClass(/pill-dim/);
  await expect(card.locator("tr", { hasText: "o-canc" }).locator(".pill")).toHaveClass(/pill-dim/);
  await expect(card.locator("tr", { hasText: "o-ack" }).locator(".pill")).toHaveClass(/pill-warn/);
  await expect(card.locator("tr", { hasText: "o-part" }).locator(".pill")).toHaveClass(/pill-warn/);
});

test("empty engine shows every empty state cleanly", async ({ page }) => {
  await mockState(page, makeState());
  await page.goto("/");

  await expect(statValue(page, "Positions")).toHaveText("0");
  await expect(statValue(page, "Net inventory")).toHaveText("0");
  await expect(statValue(page, "Open orders")).toHaveText("0");
  await expect(statValue(page, "Quotable markets")).toHaveText("0");
  await expect(statValue(page, "Mark price (BTC-USDT)")).toHaveText("—");
  await expect(page.getByText("no positions yet")).toBeVisible();
  await expect(page.getByText("flat")).toBeVisible();
  await expect(page.getByText("no orders yet")).toBeVisible();
  await expect(page.getByText("no market data yet")).toBeVisible();
});

test("sparkline draws a flat line when pnl never changes", async ({ page }) => {
  await mockState(page, makeState({ inventory: [makeInventory({ realized_pnl: "250.00" })] }));
  await page.goto("/");
  await expect(page.locator("svg.sparkline")).toBeVisible({ timeout: 10_000 });
});

test("halted risk state surfaces reason and swaps controls", async ({ page }) => {
  await mockState(
    page,
    makeState({ risk: { armed: true, halted: true, halt_reason: "drill", updated_at: 1_790_000_000_000 } }),
  );
  await page.goto("/");

  await expect(page.getByText("KILL SWITCH", { exact: true })).toBeVisible();
  await expect(page.getByText("Reason: drill")).toBeVisible();
  await expect(button(page, "Start")).toBeDisabled();
  await expect(button(page, "Kill switch")).toBeDisabled();
  await expect(button(page, "Reset kill switch")).toBeEnabled();
  await expect(button(page, "Stop")).toBeDisabled();
});
