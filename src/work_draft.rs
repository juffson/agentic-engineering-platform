//! Draft new work items with a model CLI: from a plain description it reads the repository
//! (read-only) and the project memory, and proposes one or more items with a name, template
//! and 产品需求 brief. A person reviews, edits and creates them; nothing is created here.
use crate::{
    memory::TEXT_LIMIT,
    workflow::{Profile, Template},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{fs, path::Path};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftItem {
    pub name: String,
    #[serde(default)]
    pub release: bool,
    /// "Software", "Game" or "Legacy".
    #[serde(default = "software")]
    pub template: String,
    /// The 产品需求 stage brief.
    pub brief: String,
}

fn software() -> String {
    "Software".into()
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    pub summary: String,
    pub items: Vec<DraftItem>,
}

const TEMPLATES: [Template; 3] = [Template::Software, Template::Game, Template::Legacy];

fn template_id(t: Template) -> &'static str {
    match t {
        Template::Software => "Software",
        Template::Game => "Game",
        Template::Legacy => "Legacy",
    }
}

pub fn prompt(request: &str, notes: &[Value], existing: &[String]) -> String {
    let mut text = String::from(
        "你在帮一个软件项目把一段需求描述整理成开发事项。事项会按模板的阶段（产品需求 → … → 交付归档）由不同的 Agent 依次推进，\
         第一阶段「产品需求」读你写的 brief。\n\n\
         请先在当前仓库里看看相关代码和文档（只读，不要修改任何文件），再给出事项：\n\
         - 一件事一个事项；描述里有几件相互独立、可以分开交付的事，就拆成几个，否则只给一个。\n\
         - name：简短的事项名，形如「模块 · 要做的事」，不超过 30 字。\n\
         - template：从下面的模板里选最合适的 id。\n\
         - release：只有发版、上线类事项为 true。\n\
         - brief：给产品需求阶段的说明，写清目标、范围（做什么 / 不做什么）、相关代码位置和验收要点；只写你从描述和仓库里确认过的，不确定的写成待确认问题。不超过 900 字。\n\n",
    );
    text.push_str("## 模板\n");
    for t in TEMPLATES {
        let stages: Vec<&str> = t.stages().iter().map(|s| s.name.as_str()).collect();
        text.push_str(&format!(
            "- {}（{}）：{}\n",
            template_id(t),
            t.label(),
            stages.join(" → ")
        ));
    }
    if !existing.is_empty() {
        text.push_str("\n## 已有事项（不要重复）\n");
        for name in existing.iter().take(40) {
            text.push_str(&format!("- {name}\n"));
        }
    }
    if !notes.is_empty() {
        text.push_str("\n## 项目记忆（各阶段 Agent 共享的决策和约定）\n");
        for n in notes.iter().take(32) {
            text.push_str(&format!(
                "- #{} {}\n",
                n["id"],
                n["body"].as_str().unwrap_or("")
            ));
        }
    }
    text.push_str(&format!("\n## 需求描述\n{}\n", request.trim()));
    text.push_str(
        "\n## 输出\n只输出一个 JSON 对象，不要别的文字：\n\
         {\"summary\":\"一句话说明怎么拆的\",\"items\":[{\"name\":\"…\",\"template\":\"Software\",\"release\":false,\"brief\":\"…\"}]}",
    );
    text
}

pub fn parse(answer: &str) -> Result<Draft> {
    let start = answer.find('{').context("答复里没有 JSON")?;
    let end = answer.rfind('}').context("答复里没有 JSON")?;
    let mut draft: Draft =
        serde_json::from_str(&answer[start..=end]).context("无法解析事项草稿 JSON")?;
    draft
        .items
        .retain(|i| !i.name.trim().is_empty() && !i.brief.trim().is_empty());
    for item in &mut draft.items {
        item.name = item.name.trim().chars().take(60).collect();
        item.brief = item.brief.trim().chars().take(TEXT_LIMIT).collect();
        if !TEMPLATES.iter().any(|t| template_id(*t) == item.template) {
            item.template = software();
        }
    }
    ensure!(!draft.items.is_empty(), "Agent 没有给出事项");
    Ok(draft)
}

/// A draft job: `job.json` (profile, request, repository, existing, notes) in, `status.json`
/// and `draft.json` out.
pub fn run(job: &Path) -> Result<()> {
    let spec = crate::workspace::read(&job.join("job.json"))?;
    let status = |s: &str, extra: Value| -> Result<()> {
        let mut v = json!({"status": s, "updated_at": chrono::Utc::now().to_rfc3339()});
        if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
            o.extend(e.clone());
        }
        crate::project::save_json(job, "status.json", &v)
    };
    status("running", json!({}))?;
    let result = (|| -> Result<Draft> {
        let profile: Profile = serde_json::from_value(spec["profile"].clone())?;
        let notes: Vec<Value> = spec["notes"].as_array().cloned().unwrap_or_default();
        let existing: Vec<String> =
            serde_json::from_value(spec["existing"].clone()).unwrap_or_default();
        let text = prompt(spec["request"].as_str().unwrap_or(""), &notes, &existing);
        fs::write(job.join("prompt.txt"), &text)?;
        // Work in the repository so the agent can read the code; without one, in the job folder.
        let cwd = spec["repository"]
            .as_str()
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| job.to_owned());
        let answer = crate::memory_tidy::ask(&profile, &text, job, &cwd)?;
        fs::write(job.join("result.txt"), &answer)?;
        parse(&answer)
    })();
    match result {
        Ok(d) => {
            crate::project::save_json(job, "draft.json", &d)?;
            status("succeeded", json!({"items": d.items.len()}))
        }
        Err(e) => status("failed", json!({"error": format!("{e:#}")})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drafts_are_cleaned_up() {
        let d = parse(
            r#"好的 {"summary":"拆成两件","items":[
              {"name":" 导出 · CSV ","brief":"导出对账单"},
              {"name":"发版 2.3","template":"Nope","release":true,"brief":"上线"},
              {"name":"","brief":"没有名字"}]}"#,
        )
        .unwrap();
        assert_eq!(d.items.len(), 2);
        assert_eq!(d.items[0].name, "导出 · CSV");
        assert_eq!(d.items[0].template, "Software");
        assert!(d.items[1].release);
        assert_eq!(d.items[1].template, "Software");
        assert!(parse(r#"{"summary":"","items":[]}"#).is_err());
        let p = prompt(
            "支持导出",
            &[json!({"id":1,"body":"金额用分"})],
            &["已有事项".into()],
        );
        assert!(
            p.contains("Game（游戏开发）") && p.contains("#1 金额用分") && p.contains("- 已有事项")
        );
    }
}
