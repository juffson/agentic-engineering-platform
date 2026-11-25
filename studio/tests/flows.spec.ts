import { readFileSync } from "node:fs";
import { expect, test, type Locator, type Page } from "@playwright/test";
import type { Api, FlowInput } from "../src/api/types";
import { validateFlow, type FlowIssue } from "../src/lib/flow";

const cases: {
  name: string;
  flow: FlowInput;
  taken_labels: string[];
  errors: FlowIssue[];
  warnings: FlowIssue[];
}[] = JSON.parse(readFileSync(new URL("../../tests/fixtures/flow_cases.json", import.meta.url), "utf8"));

const flowPage = (page: Page) => page.getByRole("button", { name: "流程", exact: true });
const newWork = (page: Page) => page.getByRole("navigation", { name: "事项" }).getByRole("button", { name: "新建事项", exact: true });
const flowRow = (page: Page, name: string) => page.locator(".flow-list-row").filter({
  has: page.getByText(name, { exact: true }),
});
const stages = (page: Page) => page.getByRole("navigation", { name: "阶段", exact: true });
const stageNames = (drawer: Locator) => drawer.locator(".flow-stage-toggle strong");
const warning = cases.find(({ name }) => name.startsWith("E7"))!.warnings[0].message;

test("Studio matches the backend's shared E1–E7 cases", () => {
  for (const fixture of cases) {
    expect(validateFlow(fixture.flow, fixture.taken_labels), fixture.name).toEqual({
      errors: fixture.errors,
      warnings: fixture.warnings,
    });
  }
});

