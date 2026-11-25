import type { FlowInput, FlowStage, Role } from "../api/types";

export const ROLE_LABEL: Record<Role, string> = {
  Plan: "规划",
  Build: "开发",
  Test: "测试",
  Accept: "验收",
  Deliver: "交付",
};

export const ROLE_HINT: Record<Role, string> = {
  Plan: "只写 specs/ 规格文档；非首个时可设为可跳过",
  Build: "可写工作区；通过后自动提交到事项分支",
  Test: "跑门禁，不能写文件；自动运行下失败会退回前面最近的开发阶段",
  Accept: "独立复核；合并使用它固定的提交",
  Deliver: "归档与合并，必须是最后一个阶段，且只能有一个",
};

export const BUILTIN_FLOW_LABELS = {
  Software: "软件开发",
  Game: "游戏开发",
  Legacy: "经典开发流程",
} as const;

export interface FlowIssue {
  stage: number | null;
  field: "label" | "stages" | "name" | "role" | "skippable" | "back_to";
  message: string;
}

// Rust's str::trim uses Unicode White_Space.
export function trimFlowText(text: string): string {
  return text.replace(/^\p{White_Space}+|\p{White_Space}+$/gu, "");
}

export function validateFlow(flow: FlowInput, takenLabels: readonly string[]): { errors: FlowIssue[]; warnings: FlowIssue[] } {
  const errors: FlowIssue[] = [];
  const warnings: FlowIssue[] = [];
  const label = trimFlowText(flow.label);
  if (!label) {
    errors.push({ stage: null, field: "label", message: "流程名称不能为空" });
  } else if (takenLabels.some((taken) => trimFlowText(taken) === label)) {
    const builtin = Object.values(BUILTIN_FLOW_LABELS).some((name) => name === label);
    errors.push({ stage: null, field: "label", message: builtin ? "不能与内置流程同名" : `与「${label}」重名` });
  }

  const { stages } = flow;
  if (stages.length < 2 || stages.length > 20) {
    errors.push({ stage: null, field: "stages", message: stages.length < 2 ? "至少需要 2 个阶段" : "最多 20 个阶段" });
  }
  const deliver = stages[stages.length - 1];
  const earlier = stages.slice(0, -1);
  const validDelivery = deliver?.role === "Deliver" && !earlier.some((s) => s.role === "Deliver");
  if (!validDelivery) {
    errors.push({ stage: null, field: "role", message: "需要恰好一个「交付」阶段，并放在最后" });
  }

  const earlierNames = new Map<string, number>();
  stages.forEach((s, stage) => {
    const n = stage + 1;
    const name = trimFlowText(s.name);
    const error = (field: FlowIssue["field"], message: string) => errors.push({ stage, field, message });
    const first = earlierNames.get(name);
    if (!name) error("name", `第 ${n} 个阶段：名称不能为空`);
    if ([...name].length > 20) error("name", `第 ${n} 个阶段：不超过 20 个字`);
    if (first !== undefined) error("name", `第 ${n} 个阶段：与第 ${first} 个阶段重名`);
    if (/[\r\n：:「」<>]/u.test(name)) error("name", `第 ${n} 个阶段：不能包含「：」「:」「「」」「<」「>」或换行`);
    if (s.skippable && (stage === 0 || s.role !== "Plan")) {
      error("skippable", `第 ${n} 个阶段「${name}」：只有非首个的规划阶段可以跳过`);
    }
    for (const raw of s.back_to ?? []) {
      const target = trimFlowText(raw);
      if (!earlierNames.has(target)) {
        error("back_to", `第 ${n} 个阶段「${name}」：只能退回到它之前的阶段（「${target}」在它之后或已删除）`);
      }
    }
    if (first === undefined) earlierNames.set(name, n);
  });

  const reversedBuildIndex = [...earlier].reverse().findIndex((s) => s.role === "Build");
  if (validDelivery && reversedBuildIndex !== -1) {
    const build = earlier.length - 1 - reversedBuildIndex;
    if (!earlier.slice(build + 1).some((s) => s.role === "Accept")) {
      warnings.push({
        stage: build,
        field: "role",
        message: `「${trimFlowText(earlier[build].name)}」之后、「${trimFlowText(deliver.name)}」之前没有验收阶段。合并将直接使用最后一次通过的阶段提交。`,
      });
    }
  }
  return { errors, warnings };
}

export function copyLabel(label: string, takenLabels: readonly string[]): string {
  const taken = new Set(takenLabels.map(trimFlowText));
  const base = `${label} 副本`;
  let result = base;
  for (let suffix = 2; taken.has(result); suffix++) result = `${base} ${suffix}`;
  return result;
}

export function defaultNewFlow(): FlowInput {
  const stage = (name: string, role: Role): FlowStage => ({ name, role, brief: "", skippable: false, back_to: [] });
  return {
    id: "",
    label: "",
    stages: [stage("产品需求", "Plan"), stage("开发实现", "Build"), stage("独立验收", "Accept"), stage("交付归档", "Deliver")],
  };
}
