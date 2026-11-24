import { expect, test, type Page } from "@playwright/test";

const shot = (page: Page, name: string) => page.screenshot({ path: `tests/screenshots/${name}.png` });

test.describe.configure({ mode: "serial" });

test("primary loop against the mock backend", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "dark" });
  await page.goto("/");

  const sidebar = page.getByRole("navigation", { name: "事项" });
  await expect(sidebar.getByText("自选股分组支持拖拽排序")).toBeVisible();
  // 已归档 is folded by default.
  await expect(sidebar.getByText("旧版分时图下线")).toHaveCount(0);

  // --- running item: transcript streams and groups collapse/expand ---
  await sidebar.getByText("自选股分组支持拖拽排序").click();
  await expect(page.locator(".work-header h1")).toHaveText("自选股分组支持拖拽排序");
  await expect(page.locator(".step-running")).toHaveCount(1);
  const lastTurn = page.locator(".turn").last();
  await expect(lastTurn.locator(".turn-status.live")).toBeVisible();
  const group = lastTurn.locator(".activity").first();
  await expect(group).toBeVisible();
  await page.waitForTimeout(1500);
  await shot(page, "01-running-stage");

  await expect(group.locator(".activity-list")).toHaveCount(0);
  await group.locator(".activity-head").click();
  await expect(group.locator(".activity-list")).toBeVisible();
  const cmd = group.locator(".act-cmd:not([disabled])").first();
  await cmd.click();
  await expect(group.locator(".act-output").first()).toBeVisible();
  await shot(page, "02-activity-expanded");
  await group.locator(".activity-head").click();
  await expect(group.locator(".activity-list")).toHaveCount(0);

  // --- wait for the run to finish, then decide or keep chatting ---
  await expect(lastTurn.locator(".turn-status.ok")).toBeVisible({ timeout: 45_000 });
  await expect(page.getByRole("button", { name: /确认，进入「测试验证」/ })).toBeVisible();
  await shot(page, "03-stage-succeeded");

  const composer = page.getByRole("textbox", { name: "继续对话" });
  await composer.fill("T3 的接口字段已确认：groups[].order，请继续完成持久化。");
  await composer.press("ControlOrMeta+Enter");
  await expect(page.locator(".turn .bubble").last()).toContainText("T3 的接口字段已确认");
  await expect(page.locator(".turn").last().locator(".turn-status.live")).toBeVisible();
  await page.waitForTimeout(2500);
  await shot(page, "04-follow-up");

  // --- viewing an earlier stage ---
  await page.locator(".step").filter({ hasText: "技术方案" }).locator("button").click();
  await expect(page.locator(".view-banner")).toContainText("正在查看已完成的「技术方案」");
  await expect(page.locator(".composer")).toHaveCount(0);
  await shot(page, "05-viewing-done-stage");
  await page.locator(".view-banner").getByRole("button").click();
  await expect(page.locator(".view-banner")).toHaveCount(0);

  // --- autopilot drawer ---
  await expect(page.locator(".turn").last().locator(".turn-status.ok")).toBeVisible({ timeout: 20_000 });
  await page.getByRole("button", { name: "自动运行…" }).click();
  const drawer = page.getByRole("dialog", { name: "自动运行" });
  await expect(drawer).toBeVisible();
  await expect(drawer.getByRole("button", { name: /自动运行到「交付归档」/ })).toBeVisible();
  await drawer.locator(".ap-row").filter({ hasText: "测试验证" }).locator(".ap-until").click();
  await expect(drawer.getByRole("button", { name: /自动运行到「测试验证」/ })).toBeVisible();
  await page.waitForTimeout(300);
  await shot(page, "06-autopilot-drawer");
  await page.keyboard.press("Escape");
  await expect(drawer).toHaveCount(0);

  // --- failed gate: send back ---
  await sidebar.getByText("K 线图叠加 MACD / KDJ 指标").click();
  await expect(page.getByRole("button", { name: "退回「开发实现」修复" })).toBeVisible();
  await expect(page.locator(".turn-routeback")).toContainText("需要「开发实现」先解决");
  await shot(page, "07-failed-gate");

  // --- a failed non-gate stage with route_back also offers send-back ---
  await sidebar.getByText("自选股显示港股通标识").click();
  await expect(page.getByRole("button", { name: "退回「开发实现」修复" })).toBeVisible();

  // --- autopilot running item ---
  await sidebar.getByText("盘前盘后行情推送").click();
  await expect(page.getByTestId("autopilot-live")).toBeVisible();
  await page.waitForTimeout(1200);
  await shot(page, "08-autopilot-running");

  // --- fresh item ---
  await sidebar.getByText("港股暗盘报价").click();
  await expect(page.getByRole("button", { name: "运行「产品需求」" })).toBeVisible();
  await shot(page, "09-fresh-item");

  // --- delivered item: merge + archive ---
  await sidebar.getByText("行情快照导出 CSV").click();
  await expect(page.getByTestId("delivery")).toBeVisible();
  await expect(page.getByTestId("archive")).toBeVisible();
  await expect(page.getByRole("button", { name: "合并到 main" })).toBeVisible();
  await page.getByRole("button", { name: "查看完整差异" }).click();
  await expect(page.locator(".diff .d-add").first()).toBeVisible();
  await shot(page, "10-delivered");
  await page.getByRole("button", { name: "合并到 main" }).click();
  await expect(page.locator(".merged-line")).toContainText("已合并到 main");
  await page.getByRole("combobox", { name: "目标分支" }).selectOption("release/2.3");
  await expect(page.locator(".notice-box")).toContainText("已分叉");
  await shot(page, "11-merge-diverged");

  // --- new work dialog ---
  await page.getByRole("button", { name: "新建事项" }).click();
  const dialog = page.getByRole("dialog", { name: "新建事项" });
  await expect(dialog).toBeVisible();
  await dialog.getByRole("textbox", { name: "事项名称" }).fill("分时图支持叠加对比");
  await page.waitForTimeout(300);
  await shot(page, "12-new-work");
  await dialog.getByRole("button", { name: "创建事项" }).click();
  await expect(page.locator(".work-header h1")).toHaveText("分时图支持叠加对比");

  // --- memory page ---
  await page.getByRole("button", { name: "项目记忆" }).click();
  await expect(page.getByRole("heading", { name: "项目记忆" })).toBeVisible();
  // Replaced versions sit under their note's history, not in the list.
  await expect(page.locator(".memory.superseded")).toHaveCount(0);
  await expect(page.getByTestId("memory-note").filter({ hasText: "zustand" })).toContainText("第 2 版");
  await shot(page, "13-memory");

  // --- repository memory ---
  await page.locator(".foot-link").filter({ hasText: "仓库" }).click();
  await expect(page.getByRole("heading", { name: "仓库" })).toBeVisible();
  const repo = page.locator(".repo").first();
  await repo.locator(".repo-toggle").click();
  await expect(repo.locator(".memory.superseded")).toHaveCount(1);
  await repo.getByRole("textbox", { name: "新增仓库记忆" }).fill("行情快照导出统一使用 src/features/export 下的 toCsv。");
  await repo.getByRole("button", { name: "添加" }).click();
  await expect(repo.locator(".memory").first()).toContainText("toCsv");
  await page.waitForTimeout(200);
  await shot(page, "17-repo-memory");

  // --- history tab + hand-offs ---
  await sidebar.getByText("自选股分组支持拖拽排序").click();
  const historyTab = page.getByRole("tab", { name: /历史/ });
  await expect(historyTab.locator(".tab-badge")).toHaveText("1");
  await historyTab.click();
  const handoffCard = page.getByTestId("handoffs");
  await expect(handoffCard).toContainText("来自「K 线图叠加 MACD / KDJ 指标」");
  await expect(page.locator(".tl-item").first()).toBeVisible();
  await page.locator(".tl-item").nth(2).locator(".tl-head").click();
  await expect(page.locator(".tl-item").nth(2).locator(".note-card").first()).toBeVisible();
  await expect(page.locator(".received")).toBeVisible();
  await page.locator(".history-errors").scrollIntoViewIfNeeded();
  await page.locator(".history-scroll").evaluate((el) => (el.scrollTop = 0));
  await shot(page, "15-history");
  await handoffCard.getByRole("combobox", { name: "交接给" }).selectOption({ label: "K 线图叠加 MACD / KDJ 指标 · 测试验证" });
  await handoffCard.getByRole("textbox", { name: "交接内容" }).fill("reorderGroups 已稳定，签名保持 (fromIndex, toIndex)。");
  await handoffCard.getByRole("button", { name: "发送交接" }).click();
  await expect(page.locator(".toast")).toContainText(["交接已发送"]);
  await sidebar.getByText("K 线图叠加 MACD / KDJ 指标").click();
  await expect(page.getByRole("tab", { name: /历史/ }).locator(".tab-badge")).toHaveText("1");

  // --- ZCode models ---
  await sidebar.getByText("港股暗盘报价").click();
  await page.getByRole("tab", { name: /阶段/ }).click();
  await page.getByRole("radio", { name: "ZCode" }).click();
  await expect(page.getByText("正在读取 ZCode 模型…")).toBeVisible();
  const glm = page.getByRole("button", { name: "BigModel · GLM-5.3", exact: true });
  await expect(glm).toBeVisible();
  await glm.click();
  await expect(glm).toHaveAttribute("aria-pressed", "true");
  await expect(page.locator(".step").first()).toContainText("bigmodel/glm-5.3@high");
  await shot(page, "16-zcode-models");

  // Info toasts dismiss themselves.
  await expect(page.locator(".toast-info")).toHaveCount(0, { timeout: 8000 });

  // --- light theme ---
  await page.emulateMedia({ colorScheme: "light" });
  await sidebar.getByText("K 线图叠加 MACD / KDJ 指标").click();
  await expect(page.getByRole("button", { name: "退回「开发实现」修复" })).toBeVisible();
  await shot(page, "14-light-failed-gate");
});

