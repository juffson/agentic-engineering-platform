import { expect, test, type Page } from "@playwright/test";

/** Every element whose content is taller than it, and that is not clipped on purpose, must scroll. */
async function stuck(page: Page): Promise<string[]> {
  return page.evaluate(() => {
    const out: string[] = [];
    const vh = window.innerHeight;
    for (const el of Array.from(document.querySelectorAll<HTMLElement>("body *"))) {
      const r = el.getBoundingClientRect();
      if (!r.height || r.bottom <= vh + 2) continue;
      // An element sticking out below the window: some ancestor must be able to scroll to it.
      let p: HTMLElement | null = el.parentElement;
      let ok = false;
      while (p) {
        const st = getComputedStyle(p);
        if (/(auto|scroll)/.test(st.overflowY) && p.scrollHeight > p.clientHeight) { ok = true; break; }
        p = p.parentElement;
      }
      if (!ok && getComputedStyle(el).visibility !== "hidden") out.push(`${el.tagName.toLowerCase()}.${el.className}`.slice(0, 80));
    }
    return [...new Set(out)].slice(0, 8);
  });
}

test("panels scroll at a short window height", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 560 });
  await page.goto("/");
  const sidebar = page.getByRole("navigation", { name: "事项" });
  const report: Record<string, string[]> = {};
  await sidebar.getByText("盘前盘后行情推送").click();
  await page.waitForTimeout(500);
  report.work = await stuck(page);
  await page.getByRole("button", { name: /自动运行/ }).first().click();
  await page.waitForTimeout(400);
  report.autopilot = await stuck(page);
  await page.keyboard.press("Escape");
  await page.getByRole("button", { name: "新建事项" }).click();
  report.newWork = await stuck(page);
  await page.getByRole("dialog", { name: "新建事项" }).getByRole("radio", { name: "用 CLI 起草" }).click();
  await page.getByLabel("需求描述").fill("对账单导出");
  await page.getByRole("button", { name: "起草" }).click();
  await page.waitForTimeout(3500);
  report.draft = await stuck(page);
  await page.keyboard.press("Escape");
  for (const name of ["项目记忆", "仓库", "分支", "Token 消耗"]) {
    await page.getByRole("button", { name }).click();
    await page.waitForTimeout(600);
    report[name] = await stuck(page);
  }
  await page.getByRole("button", { name: "仓库" }).click();
  await page.getByRole("button", { name: /导入/ }).first().click();
  await page.waitForTimeout(800);
  report.import = await stuck(page);
  console.log(JSON.stringify(report, null, 1));
  expect(Object.values(report).flat()).toEqual([]);
});
