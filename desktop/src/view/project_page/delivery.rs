use super::*;
use crate::stage_lifecycle::{self as flow, Models, Plan, Profile as Step};
use aep::workspace::read;
use std::fs;
const ROLES: [&str; 7] = [
    "第一轮实现",
    "第一轮审查",
    "第二轮实现",
    "第二轮审查",
    "测试验证",
    "独立验收",
    "交付归档",
];
const BRIEFS: [&str; 3] = [
    "在隔离工作区核对本次决策与 Spec，运行验收命令并记录命令和结果。通过时末行写 AEP: PASS。",
    "独立核对交付内容、决策及原始测试回执，引用完整测试 run_id、会话和一项具体结果。通过时末行写 AEP: PASS。",
    "核对原始独立验收回执及决策，记录交付版本、验收执行 ID 和交付结论。",
];
pub(super) struct State {
    pub(super) work: String,
    root: PathBuf,
    repo: PathBuf,
    stage: usize,
    tasks: Vec<crate::relay::Task>,
    task: Option<usize>,
    choices: Vec<(Client, String)>,
    slot: usize,
    input: Entity<InputState>,
    jobs: Vec<PathBuf>,
    job: Option<PathBuf>,
    preview: Option<PathBuf>,
}
fn standard(c: Client) -> &'static [&'static str] {
    match c {
        Client::Codex => &["gpt-6.1-sol", "gpt-6.2-sol", "gpt-6.2-astra"],
        Client::Claude => &["sonnet", "opus"],
        _ => &[],
    }
}
fn screenshot_label(path: &str) -> String {
    let (phase, name) = path
        .trim_end_matches(".png")
        .split_once('/')
        .unwrap_or(("", path));
    let stage = match phase {
        "relay" => "开发",
        "test" => "测试",
        "accept" => "验收",
        "deliver" => "交付",
        other => other,
    };
    let title = match name {
        "stage-first" => "第一轮回执",
        "stage-second" => "第二轮回执",
        "stage-question" => "待修订问题",
        "stage-decision" => "修订决策",
        "stage-models" => "模型配置",
        "stage-timeline-round-1" => "第一轮时间线",
        "stage-timeline-round-2" => "第二轮时间线",
        "stage-run-evidence" => "接力证据",
        "source-acceptance" => "交接确认",
        "reopened-source" => "来源执行",
        "reopened-destination" => "接收执行",
        "delivery-confirmation" => "归档确认",
        "reopened-delivery-archive" => "重开后的归档",
        other => other,
    };
    format!("{stage} · {title}")
}
fn allowed(i: usize) -> &'static [Client] {
    match i {
        0 | 2 => &[Client::Codex, Client::ZCode],
        1 | 3 => &[Client::Claude, Client::ZCode],
        _ => &[Client::Codex, Client::Claude, Client::ZCode],
    }
}
impl State {
    fn plan(&self, cx: &App) -> Result<Plan> {
        let task = self
            .tasks
            .get(self.task.context("请选择任务 Spec")?)
            .context("任务不存在")?;
        let mut choices = self.choices.clone();
        choices[self.slot].1 = self.input.read(cx).value().trim().into();
        ensure!(
            choices.iter().all(|(_, s)| !s.trim().is_empty()),
            "请为每个角色选择模型"
        );
        let round = |i: usize| Models {
            implementer: choices[i].0.label().to_lowercase(),
            implementation_model: choices[i].1.clone(),
            reviewer: choices[i + 1].0.label().to_lowercase(),
            review_model: choices[i + 1].1.clone(),
        };
        let step = |i: usize| Step {
            client: choices[i].0,
            model: choices[i].1.clone(),
            brief: BRIEFS[i - 4].into(),
        };
        Ok(Plan {
            repository: self.repo.clone(),
            work: self.work.clone(),
            stage: self.stage + 1,
            task: task.name.clone(),
            spec_sha: Some(task.sha.clone()),
            first: round(0),
            second: round(2),
            decision: String::new(),
            test: step(4),
            accept: step(5),
            deliver: step(6),
        })
    }
}
impl Studio {
    pub(super) fn open_delivery(&mut self, w: &mut Window, cx: &mut Context<Self>) {
        let result = (|| -> Result<State> {
            let p = self.board.library.project().context("请选择项目")?;
            let work = p.selected().context("请选择事项")?;
            let stage = flow::implementation_stage(work.template).context("无连续开发阶段")?;
            let root = flow::storage(
                self.board.dir.as_deref().context("项目库")?,
                &p.id,
                &work.id,
            )?;
            let jobs = flow::jobs(&root)?;
            let job = jobs.first().cloned();
            let repo = work
                .target(stage)
                .or_else(|| {
                    job.as_ref()
                        .and_then(|dir| read(&dir.join("launch.json")).ok())
                        .and_then(|p| p["repository"].as_str().map(PathBuf::from))
                })
                .context("请先选择事项仓库")?;
            let tasks = if job.is_none() {
                crate::relay::stage_tasks(&Workspace::open(&repo)?)?
            } else {
                vec![]
            };
            let choices = (0..7)
                .map(|i| {
                    let profile = work.profile(if i < 4 { stage } else { stage + i - 3 });
                    let c = if allowed(i).contains(&profile.client) {
                        profile.client
                    } else {
                        allowed(i)[0]
                    };
                    let model = Some(&profile)
                        .filter(|p| p.client == c && !p.model.is_empty())
                        .or_else(|| p.preset(c))
                        .map(|p| p.model.clone())
                        .filter(|m| !m.is_empty())
                        .unwrap_or_else(|| standard(c).first().unwrap_or(&"").to_string());
                    (c, model)
                })
                .collect::<Vec<_>>();
            let input = cx.new(|cx| {
                InputState::new(w, cx)
                    .placeholder("本轮模型 ID")
                    .default_value(choices[0].1.clone())
            });
            Ok(State {
                work: work.id.clone(),
                root,
                repo,
                stage,
                tasks,
                task: None,
                choices,
                slot: 0,
                input,
                jobs,
                job,
                preview: None,
            })
        })();
        match result {
            Ok(state) => {
                self.board.timeline = None;
                self.board.desk = None;
                self.board.delivery = Some(state);
                self.ensure_zcode_catalog(cx);
            }
            Err(e) => self.board.error = Some(format!("{e:#}")),
        }
        cx.notify();
    }
    fn launch_delivery(&mut self, resume: bool, cx: &mut Context<Self>) {
        let result = (|| -> Result<()> {
            let state = self.board.delivery.as_mut().context("未选中事项")?;
            let p = self.board.library.project().context("项目")?;
            let work = p
                .selected()
                .filter(|w| w.id == state.work)
                .context("事项已切换")?;
            let existing = state.job.as_deref().filter(|_| resume);
            let plan: Plan = if let Some(dir) = existing {
                serde_json::from_value(read(&dir.join("launch.json"))?)?
            } else {
                ensure!(work.stage == state.stage, "事项尚未进入开发实现阶段");
                let plan = state.plan(cx)?;
                ensure!(
                    crate::relay::stage_tasks(&Workspace::open(&state.repo)?)?
                        .iter()
                        .any(|t| t.name == plan.task && Some(&t.sha) == plan.spec_sha.as_ref()),
                    "任务 Spec 已变化，请重新打开「开发全流程」选择版本"
                );
                plan
            };
            ensure!(
                plan.work == work.id
                    && (state.stage..state.stage + 4)
                        .all(|i| work.target(i).as_ref() == Some(&plan.repository)),
                "请核对各阶段的目标仓库"
            );
            let dir = flow::launch(&state.root, &plan, existing, &self.runner)?;
            state.jobs = flow::jobs(&state.root)?;
            state.job = Some(dir);
            state.preview = None;
            Ok(())
        })();
        self.board.error = result.err().map(|e| format!("{e:#}"));
        cx.notify();
    }
    pub(super) fn delivery_page(&self, s: &State, work: &Work, cx: &Context<Self>) -> Div {
        let mut view = v_flex()
            .gap_3()
            .child(heading(format!("{} · 开发全流程", work.name)));
        if s.jobs.len() > 1 {
            let mut history = h_flex().gap_2().flex_wrap();
            for (i, path) in s.jobs.iter().enumerate() {
                let dir = path.clone();
                let label = read(&dir.join("launch.json"))
                    .ok()
                    .and_then(|v| v["task"].as_str().map(str::to_owned))
                    .unwrap_or_default();
                history = history.child(
                    button(
                        format!("delivery-history-{i}"),
                        format!("第 {} 次 · {label}", s.jobs.len() - i),
                        s.job.as_ref() == Some(path),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(s) = this.board.delivery.as_mut() {
                            s.job = Some(dir.clone());
                            s.preview = None;
                        }
                        cx.notify();
                    })),
                );
            }
            view = view.child(history);
        }
        if let Some(dir) = &s.job {
            view = view.child(self.delivery_record(s, work, dir, cx));
            if work.stage == s.stage
                && s.jobs.first().is_some_and(|p| {
                    read(&p.join("output/verified.json")).is_ok_and(|v| v["verified"] == true)
                })
            {
                view = view.child(button("delivery-new", "配置新的全流程", false).on_click(
                    cx.listener(|this, _, _, cx| {
                        if let Some(s) = this.board.delivery.as_mut() {
                            match Workspace::open(&s.repo)
                                .and_then(|ws| crate::relay::stage_tasks(&ws))
                            {
                                Ok(tasks) => {
                                    s.tasks = tasks;
                                    s.task = None;
                                    s.job = None;
                                    s.preview = None;
                                }
                                Err(e) => this.board.error = Some(format!("{e:#}")),
                            }
                        }
                        cx.notify();
                    }),
                ));
            }
            return view;
        }
        if work.stage != s.stage {
            return view.child(caption("事项进入开发实现阶段后可启动全流程。"));
        }
        if s.tasks.is_empty() {
            return view.child(caption("当前仓库未找到任务 Spec。请先提交 specs/<任务名>/task.json 和 spec.md，再打开此页。"));
        }
        let mut tasks = h_flex().gap_2().flex_wrap();
        for (i, task) in s.tasks.iter().enumerate() {
            tasks = tasks.child(
                button(
                    format!("delivery-spec-{}", task.name),
                    format!("{} · {}", task.name, &task.sha[..8]),
                    s.task == Some(i),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(s) = this.board.delivery.as_mut() {
                        s.task = Some(i);
                    }
                    cx.notify();
                })),
            );
        }
        view = view.child(
            panel()
                .child(heading("任务 Spec"))
                .child(caption(s.repo.display().to_string()))
                .child(tasks),
        );
        let mut slots = h_flex().gap_2().flex_wrap();
        for (i, label) in ROLES.iter().enumerate() {
            let model = if i == s.slot {
                s.input.read(cx).value().to_string()
            } else {
                s.choices[i].1.clone()
            };
            slots = slots.child(
                button(
                    format!("delivery-role-{i}"),
                    format!("{label} · {model}"),
                    i == s.slot,
                )
                .on_click(cx.listener(move |this, _, w, cx| {
                    if let Some(s) = this.board.delivery.as_mut() {
                        s.choices[s.slot].1 = s.input.read(cx).value().trim().into();
                        s.slot = i;
                        s.input
                            .update(cx, |v, cx| v.set_value(s.choices[i].1.clone(), w, cx));
                    }
                    cx.notify();
                })),
            );
        }
        let client = s.choices[s.slot].0;
        let mut clients = h_flex().gap_2();
        for &c in allowed(s.slot) {
            clients = clients.child(
                button(format!("delivery-client-{c:?}"), c.label(), c == client).on_click(
                    cx.listener(move |this, _, w, cx| {
                        let model = this
                            .board
                            .library
                            .project()
                            .and_then(|p| p.preset(c))
                            .map(|p| p.model.clone())
                            .filter(|m| !m.is_empty())
                            .unwrap_or_else(|| standard(c).first().unwrap_or(&"").to_string());
                        if let Some(s) = this.board.delivery.as_mut() {
                            s.choices[s.slot] = (c, model.clone());
                            s.input.update(cx, |v, cx| v.set_value(model, w, cx));
                        }
                        cx.notify();
                    }),
                ),
            );
        }
        let mut options = standard(client)
            .iter()
            .map(|s| (s.to_string(), s.to_string()))
            .collect::<Vec<_>>();
        if client == Client::ZCode
            && let Some(Ok(catalog)) = &self.board.zcode_catalog
        {
            options.extend(catalog.iter().map(|m| {
                (
                    format!(
                        "{} · {}",
                        m.provider_label.as_deref().unwrap_or(&m.id.provider_id),
                        m.label
                    ),
                    m.default_selection(),
                )
            }));
        }
        let mut models = h_flex().flex_wrap().gap_2();
        for (i, (name, value)) in options.into_iter().enumerate() {
            models = models.child(
                button(
                    format!("delivery-model-{i}"),
                    name,
                    s.input.read(cx).value().as_str() == value,
                )
                .on_click(cx.listener(move |this, _, w, cx| {
                    if let Some(s) = this.board.delivery.as_mut() {
                        s.input
                            .update(cx, |v, cx| v.set_value(value.clone(), w, cx));
                    }
                    cx.notify();
                })),
            );
        }
        view.child(
            panel()
                .child(heading("阶段模型"))
                .child(slots)
                .child(heading(ROLES[s.slot]))
                .child(clients)
                .child(models)
                .child(input_field("delivery-model-input", &s.input)),
        )
        .child(
            primary("delivery-start", "启动开发全流程")
                .on_click(cx.listener(|s, _, _, cx| s.launch_delivery(false, cx))),
        )
    }
}
impl Studio {
    fn delivery_record(&self, s: &State, work: &Work, dir: &Path, cx: &Context<Self>) -> Div {
        let output = dir.join("output");
        let mut view = v_flex().gap_3();
        let plan = match read(&dir.join("launch.json"))
            .and_then(|v| Ok(serde_json::from_value::<Plan>(v)?))
        {
            Ok(p) => p,
            Err(e) => return view.child(t(format!("{e:#}"), RED)),
        };
        let verified = read(&output.join("verified.json")).ok();
        let done = verified.as_ref().is_some_and(|v| v["verified"] == true);
        let live =
            flow::running(&s.root).unwrap_or(false) && s.jobs.first().is_some_and(|p| p == dir);
        let gate = ["deliver", "accept", "test", "relay"].iter().find_map(|k| {
            read(&output.join(k).join("waiting.json"))
                .ok()
                .map(|v| (*k, v))
        });
        let phase = ["deliver", "accept", "test", "relay"]
            .iter()
            .find(|k| output.join(k).join("input.json").is_file())
            .copied();
        let failure = read(&output.join("failure.json")).ok();
        let failed_stage = failure.as_ref().and_then(|f| f["stage"].as_str());
        let stage_title = match failed_stage {
            Some("first") => "第一轮实现与审查",
            Some("second") => "第二轮实现与审查",
            Some("relay") => "开发接力",
            Some("test") => "测试验证",
            Some("accept") => "独立验收",
            Some("deliver") => "交付归档",
            _ => "归档核验",
        };
        let audit_failed = verified.as_ref().is_some_and(|v| v["verified"] == false);
        let failed = !done && !live && (failure.is_some() || audit_failed);
        let status = if done {
            "核验通过"
        } else if failed {
            "执行失败"
        } else if !live {
            "已停止 · 可继续"
        } else if gate.is_some() {
            "等待 Studio 确认"
        } else {
            match phase {
                Some("relay") => "开发实现 · 两轮接力",
                Some("test") => "测试验证中",
                Some("accept") => "独立验收中",
                Some("deliver") => "交付归档中",
                _ => "正在启动",
            }
        };
        let mut summary = panel()
            .child(detail_text("delivery-status", status))
            .child(field("任务 Spec", &plan.task))
            .child(field(
                "第一轮",
                &format!(
                    "{} / {} → {} / {}",
                    plan.first.implementer,
                    plan.first.implementation_model,
                    plan.first.reviewer,
                    plan.first.review_model
                ),
            ))
            .child(field(
                "第二轮",
                &format!(
                    "{} / {} → {} / {}",
                    plan.second.implementer,
                    plan.second.implementation_model,
                    plan.second.reviewer,
                    plan.second.review_model
                ),
            ))
            .child(field(
                "测试 → 独立验收 → 交付",
                &format!(
                    "{} / {} → {} / {} → {} / {}",
                    plan.test.client.label(),
                    plan.test.model,
                    plan.accept.client.label(),
                    plan.accept.model,
                    plan.deliver.client.label(),
                    plan.deliver.model
                ),
            ));
        let mut progress = h_flex().gap_2().flex_wrap();
        for (title, phase, receipt) in [
            ("第一轮", "relay", "relay/first-run.json"),
            ("第二轮", "relay", "relay/second-run.json"),
            ("测试验证", "test", "test/destination.json"),
            ("独立验收", "accept", "accept/destination.json"),
            ("交付归档", "deliver", "deliver/archive.json"),
        ] {
            let complete = output.join(receipt).is_file();
            let current = !complete
                && output.join(phase).join("input.json").is_file()
                && (title != "第二轮" || output.join("relay/first-run.json").is_file());
            let (state, tone) = if complete {
                ("已完成", MINT)
            } else if current && failed {
                ("失败", RED)
            } else if current && live {
                ("进行中", BLUE)
            } else if current {
                ("已中断", AMBER)
            } else {
                ("待开始", MUTED)
            };
            progress = progress.child(chip(format!("{title} · {state}"), tone));
        }
        summary = summary.child(progress);
        if live && let Some((phase, g)) = gate {
            let repo = plan.repository.clone();
            let stage = s.stage;
            let kind = g["kind"].as_str().unwrap_or_default().to_owned();
            let relay = fs::read_to_string(output.join("relay/relay-id")).ok();
            let name = match kind.as_str() {
                "decision" => "修订决策并继续第二轮",
                "archive" => "查看并确认交付",
                _ => "查看验收并确认交接",
            };
            summary = summary
                .child(detail_text(
                    "delivery-waiting",
                    g["label"].as_str().unwrap_or(""),
                ))
                .child(
                    button(format!("delivery-gate-{phase}"), name, true).on_click(cx.listener(
                        move |this, _, w, cx| {
                            this.section(Section::Work, w, cx);
                            match kind.as_str() {
                                "decision" => {
                                    this.open_stage_relay(&repo, stage, w, cx);
                                    if let Some(id) = &relay {
                                        // Relay data loads in the background. sync_relay_form restores the saved round models.
                                        this.relay_id = Some(id.clone());
                                    }
                                }
                                "archive" => {}
                                _ => this.load_timeline(cx),
                            }
                        },
                    )),
                );
        }
        if failed {
            let reason = failure
                .as_ref()
                .and_then(|f| f["reason"].as_str())
                .or_else(|| verified.as_ref().and_then(|v| v["error"].as_str()))
                .unwrap_or("请查看执行日志");
            summary = summary
                .child(detail_text("delivery-failed-step", stage_title))
                .child(detail_text("delivery-failure-reason", reason).text_color(rgb(RED)));
            if let Some(run) = failure.as_ref().and_then(|f| f["run"].as_str()) {
                summary = summary.child(field("失败执行", run));
            }
        }
        if !live && !done && s.jobs.first().is_some_and(|p| p == dir) {
            summary = summary.child(
                button(
                    "delivery-resume",
                    if failed {
                        format!("从{stage_title}重试")
                    } else {
                        "继续原流程".into()
                    },
                    true,
                )
                .on_click(cx.listener(|s, _, _, cx| s.launch_delivery(true, cx))),
            );
        }
        if let Some(v) = &verified {
            if done {
                summary = summary.child(detail_text(
                    "delivery-result",
                    &format!(
                        "决策 #{} · {} 个通过回执 · {} 次失败尝试 · {} 份证据 · {} 张截图",
                        v["decision"],
                        v["runs"].as_array().map_or(0, Vec::len),
                        v["failed_attempts"].as_array().map_or(0, Vec::len),
                        v["checked_artifacts"],
                        v["screenshots"].as_array().map_or(0, Vec::len)
                    ),
                ));
            } else {
                summary =
                    summary.child(t(v["error"].as_str().unwrap_or("核验失败").to_owned(), RED));
            }
        }
        let log = workflow::tail(&dir.join("runner.log"), 1600);
        view = view
            .child(summary)
            .child(panel().child(heading("执行日志")).child(t(log, MUTED)));
        let manifest = read(&output.join("evidence/manifest.json")).ok();
        if let Some(v) = verified.filter(|_| done) {
            let mut files = h_flex().gap_2().flex_wrap();
            let mut links = vec![
                ("核验报告".to_owned(), "verified.json".to_owned()),
                ("归档清单".into(), "evidence/manifest.json".into()),
            ];
            for path in v["failed_attempts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                links.push((
                    format!("失败原因 · {}", path.trim_end_matches(".json")),
                    path.to_owned(),
                ));
            }
            if let Some(man) = manifest
                && let Some(rows) = man["runs"].as_array()
            {
                for (i, r) in rows.iter().enumerate() {
                    let stage = r["stage_index"]
                        .as_u64()
                        .and_then(|i| work.template.stages().get(i as usize))
                        .map_or("执行", |stage| stage.name);
                    let path = r["directory"].as_str().unwrap_or("");
                    for (name, label) in [
                        ("result.txt", "阶段报告"),
                        ("review.txt", "审查"),
                        ("changes.patch", "变更"),
                        ("checks.log", "验收日志"),
                        ("inputs/source-evidence.json", "来源回执"),
                        ("handoff.json", "交接"),
                        ("evidence.json", "回执"),
                    ] {
                        let rel = format!("{path}/{name}");
                        if man["files"].get(&rel).is_some() {
                            links.push((
                                format!("{stage} {} · {label}", i + 1),
                                format!("evidence/{rel}"),
                            ));
                        }
                    }
                }
            }
            for (i, (label, path)) in links.into_iter().enumerate() {
                files = files.child(self.proof_button(
                    format!("delivery-proof-{i}"),
                    label,
                    &output,
                    path,
                    cx,
                ));
            }
            let mut shots = h_flex().gap_2().flex_wrap();
            for (i, path) in v["screenshots"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .enumerate()
            {
                shots = shots.child(self.proof_button(
                    format!("delivery-shot-{i}"),
                    screenshot_label(path),
                    &output,
                    path.into(),
                    cx,
                ));
            }
            let folder = output.clone();
            view = view
                .child(
                    panel().child(heading("归档证据")).child(files).child(
                        button("delivery-folder", "在 Finder 中打开归档", false)
                            .on_click(cx.listener(move |_, _, _, cx| cx.reveal_path(&folder))),
                    ),
                )
                .child(panel().child(heading("Studio 截图")).child(shots));
        }
        if let Some(path) = &s.preview {
            let box_ = panel().child(detail_text(
                "delivery-preview",
                path.file_name().and_then(|n| n.to_str()).unwrap_or("预览"),
            ));
            view = view.child(if path.extension().is_some_and(|e| e == "png") {
                box_.child(
                    img(path.clone())
                        .w_full()
                        .h(px(400.))
                        .object_fit(ObjectFit::Contain),
                )
            } else {
                box_.child(t(workflow::tail(path, 24000), FG))
            });
        }
        view
    }
    fn proof_button(
        &self,
        id: String,
        label: String,
        root: &Path,
        path: String,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let dir = root.to_owned();
        let selected = self
            .board
            .delivery
            .as_ref()
            .and_then(|s| s.preview.as_deref())
            == Some(root.join(&path).as_path());
        button(id, label, selected).on_click(cx.listener(move |this, _, _, cx| {
            match flow::asset(&dir, &path) {
                Ok(p) => {
                    if let Some(s) = this.board.delivery.as_mut() {
                        s.preview = Some(p)
                    }
                }
                Err(e) => this.board.error = Some(format!("{e:#}")),
            };
            cx.notify();
        }))
    }
}