test("an archived item cleans up its worktree and merged branch", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "dark" });
  await page.goto("/");
  const sidebar = page.getByRole("navigation", { name: "事项" });
  await sidebar.getByRole("button", { name: /已归档/ }).click();
  await sidebar.getByText("旧版分时图下线").click();
  const cleanup = page.getByTestId("cleanup");
  await expect(cleanup).toContainText("aep-stage/0d1e2f3a");
  await expect(cleanup).toContainText("已合并");
  await shot(page, "18-archived-cleanup");
  await cleanup.getByRole("button", { name: "清理工作区和已合并分支" }).click();
  await expect(page.locator(".toast")).toContainText("删除分支 aep-stage/0d1e2f3a");
  await expect(page.getByTestId("cleanup")).toHaveCount(0);
});

test("a ZCode stage waiting for a tool approval says so and opens the task", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "dark" });
  await page.goto("/");
  const sidebar = page.getByRole("navigation", { name: "事项" });
  await sidebar.getByText("港股暗盘报价").click();
  await page.getByRole("radio", { name: "ZCode" }).click();
  await page.getByRole("button", { name: "运行「产品需求」" }).click();
  const task = page.getByTestId("zcode-task");
  await expect(task).toContainText("ZCode 正在等待工具审批");
  await expect(task).toContainText("cargo test --locked");
  await shot(page, "19-zcode-approval");
  await task.getByRole("button", { name: /在 ZCode 中打开这个任务/ }).click();
  await expect(page.locator(".toast-error")).toHaveCount(0);
});

