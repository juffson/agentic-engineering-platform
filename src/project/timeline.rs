//! Project views reference the original run evidence and the exact memory revisions it consumed.
use super::{RecordKind, Work, WorkRecord};
use crate::{
    memory::Memory,
    workflow,
    workspace::{Workspace, ident, read},
    zcode_receipt::{self, ActualModel},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
};

#[derive(Default, Serialize, Deserialize)]
pub struct Timeline {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merge_events: Vec<super::merge::MergeReceipt>,
    #[serde(default)]
    pub deliveries: Vec<super::archive::Delivery>,
    pub work_id: String,
    pub work_name: String,
    pub stages: Vec<String>,
    pub rounds: Vec<Round>,
    pub stage_transfers: Vec<super::transfer::Transfer>,
    pub errors: Vec<String>,
}
#[derive(Serialize, Deserialize)]
pub struct Round {
    pub record: String,
    pub run: String,
    pub repository: Option<PathBuf>,
    #[serde(rename = "stage_index")]
    pub stage: Option<usize>,
    pub round: Option<u32>,
    pub attempt: usize,
    pub status: String,
    pub decision: Option<i64>,
    pub read_stage_transfer: Option<super::transfer::Transfer>,
    pub agents: Vec<Agent>,
}
#[derive(Serialize, Deserialize)]
pub struct Agent {
    pub role: String,
    pub client: String,
    pub model: Option<String>,
    #[serde(default)]
    pub actual_model: Option<ActualModel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_model_error: Option<String>,
    #[serde(rename = "stage_index")]
    pub stage: Option<usize>,
    pub session: Option<String>,
    pub desktop_dir: Option<PathBuf>,
    pub reads: Vec<Note>,
    pub writes: Vec<Note>,
    pub handoff: String,
}
#[derive(Serialize, Deserialize)]
pub struct Note {
    pub store: String,
    pub id: i64,
    pub kind: String,
    pub body: String,
    pub replaces: Option<i64>,
}
fn notes(db: Option<&Memory>, store: &'static str, ids: &Value) -> Result<Vec<Note>> {
    let mut result = Vec::new();
    let values = ids
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_else(|| std::slice::from_ref(ids));
    for id in values.iter().filter_map(Value::as_i64) {
        let note = match db.map(|db| db.get(id)).transpose() {
            Ok(Some(v)) => v,
            Ok(None) => Value::Null,
            Err(e)
                if matches!(
                    e.downcast_ref::<rusqlite::Error>(),
                    Some(rusqlite::Error::QueryReturnedNoRows)
                ) =>
            {
                Value::Null
            }
            Err(e) => return Err(e),
        };
        result.push(Note {
            store: store.into(),
            id,
            kind: text(&note, "kind").into(),
            replaces: note["replaces"].as_i64(),
            body: note["body"].as_str().unwrap_or("找不到该版本的记忆").into(),
        });
    }
    Ok(result)
}
fn text<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or_default()
}
fn optional(path: PathBuf) -> Result<Value> {
    if path.try_exists()? {
        read(&path)
    } else {
        Ok(Value::Null)
    }
}

struct Receipt {
    desktop_dir: Option<PathBuf>,
    session: Option<String>,
    actual_model: Option<ActualModel>,
    actual_model_error: Option<String>,
}

fn receipt(dir: &Path) -> Result<Receipt> {
    let (saved, actual_model_error) = match zcode_receipt::read_value(dir) {
        Ok(value) => (value.unwrap_or(Value::Null), None),
        Err(_) => (Value::Null, Some("实际模型回执无法读取".into())),
    };
    let created = optional(dir.join("desktop-create.json"))?;
    let native = created["session"]["sessionId"].as_str();
    let completed = saved["session"]
        .as_str()
        .filter(|_| saved["transport"] == "desktop");
    ensure!(
        native.zip(completed).is_none_or(
            |(a, b)| a == b && created["session"]["workspacePath"] == saved["workspace"]
        ),
        "桌面会话回执不一致"
    );
    let desktop_session = native.or(completed);
    Ok(Receipt {
        desktop_dir: desktop_session.map(|_| dir.to_owned()),
        session: desktop_session
            .or_else(|| {
                (saved["transport"] == "app-server")
                    .then(|| saved["session"].as_str())
                    .flatten()
            })
            .map(str::to_owned),
        actual_model: ActualModel::from_receipt(&saved),
        actual_model_error,
    })
}

