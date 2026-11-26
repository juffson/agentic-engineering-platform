import { expect, test, type Page } from "@playwright/test";

/** Screenshots land in the run's evidence folder when AEP provides one (stage hand-off),
 *  and stay under tests/screenshots for local runs. */
const evidence = process.env.AEP_EVIDENCE_DIR ?? "tests/screenshots";
const shot = (page: Page, name: string) => page.screenshot({ path: `${evidence}/${name}.png` });

test.describe.configure({ mode: "serial" });

/** Open the new-project dialog, fill one repository line, let it resolve. */
async function newProject(page: Page, path: string) {
  await page.goto("/");
  await page.locator(".project-btn").click();
  await page.getByText("新建项目").click();
  const dialog = page.getByRole("dialog", { name: "新建项目" });
  await dialog.getByLabel("项目名称").fill("行情基础数据");
  await dialog.getByLabel("仓库路径").fill(path);
  await dialog.getByLabel("仓库路径").blur();
  return dialog;
}

test("AC-1 a workspace folder lists 11 repositories, all selected, and creates the project", async ({ page }) => {
  const dialog = await newProject(page, "/mock/workspace");
  const scan = dialog.getByTestId("folder-scan");
  await expect(scan).toBeVisible();
  const rows = scan.getByTestId("folder-scan-row");
  await expect(rows).toHaveCount(11);
  // The 2nd-level repository shows its relative path.
  await expect(rows.filter({ hasText: "quote-data-service" })).toContainText("services/quote-data-service");
  await expect(scan.getByTestId("folder-scan-count")).toHaveText("已选 11/11");
  const create = dialog.getByRole("button", { name: /创建项目/ });
  await expect(create).toHaveText("创建项目（11 个仓库）");
  await expect(create).toBeEnabled();
  await shot(page, "40-workspace-folder-scan");

  await create.click();
  await expect(page.locator(".toast", { hasText: "项目已创建" })).toBeVisible();
  await expect(dialog).toHaveCount(0);
  await page.locator(".foot-link").filter({ hasText: "仓库" }).click();
  await expect(page.getByRole("heading", { name: "仓库", exact: true })).toBeVisible();
  await expect(page.locator(".repo-list li")).toHaveCount(11);
  const card = page.getByTestId("workspace-root");
  await expect(card).toBeVisible();
  await expect(card).toContainText("03-行情基础数据计算存储");
  await expect(card).toContainText("AGENTS.md");
  await shot(page, "41-workspace-project-created");
});

test("AC-2 deselecting repositories updates the count and an empty selection disables the button", async ({ page }) => {
  const dialog = await newProject(page, "/mock/workspace");
  const scan = dialog.getByTestId("folder-scan");
  await expect(scan.getByTestId("folder-scan-row")).toHaveCount(11);
  const create = dialog.getByRole("button", { name: /创建项目/ });

  // Drop two; the count follows.
  for (const name of ["genki-kline", "quote-trade"]) {
    await scan.getByRole("checkbox", { name }).uncheck();
  }
  await expect(scan.getByTestId("folder-scan-count")).toHaveText("已选 9/11");
  await expect(create).toHaveText("创建项目（9 个仓库）");
  await shot(page, "42-workspace-two-deselected");

  // Nothing selected: disabled with the reason as tooltip.
  await scan.getByRole("button", { name: "全不选" }).click();
  await expect(scan.getByTestId("folder-scan-count")).toHaveText("已选 0/11");
  await expect(create).toBeDisabled();
  await shot(page, "43-workspace-none-selected");
});