test("an item that never ran can be deleted", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "dark" });
  await page.goto("/");
  const sidebar = page.getByRole("navigation", { name: "事项" });
  await sidebar.getByText("港股暗盘报价").click();
  page.once("dialog", (d) => d.accept());
  await page.getByRole("button", { name: "删除事项" }).click();
  await expect(sidebar.getByText("港股暗盘报价")).toHaveCount(0);
  // Items with runs offer no delete.
  await sidebar.getByText("自选股分组支持拖拽排序").click();
  await expect(page.getByRole("button", { name: "删除事项" })).toHaveCount(0);
});

test("a new project's repository can be picked from a folder", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "light" });
  await page.goto("/");
  await page.locator(".project-btn").click();
  await page.getByText("新建项目").click();
  await page.getByRole("button", { name: "选择文件夹…" }).click();
  await expect(page.getByLabel("仓库路径")).toHaveValue(/\/code\/picked-1$/);
  await expect(page.getByLabel("项目名称")).toHaveValue("picked-1");

  // What the coding agents already know about the folder can be imported.
  const imp = page.getByTestId("import");
  await expect(imp).toContainText("Claude 记忆 3/3 条");
  await expect(imp).toContainText("会话记录：Claude 19 · Codex 3 · ZCode 38");
  await imp.getByText("android-build").click(); // deselect one memory
  await expect(imp).toContainText("Claude 记忆 2/3 条");
  await imp.getByText("fix/1448").click(); // import one branch as an item
  await expect(imp.getByRole("checkbox").last()).toBeDisabled(); // detached HEAD
  await shot(page, "20-new-project-folder");
  await page.getByRole("button", { name: "创建项目" }).click();
  await expect(page.locator(".toast", { hasText: "已导入" })).toContainText("已导入 2 条记忆、1 个事项");
  await expect(page.getByRole("navigation", { name: "事项" }).getByText("fix/1448")).toBeVisible();
});