test("P2–P5 copy, edit, select and delete a flow while its work keeps the original stages", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "行情终端" }).click();
  await page.getByRole("menuitem", { name: /内部工具/ }).click();
  await expect(page.getByRole("button", { name: "内部工具" })).toBeVisible();
  await newWork(page).click();
  const form = page.getByRole("dialog", { name: "新建事项" });
  await form.getByRole("textbox", { name: "事项名称" }).fill("导航时关闭的新建事项");
  await form.getByRole("button", { name: "定制流程…" }).click();
  await expect(form).toHaveCount(0);
  await expect(page.getByRole("heading", { name: "流程", exact: true })).toBeVisible();
  await expect(page.getByText("还没有自定义流程。", { exact: false })).toBeVisible();

  await flowRow(page, "软件开发").getByRole("button", { name: "复制" }).click();
  const copy = page.getByRole("dialog", { name: "新建流程" });
  const flowName = copy.getByRole("textbox", { name: "流程名称" });
  await expect(flowName).toHaveValue("软件开发 副本");
  await flowName.fill("后端开发");
  await copy.getByRole("button", { name: "删除阶段「交互设计」" }).click();
  await copy.getByRole("button", { name: "添加阶段" }).click();
  const name = copy.getByRole("textbox", { name: "阶段名称" });
  await expect(name).toBeFocused();
  await expect(stageNames(copy)).toHaveText(["产品需求", "技术方案", "开发实现", "测试验证", "独立验收", "未命名阶段", "交付归档"]);
  await name.fill("安全：审查");
  await expect(copy.getByText(/第 6 个阶段：不能包含/)).toBeVisible();
  await expect(copy.getByRole("button", { name: "保存", exact: true })).toBeDisabled();
  await copy.getByRole("button", { name: "1 个错误，修正后才能保存", exact: true }).click();
  await expect(name).toBeFocused();
  await name.fill("安全审查");
  await copy.getByRole("radio", { name: "测试", exact: true }).click();
  const up = copy.getByRole("button", { name: "上移「安全审查」" });
  await up.click();
  await expect(up).toBeFocused();
  await expect(name).toBeVisible();
  await expect(stageNames(copy)).toHaveText(["产品需求", "技术方案", "开发实现", "测试验证", "安全审查", "独立验收", "交付归档"]);
  await copy.getByRole("button", { name: "保存", exact: true }).click();

  const custom = flowRow(page, "后端开发");
  await expect(copy).toHaveCount(0);
  await expect(custom).toContainText("7 个阶段");
  await expect(flowPage(page)).toHaveAttribute("aria-current", "page");
  await expect(flowPage(page).locator(".foot-count")).toHaveText("1");

  await newWork(page).click();
  await expect(form.getByRole("textbox", { name: "事项名称" })).toHaveValue("");
  const choice = form.getByRole("combobox", { name: "流程" });
  await choice.selectOption({ label: "后端开发" });
  await expect(form.getByRole("list", { name: "阶段预览" }).getByRole("listitem")).toHaveCount(7);
  await expect(form.locator(".template-preview li").filter({ hasText: "安全审查" })).toContainText("测试");
  await form.getByRole("textbox", { name: "事项名称" }).fill("新流程事项");
  await form.getByRole("button", { name: "创建事项" }).click();
  await expect(page.getByRole("heading", { name: "新流程事项" })).toBeVisible();
  const source = page.locator(".work-meta").getByText("后端开发", { exact: true });
  await expect(source).toHaveAttribute("title", "本项目流程 · 7 个阶段");
  await expect(stages(page).locator(".step")).toHaveCount(7);
  await expect(stages(page)).toContainText("安全审查");

  await flowPage(page).click();
  await expect(custom).toContainText("1 个事项在用");
  await custom.getByRole("button", { name: "编辑" }).click();
  const edit = page.getByRole("dialog", { name: "编辑流程" });
  await edit.getByRole("button", { name: /安全审查.*测试/ }).click();
  await edit.getByRole("textbox", { name: "阶段名称" }).fill("权限复核");
  await edit.getByRole("button", { name: "保存", exact: true }).click();
  await expect(page.getByText("流程已保存 · 已用它创建的 1 个事项不受影响")).toBeVisible();
  await expect(custom).toContainText("权限复核");
  const existing = page.getByRole("navigation", { name: "事项" }).getByRole("button", { name: /^新流程事项/ });
  await existing.click();
  await expect(source).toHaveAttribute("title", /项目里的「后端开发」之后已修改/);
  await expect(stages(page)).toContainText("安全审查");

  await flowPage(page).click();
  await custom.getByRole("button", { name: "删除" }).click();
  const confirmation = page.getByRole("dialog", { name: "删除流程「后端开发」？" });
  await expect(confirmation).toContainText("1 个事项正在使用它，它们会继续按创建时的流程推进");
  await confirmation.getByRole("button", { name: "取消" }).click();
  await expect(custom).toBeVisible();
  await custom.getByRole("button", { name: "删除" }).click();
  await confirmation.getByRole("button", { name: "删除", exact: true }).click();
  await expect(custom).toHaveCount(0);
  await existing.click();
  await expect(source).toHaveAttribute("title", /项目里的「后端开发」已删除/);
  await expect(stages(page).locator(".step")).toHaveCount(7);
  await expect(stages(page)).toContainText("安全审查");
  await newWork(page).click();
  await expect(form.getByRole("combobox", { name: "流程" }).locator("option")).toHaveText(["软件开发", "游戏开发"]);
});

test("built-ins are read-only and broken project flows can be removed independently", async ({ page }) => {
  await page.goto("/");
  await flowPage(page).click();
  const software = flowRow(page, "软件开发");
  const inspect = software.getByRole("button", { name: /^软件开发/ });
  await inspect.click();
  await expect(inspect).toHaveAttribute("aria-expanded", "true");
  const list = software.getByRole("list", { name: "内置流程阶段" });
  await expect(list.getByRole("listitem")).toHaveCount(7);
  await list.getByRole("button", { name: /技术方案.*规划/ }).click();
  await expect(list.getByRole("textbox", { name: "阶段名称" })).toBeDisabled();
  await expect(list.getByRole("radio", { name: "规划", exact: true })).toBeDisabled();
  await expect(list.getByRole("checkbox", { name: "可跳过" })).toBeChecked();
  await expect(list.getByRole("checkbox", { name: "产品需求" })).toBeDisabled();
  await expect(software.getByRole("button", { name: "复制" })).toBeVisible();
  await expect(software.getByRole("button", { name: /编辑|删除/ })).toHaveCount(0);
  await expect(flowRow(page, "经典开发流程")).toHaveCount(0);

  const broken = flowRow(page, "旧版发布流程");
  await expect(broken).toContainText("无法读取：需要恰好一个「交付」阶段");
  await expect(broken.getByRole("button")).toHaveCount(1);
  await broken.getByRole("button", { name: "删除" }).click();
  const confirmation = page.getByRole("dialog", { name: "删除流程「旧版发布流程」？" });
  await expect(confirmation).not.toContainText("个事项正在使用");
  await confirmation.getByRole("button", { name: "删除", exact: true }).click();
  await expect(broken).toHaveCount(0);
  await expect(flowRow(page, "后端开发")).toBeVisible();
  const duplicate = flowRow(page, "重复标识的后端流程");
  await expect(duplicate.getByRole("button", { name: "删除" })).toBeDisabled();
  await expect(duplicate).toContainText("流程 ID 与其他流程冲突，无法单独删除。");
});