/// A relay attempt ID is generated from its relay, round and attempt number. Require membership in the manifest as well.
pub fn relay_for_run(ws: &Workspace, run: &str) -> Result<Option<String>> {
    ident(run, 64)?;
    let Some((parent, _)) = run
        .rsplit_once("-r")
        .filter(|(p, _)| p.starts_with("relay-"))
    else {
        return Ok(None);
    };
    let manifest = optional(ws.data.join("relays").join(parent).join("relay.json"))?;
    Ok((manifest["id"] == parent
        && manifest["attempts"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v["id"] == run)))
    .then(|| parent.to_owned()))
}

// Older Codex receipts only carry the CLI thread ID in the opening JSONL event.
fn codex_thread(dir: &Path) -> Option<String> {
    let file = fs::File::open(dir.join("implementation.log")).ok()?;
    BufReader::new(file.take(32768))
        .lines()
        .map_while(Result::ok)
        .find_map(|line| {
            let event: Value = serde_json::from_str(&line).ok()?;
            (event["type"] == "thread.started")
                .then(|| event["thread_id"].as_str().map(str::to_owned))
                .flatten()
        })
}

fn repository_run(
    ws: &Workspace,
    link: &WorkRecord,
    round: &mut Round,
    db: Option<&Memory>,
) -> Result<()> {
    ident(&round.run, 64)?;
    let dir = ws.data.join("runs").join(&round.run);
    let ev = crate::workspace::status(&ws.data, &round.run)?;
    ensure!(ev["run_id"] == round.run, "运行回执身份不符");
    round.status = text(&ev, "status").into();
    round.read_stage_transfer = serde_json::from_value(ev["read_stage_transfer"].clone())?;
    for (role, name, client, folder) in [
        ("implementer", "实现", "implementation_source", "zcode-edit"),
        ("reviewer", "审查", "reviewer", "zcode-plan"),
    ] {
        let path = dir.join(folder);
        let receipt = receipt(&path)?;
        let client = text(&ev, client);
        let request = &ev["request"];
        let model = match (role, client) {
            ("implementer", "codex") => &request["codex_model"],
            ("reviewer", "zcode") if request["reviewer_model"].is_null() => &request["zcode_model"],
            ("reviewer", _) => &request["reviewer_model"],
            (_, "zcode") => &request["zcode_model"],
            _ => &Value::Null,
        }
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
        let session = receipt
            .session
            .or_else(|| ev["sessions"][role].as_str().map(str::to_owned))
            .or_else(|| {
                if client == "codex" && role == "implementer" {
                    codex_thread(&dir)
                } else {
                    None
                }
            });
        let handoff = if role == "implementer" {
            let h = optional(dir.join("handoff.json"))?;
            [text(&h, "note"), text(&h, "question")]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        } else if dir.join("review.txt").try_exists()? {
            fs::read_to_string(dir.join("review.txt"))?
        } else {
            String::new()
        };
        round.agents.push(Agent {
            role: name.into(),
            client: client.into(),
            model,
            actual_model: receipt.actual_model,
            actual_model_error: receipt.actual_model_error,
            stage: session
                .as_ref()
                .and_then(|id| link.session_stages.get(id))
                .copied()
                .or(link.stage),
            desktop_dir: receipt.desktop_dir,
            session,
            reads: notes(db, "仓库", &ev["memory_inputs"][role])?,
            writes: notes(db, "仓库", &ev["memory_outputs"][role])?,
            handoff,
        });
    }
    Ok(())
}
impl Timeline {
    fn record(&mut self, link: &WorkRecord) -> Result<()> {
        let ws = Workspace::open(&link.repository)?;
        link.kind.verify(&ws, &link.id)?;
        let db = Memory::read_only(&ws.data)?;
        let attempts = if link.kind == RecordKind::Relay {
            let manifest = read(&ws.data.join("relays").join(&link.id).join("relay.json"))?;
            manifest["attempts"]
                .as_array()
                .context("接力缺少轮次")?
                .clone()
        } else {
            vec![json!({"id":link.id})]
        };
        let mut counts = BTreeMap::new();
        for a in attempts {
            let number = match a["round"].as_str() {
                Some("First") => Some(1),
                Some("Second") => Some(2),
                None if link.kind == RecordKind::Run => None,
                _ => anyhow::bail!("无法识别接力轮次"),
            };
            let count = counts.entry(number).or_insert(0);
            *count += 1;
            let mut round = Round {
                record: link.id.clone(),
                run: text(&a, "id").into(),
                repository: Some(link.repository.clone()),
                stage: link.stage,
                round: number,
                attempt: *count,
                status: String::new(),
                read_stage_transfer: None,
                decision: a["decision"].as_i64(),
                agents: vec![],
            };
            if let Err(e) = repository_run(&ws, link, &mut round, db.as_ref()) {
                self.errors.push(format!("{} · {e:#}", round.run));
                round.status = "证据不完整".into();
            }
            self.rounds.push(round);
        }
        Ok(())
    }
}
pub fn load(home: &Path, project: &str, work: &Work) -> Result<Timeline> {
    ident(project, 64)?;
    ident(&work.id, 64)?;
    let stages = work.stages();
    let mut result = Timeline {
        deliveries: work.deliveries.clone(),
        work_id: work.id.clone(),
        stage_transfers: work.stage_transfers.clone(),
        work_name: work.name.clone(),
        stages: stages.iter().map(|s| s.name.clone()).collect(),
        ..Default::default()
    };
    for delivery in &mut result.deliveries {
        delivery.merge = super::merge::receipt(home, delivery)?.or(delivery.merge.take());
        if let Some(r) = delivery
            .merge
            .as_ref()
            .filter(|r| r.status == super::merge::MergeStatus::Merged)
        {
            result.merge_events.push(r.clone());
        }
    }
    for link in work.records.iter().filter(|r| r.kind != RecordKind::Https) {
        if let Err(e) = result.record(link) {
            result.errors.push(format!("{} · {e:#}", link.id));
        }
    }
    let mut counts = BTreeMap::new();
    for a in &work.attempts {
        let count = counts.entry(a.stage).or_insert(0);
        *count += 1;
        let attempt = (|| -> Result<Round> {
            ident(&a.id, 64)?;
            let dir = workflow::attempt_dir(home, project, &work.id, &a.id);
            let ev = workflow::evidence(&dir)?;
            let project_db = Memory::read_only(&home.join("workflows").join(project))?;
            let mut reads = notes(project_db.as_ref(), "项目", &ev["read_memory"])?;
            if let Some(repo) = &a.repository
                && ev["read_repository_memory"]
                    .as_array()
                    .is_some_and(|v| !v.is_empty())
            {
                let ws = Workspace::open(repo)?;
                reads.extend(notes(
                    Memory::read_only(&ws.data)?.as_ref(),
                    "仓库",
                    &ev["read_repository_memory"],
                )?);
            }
            let received: Option<super::transfer::Transfer> =
                serde_json::from_value(ev["read_stage_transfer"].clone())?;
            // Early delivery receipts embedded the exact revision before separately indexing repository reads.
            if let Some((t, id)) = received.as_ref().and_then(|t| {
                t.repository_decision(a.repository.as_deref())
                    .map(|id| (t, id))
            }) && !reads.iter().any(|n| n.store == "仓库" && n.id == id)
            {
                reads.push(Note {
                    store: "仓库".into(),
                    id,
                    kind: "decision".into(),
                    body: t.decision.clone(),
                    replaces: None,
                });
            }
            let receipt = receipt(&dir)?;
            let stage = stages.get(a.stage).context("阶段不存在")?;
            let agent = Agent {
                role: stage.name.clone(),
                client: a.profile.client.label().into(),
                model: (!a.profile.model.is_empty()).then(|| a.profile.model.clone()),
                actual_model: receipt.actual_model,
                actual_model_error: receipt.actual_model_error,
                stage: Some(a.stage),
                desktop_dir: receipt.desktop_dir,
                session: receipt
                    .session
                    .or_else(|| ev["session"].as_str().map(str::to_owned)),
                reads,
                writes: notes(project_db.as_ref(), "项目", &ev["written_memory"])?,
                handoff: crate::handoff::summary(&workflow::tail(&dir.join("result.txt"), 8192)),
            };
            Ok(Round {
                record: a.id.clone(),
                run: a.id.clone(),
                repository: a.repository.clone(),
                stage: Some(a.stage),
                round: None,
                attempt: *count,
                status: text(&ev, "status").into(),
                read_stage_transfer: received,
                decision: None,
                agents: vec![agent],
            })
        })();
        match attempt {
            Ok(row) => result.rounds.push(row),
            Err(e) => result.errors.push(format!("{} · {e:#}", a.id)),
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{Kind, Library, save_json};
    use crate::workflow::Template;
    #[test]
    fn legacy_relay_retries_and_session_stage_survive_reopening() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let repo = tmp.path().canonicalize()?;
        crate::workspace::git(&repo, &["init", "-q"])?;
        let ws = Workspace::open(&repo)?;
        let relay = "relay-test";
        let archive = ws.data.join("relays").join(relay);
        fs::create_dir_all(&archive)?;
        let mut attempts = vec![];
        for (id, round) in [
            ("relay-test-r1-1", "First"),
            ("relay-test-r1-2", "First"),
            ("relay-test-r2-3", "Second"),
        ] {
            attempts.push(json!({"id":id,"round":round}));
            let dir = ws.data.join("runs").join(id);
            fs::create_dir_all(dir.join("zcode-edit"))?;
            save_json(
                &dir,
                "evidence.json",
                &json!({"run_id":id,"implementation_source":"zcode","reviewer":"zcode","status":"complete","memory_inputs":{"implementer":[5]}}),
            )?;
            if id == "relay-test-r1-1" {
                save_json(
                    &dir.join("zcode-edit"),
                    "session.json",
                    &json!({"session":id,"transport":"desktop"}),
                )?;
            } else if id == "relay-test-r1-2" {
                fs::write(dir.join("zcode-edit/session.json"), "{")?;
            } else if id == "relay-test-r2-3" {
                for (folder, transport, provider) in [
                    ("zcode-edit", "desktop", "account:bigmodel-start-plan"),
                    (
                        "zcode-plan",
                        "app-server",
                        "account:bigmodel-individual-coding-plan",
                    ),
                ] {
                    let path = dir.join(folder);
                    fs::create_dir_all(&path)?;
                    save_json(
                        &path,
                        "session.json",
                        &json!({
                            "session":if folder == "zcode-edit" { id } else { "review-session" },
                            "transport":transport,"providerId":provider,
                            "modelId":"glm","turn":"turn"
                        }),
                    )?;
                }
            }
        }
        save_json(
            &archive,
            "relay.json",
            &json!({"id":relay,"task":{"name":"demo"},"attempts":attempts}),
        )?;
        let mut lib = Library::default();
        let home = tmp.path().join("catalog");
        lib.change(&home, |l| {
            l.create("Test", vec![repo.clone()])?;
            let p = l.project_mut()?;
            p.add_work("Feature", Kind::Feature, Template::Software)?;
            let work = p.selected_mut()?;
            work.repositories = vec![repo.clone()];
            // Catalogs written before stage-level associations have neither field.
            work.records.push(serde_json::from_value(
                json!({"kind":"relay","repository":repo,"id":relay}),
            )?);
            let wid = work.id.clone();
            p.link_session(&wid, &repo, RecordKind::Relay, relay, "relay-test-r2-3", 3)?;
            p.link_at_stage(&wid, &repo, RecordKind::Relay, relay, Some(2))?;
            p.add_work("Other", Kind::Feature, Template::Software)?;
            let other = p.selected_mut()?;
            other.repositories = vec![repo.clone()];
            let wid = other.id.clone();
            assert!(
                p.link_record(&wid, &repo, RecordKind::Relay, relay)
                    .is_err()
            );
            Ok(())
        })?;
        let persisted = Library::open(&home)?;
        let p = persisted.project().unwrap();
        let timeline = load(&home, &p.id, &p.work[0])?;
        assert!(timeline.errors.is_empty());
        assert_eq!(
            timeline
                .rounds
                .iter()
                .map(|r| (r.round, r.attempt))
                .collect::<Vec<_>>(),
            [(Some(1), 1), (Some(1), 2), (Some(2), 1)]
        );
        assert_eq!(timeline.rounds[2].agents[0].stage, Some(3));
        assert_eq!(timeline.rounds[2].agents[1].stage, Some(2));
        assert_eq!(timeline.rounds[2].agents[0].reads[0].id, 5);
        let old = &timeline.rounds[0].agents[0];
        assert_eq!(
            serde_json::to_value(old)?.get("actual_model"),
            Some(&Value::Null)
        );
        assert!(old.actual_model_error.is_none());
        let corrupt = &timeline.rounds[1].agents[0];
        assert!(corrupt.actual_model.is_none());
        assert_eq!(
            corrupt.actual_model_error.as_deref(),
            Some("实际模型回执无法读取")
        );
        let agents = &timeline.rounds[2].agents;
        assert_eq!(
            agents[0].actual_model.as_ref().unwrap().plan,
            "BigModel Start Plan"
        );
        assert_eq!(
            agents[1].actual_model.as_ref().unwrap().plan,
            "BigModel 个人 Coding Plan"
        );
        assert!(agents[1].desktop_dir.is_none());
        assert!(load(&home, &p.id, &p.work[1])?.rounds.is_empty());
        assert!(RecordKind::Run.verify(&ws, "relay-test-r1-1").is_err());
        assert!(!ws.data.join("memory.sqlite3").exists());
        Ok(())
    }
}
