use crate::zcode_catalog::desktop_plan;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{fs, io, path::Path};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActualModel {
    pub transport: String,
    pub plan: String,
    pub provider_id: String,
    pub model_id: String,
    pub reasoning_level: Option<String>,
    pub selected: Option<String>,
    pub session: Option<String>,
    pub turn: Option<String>,
}

impl ActualModel {
    pub fn from_receipt(value: &Value) -> Option<Self> {
        let field = |key: &str| value[key].as_str();
        let transport = field("transport")?;
        if !matches!(transport, "desktop" | "app-server") {
            return None;
        }
        let provider = field("providerId").filter(|value| !value.trim().is_empty())?;
        let model = field("modelId").filter(|value| !value.trim().is_empty())?;
        Some(Self {
            transport: transport.into(),
            plan: plan(provider).into(),
            provider_id: provider.into(),
            model_id: model.into(),
            reasoning_level: field("reasoningLevel").map(str::to_owned),
            selected: field("selected").map(str::to_owned),
            session: field("session").map(str::to_owned),
            turn: field("turn").map(str::to_owned),
        })
    }

    fn default_suffix(&self) -> &str {
        if self.selected.as_deref() == Some("") {
            " · 客户端默认"
        } else {
            ""
        }
    }

    pub fn label(&self) -> String {
        format!(
            "{} / {} / {}{}",
            self.plan,
            self.model_id,
            level_label(self.reasoning_level.as_deref()),
            self.default_suffix()
        )
    }

    pub fn author(&self) -> String {
        format!(
            "ZCode / {} / {}@{}{}",
            self.plan,
            self.model_id,
            level_label(self.reasoning_level.as_deref()),
            self.default_suffix()
        )
    }

    pub fn transport_label(&self) -> &'static str {
        if self.transport == "desktop" {
            "ZCode 桌面"
        } else {
            "ZCode app-server"
        }
    }
}

fn plan(provider: &str) -> &str {
    desktop_plan(provider).unwrap_or(provider)
}

fn level_label(level: Option<&str>) -> &str {
    level
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("默认")
}

pub fn read(dir: &Path) -> Result<Option<ActualModel>> {
    Ok(read_value(dir)?
        .as_ref()
        .and_then(ActualModel::from_receipt))
}

pub(crate) fn read_value(dir: &Path) -> Result<Option<Value>> {
    let path = dir.join("session.json");
    let data = match fs::read(&path) {
        Ok(data) => data,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("读取 {}", path.display())),
    };
    serde_json::from_slice(&data)
        .map(Some)
        .with_context(|| format!("解析 {}", path.display()))
}

pub fn selected(model: Option<&Value>) -> String {
    let Some(model) = model else {
        return String::new();
    };
    let provider = model["providerId"].as_str().unwrap_or_default();
    let name = model["modelId"].as_str().unwrap_or_default();
    match model
        .pointer("/options/reasoningLevel")
        .and_then(Value::as_str)
    {
        Some(level) if !level.trim().is_empty() => format!("{provider}/{name}@{level}"),
        _ => format!("{provider}/{name}"),
    }
}

pub fn fallback(selected: &str) -> String {
    let selected = if selected.is_empty() {
        "默认模型"
    } else {
        selected
    };
    format!("{selected} · 未记录实际模型")
}

pub fn mismatch(selected: &str, provider: &str, model: &str, level: Option<&str>) -> String {
    let selected = if selected.is_empty() {
        "客户端默认"
    } else {
        selected
    };
    format!(
        "ZCode 实际模型与所选不符：所选 {selected}，实际 {} / {model} / {}",
        plan(provider),
        level_label(level)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn formats_both_plans_and_legacy_models() {
        let mut receipt = json!({"transport":"desktop","plan":"ignored",
            "providerId":"account:bigmodel-start-plan","modelId":"glm","reasoningLevel":"high"});
        assert_eq!(
            ActualModel::from_receipt(&receipt).unwrap().label(),
            "BigModel Start Plan / glm / high"
        );
        receipt["providerId"] = json!("account:bigmodel-individual-coding-plan");
        receipt["reasoningLevel"] = json!(" ");
        receipt["selected"] = json!("");
        let actual = ActualModel::from_receipt(&receipt).unwrap();
        assert_eq!(
            actual.label(),
            "BigModel 个人 Coding Plan / glm / 默认 · 客户端默认"
        );
        assert_eq!(
            actual.author(),
            "ZCode / BigModel 个人 Coding Plan / glm@默认 · 客户端默认"
        );
        receipt["providerId"] = json!("custom");
        receipt.as_object_mut().unwrap().remove("selected");
        assert_eq!(
            ActualModel::from_receipt(&receipt).unwrap().label(),
            "custom / glm / 默认"
        );
        assert_eq!(fallback(""), "默认模型 · 未记录实际模型");
        assert_eq!(
            mismatch(
                "custom/glm@high",
                "account:bigmodel-start-plan",
                "glm",
                None
            ),
            "ZCode 实际模型与所选不符：所选 custom/glm@high，实际 BigModel Start Plan / glm / 默认"
        );
    }

    #[test]
    fn reads_missing_legacy_complete_and_corrupt_receipts() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("session.json");
        assert!(read(temp.path())?.is_none());
        fs::write(&path, r#"{"session":"task","transport":"app-server"}"#)?;
        assert!(read(temp.path())?.is_none());
        fs::write(
            &path,
            r#"{"session":"task","transport":"app-server","providerId":"native","modelId":"glm","selected":""}"#,
        )?;
        assert_eq!(
            serde_json::to_value(read(temp.path())?)?,
            json!({
                "transport":"app-server","plan":"native","providerId":"native","modelId":"glm",
                "reasoningLevel":null,"selected":"","session":"task","turn":null
            })
        );
        fs::write(path, "{")?;
        assert!(read(temp.path()).is_err());
        Ok(())
    }
}