test("stage references survive renames and undo, and leaving a dirty editor needs confirmation", async ({ page }) => {
  await page.goto("/");
  await flowPage(page).click();
  await flowRow(page, "后端开发").getByRole("button", { name: "编辑" }).click();
  const editor = page.getByRole("dialog", { name: "编辑流程" });
  const stageName = editor.getByRole("textbox", { name: "阶段名称" });
  const first = editor.getByRole("button", { name: /产品需求.*规划/ });
  const audit = editor.getByRole("button", { name: /安全审查.*测试/ });
  await first.click();
  await stageName.fill("范围确认");
  await audit.click();
  const renamed = editor.getByRole("checkbox", { name: "范围确认" });
  const testTarget = editor.getByRole("checkbox", { name: "测试验证" });
  await expect(renamed).toBeChecked();
  await editor.getByRole("button", { name: "删除阶段「范围确认」" }).click();
  await expect(renamed).toHaveCount(0);
  await expect(editor.getByRole("status")).toContainText("同时从 1 个阶段的退回目标中移除");
  await testTarget.check();
  await editor.getByRole("button", { name: "撤销" }).click();
  await expect(renamed).toBeChecked();
  await expect(testTarget).toBeChecked();

  await editor.getByRole("button", { name: "上移「安全审查」" }).click();
  await expect(editor.getByText(/只能退回到它之前的阶段（「测试验证」在它之后或已删除）/)).toBeVisible();
  await expect(editor.getByRole("button", { name: "保存", exact: true })).toBeDisabled();
  const down = editor.getByRole("button", { name: "下移「安全审查」" });
  await down.click();
  await expect(down).toBeFocused();
  await expect(testTarget).toBeChecked();
  await editor.getByRole("button", { name: /技术方案.*规划/ }).click();
  await expect(editor.getByRole("checkbox", { name: "可跳过" })).toBeChecked();
  await editor.getByRole("radio", { name: "测试", exact: true }).click();
  await expect(editor.getByText("非规划阶段不可跳过，已取消勾选")).toBeVisible();
  await expect(editor.getByRole("checkbox", { name: "可跳过" })).not.toBeChecked();

  const discard = page.getByRole("dialog", { name: "放弃对「后端开发」的修改？" });
  await page.keyboard.press("Escape");
  await expect(discard).toBeVisible();
  await discard.getByRole("button", { name: "继续编辑" }).click();
  await expect(editor).toBeVisible();
  await page.getByRole("button", { name: "项目记忆" }).click();
  await expect(discard).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(discard).toHaveCount(0);
  await page.getByRole("button", { name: "行情终端" }).click();
  await page.getByRole("menuitem", { name: /内部工具/ }).click();
  await expect(discard).toBeVisible();
  await discard.getByRole("button", { name: "放弃", exact: true }).click();
  await expect(editor).toHaveCount(0);
  await expect(page.getByRole("button", { name: "内部工具" })).toBeVisible();
  await flowPage(page).click();
  await expect(page.getByText("还没有自定义流程。", { exact: false })).toBeVisible();
});

