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
let page;
let step = "sign-in";
try {
  page = await browser.newPage({ viewport: { width: 1400, height: 900 } });
  page.on("pageerror", (error) => errors.push(error.message));
  const columns = async (title, labels) => {
    const headers = page.getByRole("table", { name: title, exact: true }).getByRole("columnheader");
    assert.equal(await headers.count(), labels.length);
    for (const label of labels)
      assert.equal(await headers.filter({ hasText: new RegExp(`^${label}(?:\\s|$)`) }).count(), 1);
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
  await page.goto(required("WAMN_BROWSER_URL"));
  await page.getByLabel("Email", { exact: true }).fill(required("WAMN_BROWSER_EMAIL"));
  await page.getByLabel("Password", { exact: true }).fill(required("WAMN_BROWSER_PASSWORD"));
  await capture("sign-in");
  await page.getByRole("button", { name: "Sign in", exact: true }).click();
  step = "audiences";
  await table("Audiences").getByRole("link", { name: "dev", exact: true }).waitFor();
  await capture("audiences", true);
  step = "purchase-orders";
  await table("Audiences").getByRole("link", { name: "dev", exact: true }).click();
  await table("Purchase orders").locator("tbody > tr[data-row]").first().waitFor();
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
  const orderRow = table("Purchase orders").locator("tbody > tr[data-row]").first();
  step = "purchase-order";
  const orderLink = orderRow.getByRole("link", { name: /^PO-BROWSER-/ });
  const orderNumber = await orderLink.textContent();
  await orderRow.getByText("SUPPLIER-BROWSER", { exact: true }).waitFor();
  await columns("Purchase orders", ["Order number", "Supplier", "Status", "Revision", "Actions"]);
  assert.equal(
    await table("Purchase orders")
      .getByRole("columnheader", { name: /^Purchase order Drag/ })
      .count(),
    0,
  );
  assert.equal(
    await table("Purchase orders")
      .getByRole("columnheader", { name: /^Actions / })
      .count(),
    1,
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
  await page.locator('[data-generator-field="supplier_id"]').getByText("SUPPLIER-BROWSER", { exact: true }).waitFor();
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
  const orderPath = new URL(page.url()).pathname;
  step = "change-supplier";
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
  step = "purchase-order-history";
  await table("Purchase orders").locator(`a[href="${orderPath}"]`).click();
  await page.getByRole("link", { name: "Purchase order history", exact: true }).click();
  await table("Purchase order history").locator("tbody > tr[data-row]").first().waitFor();
  await capture("purchase-order-history", true);
  step = "receiving";
  await page.getByRole("navigation", { name: "Platform location" }).locator(`a[href="${orderPath}"]`).click();
  await page.getByRole("link", { name: "Receiving screen", exact: true }).click();
  await table("Receiving screen").getByText("ITEM-BROWSER", { exact: true }).waitFor();
  await capture("receiving", true);
  step = "record-receipt";
  await table("Receiving screen").getByRole("button", { name: "Record a receipt", exact: true }).click();
  await page.getByRole("button", { name: "Record a receipt", exact: true }).waitFor();
  await page.waitForFunction(() => document.querySelector('input[aria-label="Order line"]')?.value === "ITEM-BROWSER");
  await choose("Location", "DOCK-BROWSER");
  await page.getByLabel("Receipt reference", { exact: true }).fill("BROWSER-RECEIPT");
  await page.getByLabel("Quantity", { exact: true }).fill("1");
  await capture("record-receipt");
  await page.getByRole("button", { name: "Record a receipt", exact: true }).click();
  await table("Receipts").getByText("BROWSER-RECEIPT", { exact: true }).waitFor();
  assert.ok(new URL(page.url()).pathname.endsWith("/receipts"));
  step = "receipts";
  const createReceipt = page.getByRole("button", { name: "Record a receipt", exact: true });
  assert.ok(
    await createReceipt.evaluate((node) => !!node.closest(".table-workspace-header")),
    "receipt action belongs in the table header",
  );
  await columns("Receipts", ["Receipt reference", "Purchase order", "Received at"]);
  await capture("receipts", true);
  step = "receipt";
  await table("Receipts").getByText(orderNumber, { exact: true }).waitFor();
  await table("Receipts").getByRole("link", { name: "BROWSER-RECEIPT", exact: true }).click();
  await page.getByRole("heading", { name: "Receipt", exact: true }).waitFor();
  await page.locator('[data-generator-field="purchase_order_id"]').getByText(orderNumber, { exact: true }).waitFor();
  await capture("receipt");
  step = "create-supplier";
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
  step = "suppliers";
  await columns("Suppliers", ["Supplier name"]);
  await capture("suppliers", true);
  step = "locations";
  await menu("Locations");
  await table("Locations").getByText("DOCK-BROWSER", { exact: true }).waitFor();
  await columns("Locations", ["Location code", "Actions"]);
  await capture("locations", true);
  step = "cancel-receipt";
  await table("Locations").getByRole("button", { name: "Record a receipt", exact: true }).click();
  await page.getByRole("button", { name: "Cancel", exact: true }).click();
  await table("Receipts").waitFor();
  assert.equal(captured.size, 13);
  assert.deepEqual(errors, []);
  console.log("PASS: Receiving screens, writes, pagination and navigation");
} catch (error) {
  if (page)
    await page
      .screenshot({ path: join(fixture.reportDirectory, `${step}-failure.png`), fullPage: false })
      .catch((captureError) => console.error(`Failed to capture ${step}:`, captureError));
  throw error;
} finally {
  await browser.close();
}