test("AC-3 empty and missing paths show the fixed texts, never git noise", async ({ page }) => {
  const dialog = await newProject(page, "/mock/empty");
  await expect(dialog.locator(".inline-error").first()).toContainText("这个文件夹和它的子目录里没有 git 仓库（已查找 2 层）");
  await expect(dialog.getByRole("button", { name: /创建项目/ })).toBeDisabled();
  await expect(dialog.getByTestId("folder-scan")).toHaveCount(0);
  await shot(page, "44-empty-folder-error");

  await dialog.getByRole("button", { name: "移除这一行" }).click();
  await expect(dialog.locator(".inline-error")).toHaveCount(0);

  await dialog.getByLabel("仓库路径").fill("/mock/missing");
  await dialog.getByLabel("仓库路径").blur();
  await expect(dialog.locator(".inline-error").first()).toContainText("路径不存在：/mock/missing");
  await expect(dialog.getByRole("button", { name: /创建项目/ })).toBeDisabled();
  await expect(dialog.getByTestId("folder-scan")).toHaveCount(0);
  // No raw git output anywhere on the page (AC-3).
  await expect(page.getByText(/fatal|rev-parse/)).toHaveCount(0);
  await shot(page, "45-missing-path-error");
});

test("AC-4 a plain repository shows no checklist and keeps the import section", async ({ page }) => {
  const dialog = await newProject(page, "/Users/demo/code/picked-1");
  await expect(dialog.getByTestId("import").first()).toBeVisible();
  await expect(dialog.getByTestId("folder-scan")).toHaveCount(0);
  await expect(dialog.getByRole("button", { name: "创建项目", exact: true })).toBeEnabled();
  await shot(page, "46-plain-repository-unchanged");
});

test("「再查一层」 goes deeper and keeps what was deselected", async ({ page }) => {
  const dialog = await newProject(page, "/mock/workspace");
  const scan = dialog.getByTestId("folder-scan");
  await expect(scan.getByTestId("folder-scan-row")).toHaveCount(11);
  await scan.getByRole("checkbox", { name: "genki-kline" }).uncheck();
  await scan.getByRole("button", { name: "再查一层" }).click();
  await expect(scan).toContainText("只查找了 3 层子目录");
  await expect(scan.getByTestId("folder-scan-row")).toHaveCount(11);
  await expect(scan.getByRole("checkbox", { name: "genki-kline" })).not.toBeChecked();
  await expect(scan.getByTestId("folder-scan-count")).toHaveText("已选 10/11");
  await shot(page, "47-scan-deeper-keeps-deselections");
});

test("AC-5 the repositories page adds from a workspace, marking what is already there", async ({ page }) => {
  await page.goto("/");
  await page.locator(".project-btn").click();
  await page.getByRole("menuitem", { name: /内部工具/ }).click();
  await expect(page.locator(".project-name")).toHaveText("内部工具");
  await page.locator(".foot-link").filter({ hasText: "仓库" }).click();
  await expect(page.getByRole("heading", { name: "仓库", exact: true })).toBeVisible();

  const card = page.getByTestId("workspace-root");
  await expect(card).toContainText("03-行情基础数据计算存储");
  await expect(card).toContainText("说明文件：AGENTS.md · 各阶段开始前会读取");

  await page.getByLabel("仓库路径").fill("/mock/workspace");
  await page.getByRole("button", { name: "添加仓库" }).click();
  const scan = page.getByTestId("folder-scan");
  await expect(scan).toBeVisible();
  const rows = scan.getByTestId("folder-scan-row");
  await expect(rows).toHaveCount(11);
  const added = rows.filter({ has: page.getByText("已添加", { exact: true }) });
  await expect(added).toHaveCount(2);
  for (const box of await added.getByRole("checkbox").all()) {
    await expect(box).toBeDisabled();
    await expect(box).toBeChecked();
  }
  const add = page.getByRole("button", { name: /添加所选/ });
  await expect(add).toHaveText("添加所选（9）");
  await shot(page, "48-repos-page-add-list");

  await add.click();
  await expect(page.locator(".toast", { hasText: "已添加 9 个仓库" })).toBeVisible();
  await expect(page.locator(".repo-list li")).toHaveCount(11);
  // No duplicates: the repo that was already present appears exactly once.
  await expect(page.getByText("/Users/demo/Desktop/cowork/market-data/03-行情基础数据计算存储/genki", { exact: true })).toHaveCount(1);
  await shot(page, "49-repos-page-added");
});