test("new flows focus the blank label and E7 warns without blocking save", async ({ page }) => {
  await page.goto("/");
  await flowPage(page).click();
  await page.getByRole("button", { name: "新建流程" }).click();
  const editor = page.getByRole("dialog", { name: "新建流程" });
  const label = editor.getByRole("textbox", { name: "流程名称" });
  const save = editor.getByRole("button", { name: "保存", exact: true });
  await expect(label).toBeFocused();
  await expect(editor.getByText("流程名称不能为空")).toHaveCount(0);
  await expect(save).toBeDisabled();
  await expect(stageNames(editor)).toHaveText(["产品需求", "开发实现", "独立验收", "交付归档"]);
  await editor.getByRole("button", { name: "1 个错误，修正后才能保存", exact: true }).click();
  await expect(editor.getByText("流程名称不能为空")).toBeVisible();
  await label.fill("缺少验收的流程");
  await editor.getByRole("button", { name: "删除阶段「独立验收」" }).click();
  await expect(editor.getByText(warning, { exact: true })).toBeVisible();
  await expect(save).toBeEnabled();
  await save.click();
  await expect(flowRow(page, "缺少验收的流程").getByText(warning, { exact: true })).toBeVisible();
});

type TestWindow = Window & {
  __TAURI_INTERNALS__: { invoke(name: keyof Api, args: unknown): Promise<unknown> };
  __testFlows: { mock: Promise<Api>; snapshotBlocked: boolean };
};

async function connectMock(page: Page) {
  await page.addInitScript(() => {
    const host = window as unknown as TestWindow;
    const url = new URL("/src/api/mock.ts", location.href).href;
    const mock: Promise<Api> = import(url).then((module) => module.createMockApi());
    const state = { mock, snapshotBlocked: false };
    host.__testFlows = state;
    host.__TAURI_INTERNALS__ = {
      async invoke(name, args) {
        if (name === "snapshot" && state.snapshotBlocked) throw new Error("测试读取故障");
        const backend = await mock;
        const command = backend[name] as (...params: unknown[]) => unknown;
        return command.call(backend, args);
      },
    };
  });
}

test("a flow removed while editing can be saved as new with the same draft contents", async ({ page }) => {
  await connectMock(page);
  await page.goto("/");
  await flowPage(page).click();
  await flowRow(page, "后端开发").getByRole("button", { name: "编辑" }).click();
  const editor = page.getByRole("dialog", { name: "编辑流程" });
  await editor.getByRole("textbox", { name: "流程名称" }).fill("恢复的后端流程");
  await editor.getByRole("button", { name: /安全审查.*测试/ }).click();
  await editor.getByRole("textbox", { name: "职责" }).fill("应当从未保存的编辑中恢复");
  await page.evaluate(async () => {
    const mock = await (window as unknown as TestWindow).__testFlows.mock;
    const project = mock.snapshot().projects.find(({ id }) => id === "quote")!;
    const flow = project.flows.find(({ label }) => label === "后端开发")!;
    mock.deleteFlow({ project: project.id, id: flow.id });
  });
  await editor.getByRole("button", { name: "保存", exact: true }).click();
  await expect(editor.getByRole("alert")).toHaveText("这套流程已被删除");
  await expect(editor.getByRole("textbox", { name: "职责" })).toHaveValue("应当从未保存的编辑中恢复");
  await editor.getByRole("button", { name: "另存为新流程" }).click();
  const restored = flowRow(page, "恢复的后端流程");
  await expect(restored).toBeVisible();
  await restored.getByRole("button", { name: "编辑" }).click();
  await editor.getByRole("button", { name: /安全审查.*测试/ }).click();
  await expect(editor.getByRole("textbox", { name: "职责" })).toHaveValue("应当从未保存的编辑中恢复");
});

test("a snapshot read failure keeps built-in flows available and supports retry", async ({ page }) => {
  await connectMock(page);
  await page.goto("/");
  await flowPage(page).click();
  await page.evaluate(() => { (window as unknown as TestWindow).__testFlows.snapshotBlocked = true; });
  await expect(page.getByText("读取流程失败：测试读取故障", { exact: true })).toBeVisible();
  await expect(flowRow(page, "软件开发").getByRole("button", { name: "复制" })).toBeEnabled();
  await expect(page.getByRole("button", { name: "新建流程" })).toBeEnabled();
  await page.evaluate(() => { (window as unknown as TestWindow).__testFlows.snapshotBlocked = false; });
  await page.getByRole("button", { name: "重试" }).click();
  await expect(page.getByText("读取流程失败：测试读取故障", { exact: true })).toHaveCount(0);
  await expect(flowRow(page, "后端开发")).toBeVisible();
});