test("an existing project imports from its repository page, and can be removed", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "dark" });
  await page.goto("/");
  await page.getByRole("button", { name: /^仓库/ }).click();
  await page.getByRole("button", { name: "导入 / 同步" }).first().click();
  const dialog = page.getByRole("dialog", { name: "导入与同步" });
  await dialog.getByRole("radio", { name: "挑选导入" }).click();
  await expect(dialog.getByTestId("import")).toContainText("Claude 记忆 3/3 条");
  await dialog.getByText("chore/code-structure-review").click();
  await shot(page, "21-import-existing");
  await dialog.getByRole("button", { name: "导入所选" }).click();
  await expect(page.locator(".toast", { hasText: "已导入" })).toContainText("已导入 3 条记忆、1 个事项");
  await expect(page.getByRole("navigation", { name: "事项" }).getByText("chore/code-structure-review")).toBeVisible();

  // Importing the same branch again does not add a copy.
  await page.getByRole("button", { name: /^仓库/ }).click();
  await page.getByRole("button", { name: "导入 / 同步" }).first().click();
  await dialog.getByRole("radio", { name: "挑选导入" }).click();
  await dialog.getByText("chore/code-structure-review").click();
  await dialog.getByRole("button", { name: "导入所选" }).click();
  await expect(page.getByRole("navigation", { name: "事项" }).getByText("chore/code-structure-review")).toHaveCount(1);

  // Sync previews what it changes, then makes the project match its sources.
  await page.getByRole("button", { name: "导入 / 同步" }).first().click();
  const sync = dialog.getByTestId("sync");
  await expect(sync).toContainText("新增 1 · 更新 1 · 移出 1 · 未变 1");
  await expect(sync).toContainText("− old-deploy");
  await expect(sync).toContainText("新增 0 · 移除 0"); // the branch imported above is not added again
  await shot(page, "24-sync");
  await dialog.getByRole("button", { name: "同步" }).click();
  await expect(page.locator(".toast", { hasText: "已同步" })).toBeVisible();

  await page.locator(".project-btn").click();
  page.once("dialog", (d) => d.accept());
  await page.getByRole("menuitem", { name: /移除项目「行情终端」/ }).click();
  await expect(page.locator(".project-name")).not.toHaveText("行情终端");
});

