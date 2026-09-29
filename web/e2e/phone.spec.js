// The phone layout: the view on top, and tabs for the editor, console and
// inspector panels below it.

import { expect, test } from "@playwright/test";

const shots = process.env.E2E_SHOTS;

test("panels collapse into tabs under the view", async ({ page }) => {
  await page.goto("/try/#example=sign");
  await page.waitForSelector("html[data-ready]");
  const tabs = page.locator("#mobile-tabs [role=tab]");
  await expect(tabs).toHaveText(["Editor", "Console", "Customizer", "Check", "Measure"]);
  const view = await page.locator("#viewport").boundingBox();
  const bar = await page.locator("#mobile-tabs").boundingBox();
  expect(view.y).toBeLessThan(bar.y);
  await expect(page.locator("#editor")).toBeVisible();
  await expect(page.locator("#inspector")).toBeHidden();
  if (shots) await page.screenshot({ path: `${shots}/phone-editor.png` });

  await tabs.filter({ hasText: "Customizer" }).click();
  await expect(page.getByTestId("customizer")).toBeVisible();
  await expect(page.locator("#editor")).toBeHidden();
  await expect(page.getByTestId("customizer").locator("summary").first()).toHaveText("properties of Sign");
  if (shots) await page.screenshot({ path: `${shots}/phone-customizer.png` });

  await tabs.filter({ hasText: "Console" }).click();
  await expect(page.locator("#console")).toBeVisible();
  await expect(page.getByTestId("render-summary")).toContainText("Previewed");
});

test("the agent button fits the phone's top bar, and its dialog says it needs a desktop", async ({ page }) => {
  await page.goto("/try/#example=sign");
  await page.waitForSelector("html[data-ready]");
  const button = page.getByTestId("agent-button");
  await expect(button).toBeVisible();
  const box = await button.boundingBox();
  expect(box.x + box.width).toBeLessThanOrEqual(page.viewportSize().width);
  if (shots) await page.screenshot({ path: `${shots}/phone-agent-button.png` });
  await button.click();
  const dialog = page.getByTestId("agent-dialog");
  await expect(dialog).toBeVisible();
  await expect(dialog.locator(".agent-phone")).toBeVisible();
  const d = await dialog.boundingBox();
  expect(d.width).toBeLessThanOrEqual(page.viewportSize().width + 1);
  if (shots) await page.screenshot({ path: `${shots}/phone-agent-dialog.png` });
});
