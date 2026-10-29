// The Rust fixture starts the real local services and supplies disposable data.
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { join } from "node:path";
import { createInterface } from "node:readline";

const require = createRequire(new URL("../../../../web/shell/package.json", import.meta.url));
const { chromium } = require("playwright");
const required = (name) => {
  assert.ok(process.env[name], `set ${name}`);
  return process.env[name];
};
const input = createInterface({ input: process.stdin });
const fixture = JSON.parse((await input[Symbol.asyncIterator]().next()).value);
input.close();
assert.ok(fixture.reportDirectory, "the Rust fixture supplies the screenshot directory");
console.error(`Receiving screenshots: ${fixture.reportDirectory}`);
const browser = await chromium.launch({ channel: "chrome", headless: true });
const errors = [];
const captured = new Set();
// A failed step is recorded and the run goes on, so one run lists every
// failed step. The step after a failure first goes to its own start.
const failures = [];
let page;
try {
  page = await browser.newPage({ viewport: { width: 1400, height: 900 } });
  page.on("pageerror", (error) => errors.push(error.message));
  const columns = async (title, labels) => {
    const headers = page.getByRole("table", { name: title, exact: true }).getByRole("columnheader");
    assert.equal(await headers.count(), labels.length);
    const actual = await headers.locator(".column-heading :is(.column-sort-label, .column-title)").allTextContents();
    assert.deepEqual(actual.sort(), [...labels].sort());
  };
  const capture = async (name, table = false) => {
    if (name === "purchase-order" || name === "receipt") await page.locator("[data-generator-detail]").waitFor();
    await page.evaluate(() => document.fonts.ready);
    for (const text of await page
      .getByRole("navigation", { name: "Platform location" })
      .locator("a, [aria-current=page]")
      .allTextContents())
      assert.equal(text, text.toUpperCase());
    assert.equal(await page.getByRole("dialog").count(), 0, "entry uses a screen");
    assert.ok((await page.getByRole("table").count()) <= 1);
    if (table) {
      const panel = page.locator(".platform-table-screen");
      assert.equal(
        await panel.evaluate((node) => {
          const main = node.closest(".ui-dashboard-main");
          const style = getComputedStyle(main);
          return (
            [style.paddingTop, style.paddingRight, style.paddingBottom, style.paddingLeft].every(
              (value) => value === "0px",
            ) && Math.abs(main.getBoundingClientRect().left - node.getBoundingClientRect().left) < 1
          );
        }),
        true,
        `${name} fills the main panel`,
      );
      await panel.getByRole("navigation", { name: "Table pages" }).waitFor();
      assert.ok((await panel.locator("tbody > tr[data-row]").count()) <= 25);
    }
    await page.screenshot({ path: join(fixture.reportDirectory, `${name}.png`), fullPage: false });
    captured.add(name);
  };
  const table = (name) => page.getByRole("table", { name, exact: true });
  const menu = (name) =>
    page.getByRole("navigation", { name: "Primary navigation" }).getByRole("link", { name, exact: true }).click();
  const choose = async (label, text) => {
    await page.getByRole("combobox", { name: label, exact: true }).click();
    await page.getByRole("option", { name: text, exact: true }).click();
  };
  const order = { number: undefined, path: undefined };
  const purchaseOrders = async () => {
    await menu("Purchase orders");
    await table("Purchase orders").locator("tbody > tr[data-row]").first().waitFor();
  };
  // The order the run works on: the first order of the first page.
  const openOrder = async () => {
    await purchaseOrders();
    if (!order.number) {
      const link = table("Purchase orders")
        .locator("tbody > tr[data-row]")
        .first()
        .getByRole("link", { name: /^PO-BROWSER-/ });
      order.number = await link.textContent();
    }
    await table("Purchase orders").getByRole("link", { name: order.number, exact: true }).click();
    await page.getByRole("heading", { name: "Purchase order", exact: true }).waitFor();
    order.path ??= new URL(page.url()).pathname;
  };
  const orderHistory = async () => {
    await openOrder();
    await page.getByRole("link", { name: "Purchase order history", exact: true }).click();
    await table("Purchase order history").locator("tbody > tr[data-row]").first().waitFor();
  };
  const receivingScreen = async () => {
    await orderHistory();
    await page.getByRole("navigation", { name: "Platform location" }).locator(`a[href="${order.path}"]`).click();
    await page.getByRole("link", { name: "Receiving screen", exact: true }).click();
    await table("Receiving screen").getByText("ITEM-BROWSER", { exact: true }).waitFor();
  };
  const receipts = async () => {
    await menu("Receipts");
    await table("Receipts").waitFor();
  };
  let recovering = false;
  // A step that the later steps need stops the run when it fails.
  const run = async (name, { start, body, needed = false }) => {
    try {
      if (recovering && start) await start();
      await body();
      recovering = false;
    } catch (error) {
      failures.push(`${name}: ${String(error?.message ?? error).split("\n")[0]}`);
      await page
        .screenshot({ path: join(fixture.reportDirectory, `${name}-failure.png`), fullPage: false })
        .catch((captureError) => console.error(`Failed to capture ${name}:`, captureError));
      recovering = true;
      if (needed) throw new Error(`${name} failed, and the later steps need it`);
    }
  };
  await run("sign-in", {
    needed: true,
    body: async () => {
      await page.goto(required("WAMN_BROWSER_URL"));
      await page.getByLabel("Email", { exact: true }).fill(required("WAMN_BROWSER_EMAIL"));
      await page.getByLabel("Password", { exact: true }).fill(required("WAMN_BROWSER_PASSWORD"));
      await capture("sign-in");
      await page.getByRole("button", { name: "Sign in", exact: true }).click();
    },
  });
  await run("audiences", {
    needed: true,
    body: async () => {
      await table("Audiences").getByRole("link", { name: "dev", exact: true }).waitFor();
      await capture("audiences", true);
      await table("Audiences").getByRole("link", { name: "dev", exact: true }).click();
      await table("Purchase orders").locator("tbody > tr[data-row]").first().waitFor();
    },
  });
  await run("purchase-orders", {
    start: purchaseOrders,
    body: async () => {
      await capture("purchase-orders", true);
      const firstPage = await table("Purchase orders").locator("tbody > tr[data-row]").allTextContents();
      assert.equal(firstPage.length, 25);
      await page.getByRole("button", { name: "Next page", exact: true }).click();
      await page
        .getByRole("status")
        .filter({ hasText: /^Page 2$/ })
        .waitFor();
      assert.equal(await table("Purchase orders").locator("tbody > tr[data-row]").count(), 1);
      await page.getByRole("button", { name: "Previous page", exact: true }).click();
      await page
        .getByRole("status")
        .filter({ hasText: /^Page 1$/ })
        .waitFor();
      assert.deepEqual(await table("Purchase orders").locator("tbody > tr[data-row]").allTextContents(), firstPage);
    },
  });
  await run("purchase-order", {
    start: purchaseOrders,
    body: async () => {
      const orderRow = table("Purchase orders").locator("tbody > tr[data-row]").first();
      const orderLink = orderRow.getByRole("link", { name: /^PO-BROWSER-/ });
      order.number = await orderLink.textContent();
      const orderNumber = order.number;
      await orderRow.getByText("SUPPLIER-BROWSER", { exact: true }).waitFor();
      await columns("Purchase orders", ["Order number", "Supplier", "Status", "Revision", "Actions"]);
      assert.equal(
        await table("Purchase orders")
          .getByRole("columnheader", { name: /^Purchase order Drag/ })
          .count(),
        0,
      );
      assert.equal(await orderRow.getByRole("button", { name: "Change the supplier", exact: true }).count(), 0);
      await orderRow.getByRole("button", { name: "Record a receipt", exact: true }).click();
      await page.getByRole("combobox", { name: "Purchase order", exact: true }).waitFor();
      await page.waitForFunction(
        (number) => document.querySelector('input[role="combobox"]')?.value === number,
        orderNumber,
      );
      await page.getByRole("button", { name: "Cancel", exact: true }).click();
      await menu("Purchase orders");
      await table("Purchase orders").getByRole("link", { name: orderNumber, exact: true }).click();
      await page.getByRole("heading", { name: "Purchase order", exact: true }).waitFor();
      await page
        .locator('[data-generator-field="supplier_id"]')
        .getByText("SUPPLIER-BROWSER", { exact: true })
        .waitFor();
      await capture("purchase-order");
      await page.getByRole("button", { name: "Record a receipt", exact: true }).click();
      await page.getByRole("combobox", { name: "Purchase order", exact: true }).waitFor();
      await page.waitForFunction(
        (number) => document.querySelector('input[role="combobox"]')?.value === number,
        orderNumber,
      );
      await page.getByRole("button", { name: "Cancel", exact: true }).click();
      await menu("Purchase orders");
      await table("Purchase orders").getByRole("link", { name: orderNumber, exact: true }).click();
      await page.getByRole("heading", { name: "Purchase order", exact: true }).waitFor();
      order.path = new URL(page.url()).pathname;
    },
  });
  await run("change-supplier", {
    start: openOrder,
    body: async () => {
      await page.getByRole("button", { name: "Change the supplier", exact: true }).click();
      await page.getByRole("button", { name: "Change the supplier", exact: true }).waitFor();
      await page.waitForFunction(
        () => document.querySelector('input[aria-label="Supplier"]')?.value === "SUPPLIER-BROWSER",
      );
      await capture("change-supplier");
      await choose("Supplier", "SUPPLIER-BROWSER");
      await page.getByRole("button", { name: "Change the supplier", exact: true }).click();
      await table("Purchase orders").waitFor();
      assert.ok(new URL(page.url()).pathname.endsWith("/purchase-orders"));
    },
  });
  await run("purchase-order-history", {
    start: async () => {
      if (!order.path) await openOrder();
      await purchaseOrders();
    },
    body: async () => {
      await table("Purchase orders").locator(`a[href="${order.path}"]`).click();
      await page.getByRole("link", { name: "Purchase order history", exact: true }).click();
      await table("Purchase order history").locator("tbody > tr[data-row]").first().waitFor();
      await capture("purchase-order-history", true);
    },
  });
  await run("receiving", {
    start: orderHistory,
    body: async () => {
      await page.getByRole("navigation", { name: "Platform location" }).locator(`a[href="${order.path}"]`).click();
      await page.getByRole("link", { name: "Receiving screen", exact: true }).click();
      await table("Receiving screen").getByText("ITEM-BROWSER", { exact: true }).waitFor();
      await capture("receiving", true);
    },
  });
  await run("record-receipt", {
    start: receivingScreen,
    body: async () => {
      // The receiving screen is a sync screen: its row action opens a receipt panel for that line.
      await table("Receiving screen").getByRole("button", { name: "Record a receipt", exact: true }).click();
      const receiptPanel = page.getByRole("form", { name: "Record a receipt", exact: true });
      await receiptPanel.getByRole("heading", { name: /ITEM-BROWSER$/ }).waitFor();
      await receiptPanel
        .getByRole("combobox", { name: "Location", exact: true })
        .selectOption({ label: "DOCK-BROWSER" });
      await receiptPanel.getByLabel("Receipt reference", { exact: true }).fill("BROWSER-RECEIPT");
      await receiptPanel.getByLabel("Quantity", { exact: true }).fill("1");
      await capture("record-receipt");
      await receiptPanel.getByRole("button", { name: "Record a receipt", exact: true }).click();
      await receiptPanel.waitFor({ state: "detached" });
      await table("Receiving screen").locator('[data-tentative="true"]').first().waitFor({ state: "detached" });
      await page
        .getByLabel("Sync status", { exact: true })
        .filter({ hasText: /Pending: 0\. Refused: 0\./ })
        .waitFor();
      await menu("Receipts");
      await table("Receipts").getByText("BROWSER-RECEIPT", { exact: true }).waitFor();
      assert.ok(new URL(page.url()).pathname.endsWith("/receipts"));
    },
  });
  await run("receipts", {
    start: receipts,
    body: async () => {
      const createReceipt = page.getByRole("button", { name: "Record a receipt", exact: true });
      assert.ok(
        await createReceipt.evaluate((node) => !!node.closest(".table-workspace-header")),
        "receipt action belongs in the table header",
      );
      await columns("Receipts", ["Receipt reference", "Purchase order", "Received at"]);
      await capture("receipts", true);
    },
  });
  await run("receipt", {
    start: receipts,
    body: async () => {
      await table("Receipts").getByText(order.number, { exact: true }).waitFor();
      await table("Receipts").getByRole("link", { name: "BROWSER-RECEIPT", exact: true }).click();
      await page.getByRole("heading", { name: "Receipt", exact: true }).waitFor();
      await page
        .locator('[data-generator-field="purchase_order_id"]')
        .getByText(order.number, { exact: true })
        .waitFor();
      await capture("receipt");
    },
  });
  await run("create-supplier", {
    body: async () => {
      await menu("Suppliers");
      await table("Suppliers").getByText("SUPPLIER-BROWSER", { exact: true }).waitFor();
      const createSupplier = page.getByRole("button", { name: "Add a supplier", exact: true });
      assert.ok(
        await createSupplier.evaluate((node) => !!node.closest(".table-workspace-header")),
        "supplier action belongs in the table header",
      );
      await createSupplier.click();
      await page.getByLabel("Supplier name", { exact: true }).fill("BROWSER-NEW-SUPPLIER");
      await capture("create-supplier");
      await page.getByRole("button", { name: "Add a supplier", exact: true }).click();
      await table("Suppliers").getByText("BROWSER-NEW-SUPPLIER", { exact: true }).waitFor();
      assert.ok(new URL(page.url()).pathname.endsWith("/suppliers"));
    },
  });
  await run("suppliers", {
    start: async () => {
      await menu("Suppliers");
      await table("Suppliers").waitFor();
    },
    body: async () => {
      await columns("Suppliers", ["Supplier name"]);
      await capture("suppliers", true);
    },
  });
  await run("locations", {
    body: async () => {
      await menu("Locations");
      await table("Locations").getByText("DOCK-BROWSER", { exact: true }).waitFor();
      await columns("Locations", ["Location code", "Actions"]);
      await capture("locations", true);
    },
  });
  await run("cancel-receipt", {
    start: async () => {
      await menu("Locations");
      await table("Locations").getByText("DOCK-BROWSER", { exact: true }).waitFor();
    },
    body: async () => {
      await table("Locations").getByRole("button", { name: "Record a receipt", exact: true }).click();
      await page.getByRole("button", { name: "Cancel", exact: true }).click();
      await table("Receipts").waitFor();
    },
  });
  if (errors.length) failures.push(`page errors: ${errors.join(" | ")}`);
  if (!failures.length) assert.equal(captured.size, 13);
} catch (error) {
  if (!failures.some((failure) => error?.message?.startsWith(failure.split(":")[0])))
    failures.push(`run: ${String(error?.message ?? error).split("\n")[0]}`);
} finally {
  await browser.close();
}
if (failures.length) {
  for (const failure of failures) console.error(`FAIL ${failure}`);
  throw new Error(`${failures.length} Receiving browser steps failed`);
}
console.log("PASS: Receiving screens, writes, pagination and navigation");