test("project memory keeps versions: edit, see what changed, restore", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "dark" });
  await page.goto("/");
  await page.getByRole("button", { name: /^项目记忆/ }).click();
  const note = page.getByTestId("memory-note").filter({ hasText: "zustand" });
  await expect(note).toContainText("第 2 版");
  await expect(page.getByText("前端状态管理使用 zustand（替代旧的 redux store）。")).toHaveCount(0); // old version hidden

  await note.getByRole("button", { name: /编辑/ }).click();
  await note.getByLabel("编辑记忆").fill("前端状态使用 zustand，跨组件共享通过 store 切片完成。");
  await note.getByRole("button", { name: "保存为新版本" }).click();
  const latest = page.getByTestId("memory-note").filter({ hasText: "zustand" });
  await expect(latest).toContainText("第 3 版");
  await latest.getByRole("button", { name: /历史 2/ }).click();
  await expect(latest.locator(".diff-del").first()).toContainText("禁止新增全局单例");
  await shot(page, "22-memory-versions");
  await latest.getByRole("button", { name: "恢复此版本" }).first().click();
  await expect(page.getByTestId("memory-note").filter({ hasText: "zustand" })).toContainText("禁止新增全局单例");
  await expect(page.getByTestId("memory-note").filter({ hasText: "zustand" })).toContainText("第 4 版");
});

test("the branch page draws branches, their worktrees and items, and opens git tools", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "dark" });
  await page.goto("/");
  await page.getByRole("button", { name: /^分支/ }).click();
  const list = page.getByRole("complementary", { name: "分支列表" });
  await expect(list).toContainText("事项：自选股分组支持拖拽排序");
  await expect(list).toContainText("Claude 工作区 · 0 改动 · 74 未跟踪");
  await expect(page.locator(".graph-svg circle")).toHaveCount(10);
  await list.getByText("feat/export-statement").click();
  await expect(page.locator(".graph-row.on")).toContainText("导出对账单：补齐表头");
  await shot(page, "23-branches");
  await page.getByRole("button", { name: "Fork" }).click();
  await expect(page.locator(".toast-error")).toHaveCount(0);
  await list.getByText("fix/snapshot-paging").click();
  await expect(page.getByText("这个分支没有检出到工作区")).toBeVisible();
});

test("memory is tidied by a CLI: review the proposal, apply what you accept", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "dark" });
  await page.goto("/");
  await page.getByRole("button", { name: /^项目记忆/ }).click();
  const tidy = page.getByTestId("tidy");
  await tidy.getByRole("button", { name: "开始整理" }).click();
  await tidy.getByRole("radio", { name: "Codex" }).click();
  await tidy.getByLabel("整理要求").fill("前端约定合成一条");
  await tidy.getByRole("button", { name: "开始整理" }).click();
  await expect(tidy).toContainText("Codex 正在整理");
  await expect(tidy).toContainText("合并两条前端约定", { timeout: 10_000 });
  await expect(tidy.getByRole("button", { name: "应用所选（2）" })).toBeVisible();
  await shot(page, "25-memory-tidy");
  await tidy.getByRole("checkbox").nth(1).uncheck(); // keep the delivery note
  await tidy.getByRole("button", { name: "应用所选（1）" }).click();
  await expect(page.locator(".toast", { hasText: "已应用 1 项整理" })).toBeVisible();
  await expect(page.getByTestId("memory-note").filter({ hasText: "前端约定：行情只走" })).toHaveCount(1);
  await expect(page.getByTestId("memory-note").filter({ hasText: "旧版分时图下线" })).toHaveCount(1);
});
