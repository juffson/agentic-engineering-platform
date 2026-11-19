mod github;

use super::{Args, Driver, checked_run, handoff_fixture};
use aep::{
    studio::Snapshot,
    workspace::{Workspace, commit, git, read, status},
};
use anyhow::{Context, Result, ensure};
use github::{Fixture, command, remote_ref};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    io::{self, IsTerminal, Write},
    path::Path,
};

const DEFAULT_DECISION: &str =
    "Round future fractional seconds to the nearest millisecond, with ties to even.";

pub(super) fn run(mut args: Args) -> Result<()> {
    ensure!(
        io::stdin().is_terminal(),
        "--live-lifecycle needs a terminal for the decision and two confirmations"
    );
    let parent = args
        .output_dir
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("target/live-lifecycle"));
    fs::create_dir_all(&parent)?;
    let root = tempfile::Builder::new()
        .prefix("studio-")
        .tempdir_in(parent.canonicalize()?)?
        .keep();
    let out = root.join("evidence");
    fs::create_dir(&out)?;
    println!("Live Studio result: {}", root.display());
    args.output_dir = Some(out.clone());
    args.screenshots = true;
    let result = (|| {
        let binary = handoff_fixture::build_runner()?;
        let (fixture, ws) = Fixture::prepare(&out, &root, &binary)?;
        println!("Isolated GitHub target: {}/{}", fixture.repo, fixture.base);
        let runner = aep_studio::Runner {
            binary: Some(binary),
            client_bin: None,
        };
        let mut ui = Driver::open_with_runner(&ws, &args, runner)?;
        let result = exercise(&mut ui, &ws, &fixture, &out, args.codex_model.as_deref());
        if result.is_err() {
            let _ = ui.checkpoint("failure");
        }
        result
    })();
    if let Err(error) = &result {
        fs::write(out.join("failure.txt"), format!("{error:#}\n"))?;
    }
    let archived = archive(&root, &out);
    if let Err(error) = &archived {
        eprintln!("Could not package evidence: {error:#}");
    }
    result?;
    archived?;
    println!(
        "Workflow verified. Evidence: {}",
        root.join("evidence.tar.gz").display()
    );
    Ok(())
}

fn exercise(
    ui: &mut Driver<'_>,
    ws: &Workspace,
    fixture: &Fixture,
    out: &Path,
    model: Option<&str>,
) -> Result<()> {
    ui.click("两轮接力")?;
    let task = format!("relay-task-{}", fixture.task);
    if !ui.element(&task)?.visible() {
        ui.reveal("relay-list", &task)?;
    }
    ui.click(&task)?;
    if let Some(model) = model {
        ui.click("relay-model")?;
        ui.input(model)?;
    }
    ui.checkpoint("01-task-selected")?;
    println!("Starting round one with live Codex and Claude.");
    ui.click("start-relay")?;
    let entry = fs::read_dir(ws.data.join("relays"))?
        .next()
        .context("no relay created")??;
    let relay_path = entry.path().join("relay.json");
    let relay = read(&relay_path)?;
    ensure!(
        relay["task"]["name"] == fixture.task && relay["task"]["sha"] == fixture.spec,
        "Studio selected a different task version"
    );
    let first_id = field(&relay["attempts"][0], "id")?;
    let first = finished_run(ws, first_id).context("first round")?;
    ui.checkpoint("02-round-one-passed")?;

    let (question, decision, reference) = revise(ui, ws, out, first_id)?;
    println!("Starting round two on the first candidate with decision #{decision}.");
    ui.click(&format!("relay-continue-{decision}"))?;
    let session = read(&relay_path)?;
    let second_id = field(&session["attempts"][1], "id")?;
    let second = finished_run(ws, second_id).context("second round")?;
    ensure!(
        first["spec_sha"] == fixture.spec
            && second["spec_sha"] == fixture.spec
            && first["candidate_sha"] == second["requested_candidate_sha"]
            && second["request"]["require_memory"] == decision,
        "second round did not use the first candidate and fixed decision"
    );
    for role in ["implementer", "reviewer"] {
        let inputs = second["memory_inputs"][role]
            .as_array()
            .context("missing memory inputs")?;
        ensure!(
            inputs.contains(&json!(decision)) && !inputs.contains(&json!(question)),
            "{role} did not receive the revised decision instead of the original question"
        );
    }
    handoff_fixture::verify_reference(ws, second_id, &second, &reference)?;
    save(
        out,
        "relay-verification.json",
        &json!({
            "relay": relay["id"], "spec_sha": fixture.spec, "first_run": first_id, "second_run": second_id,
            "question": question, "decision": decision, "reference": reference,
            "second_round_memory_inputs": second["memory_inputs"]
        }),
    )?;
    ui.reveal("relay-detail", "relay-second-evidence")?;
    ui.checkpoint("06-round-two-passed")?;
    ui.click("relay-second-evidence")?;
    ui.click(&format!("memory_inputs-reviewer-{decision}"))?;
    ui.reveal(
        &format!("detail-memory-{decision}"),
        &format!("read-run-{second_id}"),
    )?;
    ensure!(
        ui.element(&format!("read-roles-{second_id}"))?.label() == Some("实现 Agent、审查 Agent"),
        "Studio did not show both readers of the decision"
    );
    ui.checkpoint("07-decision-readers")?;
    ui.click("两轮接力")?;

    let candidate = field(&second, "candidate_sha")?;
    let pr = publish(ui, ws, fixture, out, second_id, candidate)?;
    let merged = merge(ui, ws, fixture, out, second_id, candidate, pr)?;
    finish(ui, ws, fixture, first_id, second_id, &merged)?;
    fixture.retire_dispatch(ws)?;
    save(
        out,
        "verification.json",
        &json!({
            "status": "passed", "task": fixture.task, "spec_sha": fixture.spec, "target": fixture.base,
            "original_target_sha": fixture.base_sha, "first_run": first_id, "second_run": second_id,
            "old_question": question, "decision": decision, "decision_reference": reference,
            "candidate": candidate, "pr": pr, "url": merged["url"], "merge_sha": merged["mergeCommit"]["oid"],
            "local_target_synced": true, "candidate_branch_removed": true, "run_worktrees_removed": true,
            "evidence_memory_and_receipts_preserved": true, "temporary_task_dispatch_removed": true
        }),
    )?;
    Ok(())
}

fn finished_run(ws: &Workspace, id: &str) -> Result<Value> {
    checked_run(ws, id).with_context(|| {
        let record = status(&ws.data, id).unwrap_or_default();
        format!(
            "Run {id} stopped in {}: {}",
            record["phase"], record["error"]
        )
    })
}

fn revise(
    ui: &mut Driver<'_>,
    ws: &Workspace,
    out: &Path,
    first: &str,
) -> Result<(i64, i64, String)> {
    let notes = Snapshot::load(ws).notes;
    let question = notes
        .iter()
        .filter(|n| {
            n["run_id"] == first
                && n["kind"] == "open_question"
                && n["scope"] == "task"
                && n["successor"].is_null()
        })
        .min_by_key(|n| n["author"] != "implementer")
        .context("neither first-round agent saved a task question")?;
    let id = question["id"].as_i64().context("question ID")?;
    save(out, "question.json", question)?;
    let resolve = format!("relay-resolve-{id}");
    ui.reveal("relay-detail", &resolve)?;
    ui.checkpoint("03-first-question")?;
    ui.click(&resolve)?;
    println!("First-round question #{id}: {}", field(question, "body")?);
    println!("Suggested decision: {DEFAULT_DECISION}");
    let input = prompt("Enter the decision for Studio to save (Enter accepts the suggestion): ")?;
    let text = if input.is_empty() {
        DEFAULT_DECISION
    } else {
        &input
    };
    let reference = format!("decision-{}", uuid::Uuid::new_v4().simple());
    let body = format!("{text} Decision reference {reference}.");
    ensure!(
        body.chars().count() <= 1000,
        "the decision and its reference exceed Studio's 1000-character limit"
    );
    ui.replace_focused(&body)?;
    ui.checkpoint("04-studio-revision")?;
    ui.click("save-note")?;
    let revised = Snapshot::load(ws)
        .notes
        .into_iter()
        .find(|n| n["replaces"] == id && n["kind"] == "decision")
        .context("Studio did not save the revision")?;
    ensure!(
        revised["author"] == "manual"
            && revised["body"] == body
            && revised["scope"] == question["scope"]
            && revised["task"] == question["task"]
            && revised["spec_sha"] == question["spec_sha"],
        "Studio saved the wrong decision"
    );
    let decision = revised["id"].as_i64().context("decision ID")?;
    save(out, "decision.json", &revised)?;
    ui.reveal("relay-detail", &format!("relay-continue-{decision}"))?;
    ui.checkpoint("05-decision-saved")?;
    Ok((id, decision, reference))
}

fn publish(
    ui: &mut Driver<'_>,
    ws: &Workspace,
    fixture: &Fixture,
    out: &Path,
    second: &str,
    candidate: &str,
) -> Result<u64> {
    ui.reveal("relay-detail", "open-human-review")?;
    ui.click("open-human-review")?;
    ui.click("review-target-input")?;
    ui.input(&fixture.base)?;
    ensure!(
        ui.absent("acknowledge-review")?,
        "editing the target kept the old review actionable"
    );
    ui.click("load-pr-target")?;
    ensure!(
        ui.element("review-target-branch")?.label() == Some(fixture.base.as_str()),
        "Studio review targets a different branch"
    );
    let preview = commit(&ws.root, &format!("refs/aep/publish/{second}/preview"))?;
    ensure!(
        preview == fixture.base_sha,
        "isolated target moved before review"
    );

    let files = git(
        &ws.root,
        &[
            "diff",
            "--name-only",
            "--no-renames",
            &preview,
            candidate,
            "--",
        ],
    )?;
    let expected = ["spec.md", "task.json", "normalize.rs", "acceptance.rs"]
        .map(|name| format!("specs/{}/{name}", fixture.task));
    ensure!(
        files.lines().collect::<BTreeSet<_>>() == expected.iter().map(String::as_str).collect(),
        "candidate changes files outside the synthetic fixture"
    );
    let diff = git(
        &ws.root,
        &[
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            &preview,
            candidate,
            "--",
        ],
    )?;
    fs::write(out.join("reviewed.diff"), diff)?;
    let reference = format!("refs/heads/{}", fixture.head_branch(second));
    ensure!(
        remote_ref(ws, &reference)?.is_empty() && ui.absent("publish-confirmed")?,
        "candidate became publishable before confirmation"
    );
    ui.checkpoint("08-final-diff")?;
    ui.click("review-checks")?;
    ensure!(
        ui.element("review-content")?
            .label()
            .is_some_and(|s| s.contains("test result: ok")),
        "missing Studio acceptance log"
    );
    ui.checkpoint("09-acceptance-log")?;
    ui.click("review-report")?;
    ui.checkpoint("10-claude-review")?;
    let run = ws.data.join("runs").join(second);
    println!(
        "Review {} and the Studio screenshots before publication.",
        out.join("reviewed.diff").display()
    );
    println!(
        "Acceptance: {}\nClaude review: {}",
        run.join("checks.log").display(),
        run.join("review.txt").display()
    );
    confirm(
        &format!(
            "Publish {candidate} to the isolated target {}?",
            fixture.base
        ),
        "publish",
    )?;
    ui.click("review-diff")?;
    ui.click("acknowledge-review")?;
    ui.checkpoint("11-publication-confirmed")?;
    ui.click("publish-confirmed")?;
    let publication = read(&run.join("publication.json"))?;
    if let Some(error) = publication["error"].as_str() {
        anyhow::bail!("Studio publication failed: {error}");
    }
    let number = fixture.check_publication(&publication, second, candidate)?;
    ui.checkpoint("12-pr-created")?;
    ui.click("track-created-pr")?;
    ui.checkpoint("13-github-ci-started")?;
    println!("Created https://github.com/{}/pull/{number}", fixture.repo);
    Ok(number)
}

fn merge(
    ui: &mut Driver<'_>,
    ws: &Workspace,
    fixture: &Fixture,
    out: &Path,
    second: &str,
    candidate: &str,
    number: u64,
) -> Result<Value> {
    let ci = fixture.wait_ci(ws, out, number, candidate, second);
    ui.click("refresh-github-pr")?;
    let checked = ci?;
    ensure!(
        ui.element("github-confirm-merge")?.visible() && ui.absent("github-merge-now")?,
        "Studio did not present the human confirmation gate after CI"
    );
    ui.checkpoint("14-github-ci-passed")?;
    ui.click("github-review-tab")?;
    ui.checkpoint("15-github-review-tab")?;
    confirm(
        &format!(
            "Merge PR #{number} at {candidate} into {}? Review {} and screenshots 14–15.",
            fixture.base,
            field(&checked, "url")?
        ),
        "merge",
    )?;
    ui.click("github-confirm-merge")?;
    ui.checkpoint("16-merge-confirmed")?;
    ui.click("github-merge-now")?;
    if ui.absent("github-merged")? {
        let error = ui
            .element("github-pr-error")?
            .label()
            .unwrap_or("no merge receipt")
            .to_owned();
        anyhow::bail!("Studio merge failed: {error}");
    }
    ui.checkpoint("17-github-merged")?;
    let record = read(&ws.data.join("runs").join(second).join("merge.json"))?;
    let merged = fixture.pr(ws, number, candidate, second)?;
    save(out, "github-merged.json", &merged)?;
    ensure!(
        merged["state"] == "MERGED"
            && record["number"] == number
            && record["head"] == candidate
            && record["base"] == fixture.base_sha
            && record["confirmed_by"] == fixture.actor
            && merged["mergedBy"]["login"] == fixture.actor
            && field(&record, "merged_sha")? == field(&merged["mergeCommit"], "oid")?,
        "GitHub merge did not match the confirmed PR"
    );
    Ok(merged)
}

fn finish(
    ui: &mut Driver<'_>,
    ws: &Workspace,
    fixture: &Fixture,
    first: &str,
    second: &str,
    merged: &Value,
) -> Result<()> {
    let run = ws.data.join("runs").join(second);
    let preserve = [
        ws.data.join("runs").join(first).join("evidence.json"),
        run.join("evidence.json"),
        ws.data.join("memory.sqlite3"),
        run.join("publication.json"),
        run.join("merge.json"),
    ];
    let before = preserve
        .iter()
        .map(fs::read)
        .collect::<io::Result<Vec<_>>>()?;
    ui.reveal("github-merge-card", "github-finish-merge")?;
    ui.checkpoint("18-cleanup-ready")?;
    ui.click("github-finish-merge")?;
    ui.reveal("github-merge-card", "github-cleanup-complete")?;
    ui.checkpoint("19-cleanup-complete")?;
    let cleaned = read(&run.join("cleanup.json"))?;
    let merge_sha = field(&merged["mergeCommit"], "oid")?;
    ensure!(
        cleaned["base"] == fixture.base
            && cleaned["merged_sha"] == merge_sha
            && cleaned["target"]["done"] == true
            && cleaned["branch"]["done"] == true,
        "Studio did not complete sync and candidate cleanup"
    );
    ensure!(
        git(&ws.root, &["symbolic-ref", "--short", "HEAD"])?.trim() == fixture.base
            && commit(&ws.root, "HEAD")? == merge_sha
            && remote_ref(ws, &format!("refs/heads/{}", fixture.base))? == merge_sha
            && remote_ref(ws, &format!("refs/heads/{}", fixture.head_branch(second)))?.is_empty(),
        "Git does not match Studio's cleanup receipt"
    );
    let worktrees = git(&ws.root, &["worktree", "list", "--porcelain"])?;
    let receipts = cleaned["worktrees"]
        .as_array()
        .context("missing Worktree cleanup receipts")?;
    for id in [first, second] {
        let evidence = status(&ws.data, id)?;
        let path = field(&evidence, "worktree")?;
        ensure!(
            receipts
                .iter()
                .any(|r| r["run"] == id && r["step"]["done"] == true)
                && !Path::new(path).try_exists()?
                && !worktrees
                    .lines()
                    .any(|line| line.strip_prefix("worktree ") == Some(path)),
            "Run {id} still has a temporary Worktree"
        );
    }
    for (path, expected) in preserve.iter().zip(before) {
        ensure!(
            fs::read(path)? == expected,
            "cleanup changed {}",
            path.display()
        );
    }
    ui.click("github-back-to-relay")?;
    ensure!(
        ui.element("relay-second-progress")?.label() == Some("合并与收尾已完成"),
        "Studio did not show the completed relay"
    );
    ui.checkpoint("20-relay-complete")
}

fn prompt(message: &str) -> Result<String> {
    print!("{message}");
    io::stdout().flush()?;
    let mut response = String::new();
    ensure!(
        io::stdin().read_line(&mut response)? != 0,
        "operator input closed"
    );
    Ok(response.trim().to_owned())
}

fn confirm(message: &str, word: &str) -> Result<()> {
    println!("{message}");
    ensure!(
        prompt(&format!("Type {word} to confirm: "))? == word,
        "operator declined {word}"
    );
    Ok(())
}

fn archive(root: &Path, out: &Path) -> Result<()> {
    let repo = root.join("workspace/repo");
    if repo.join(".git").is_dir() {
        let ws = Workspace::open(&repo)?;
        let runs = ws.data.join("runs");
        if runs.is_dir() {
            let ids = fs::read_dir(runs)?
                .map(|entry| {
                    let name = entry?.file_name();
                    name.into_string()
                        .map_err(|_| anyhow::anyhow!("invalid Run directory"))
                })
                .collect::<Result<Vec<_>>>()?;
            handoff_fixture::export_runs(&ws, &out.join("runs"), ids.iter().map(String::as_str))?;
        }
        let relays = ws.data.join("relays");
        if relays.is_dir() {
            for entry in fs::read_dir(relays)? {
                let source = entry?.path().join("relay.json");
                if source.is_file() {
                    fs::copy(source, out.join("relay.json"))?;
                }
            }
        }
        let memory = ws.data.join("memory.sqlite3");
        if memory.is_file() {
            fs::copy(memory, out.join("memory.sqlite3"))?;
        }
    }
    let mut shots = fs::read_dir(out)?
        .filter_map(|entry| {
            let path = entry.ok()?.file_name().into_string().ok()?;
            path.ends_with(".png").then_some(path)
        })
        .collect::<Vec<_>>();
    shots.sort();
    let mut index = String::from("# Studio live lifecycle evidence\n\n");
    for (file, label) in [
        ("verification.json", "Verification"),
        ("failure.txt", "Failure"),
        ("relay-verification.json", "Relay and memory reads"),
        ("reviewed.diff", "Reviewed diff"),
        ("github-ci.json", "GitHub checks"),
        ("github-actions.json", "GitHub Actions jobs"),
        ("github-acceptance.log", "GitHub acceptance log"),
        ("github-merged.json", "GitHub merge"),
        ("runs", "Execution evidence and Studio receipts"),
    ] {
        if out.join(file).exists() {
            index.push_str(&format!("- [{label}]({file})\n"));
        }
    }
    index.push_str("\n## Studio screenshots\n\n");
    for file in shots {
        index.push_str(&format!(
            "- [{file}]({file}) · [controls]({}.json)\n",
            file.trim_end_matches(".png")
        ));
    }
    fs::write(out.join("index.md"), index)?;
    command("tar", root, &["-czf", "evidence.tar.gz", "evidence"])?;
    println!("Evidence index: {}", out.join("index.md").display());
    Ok(())
}

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("missing {key}"))
}

fn save(out: &Path, name: &str, data: &impl Serialize) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(out)?;
    serde_json::to_writer_pretty(&mut file, data)?;
    file.persist(out.join(name))?;
    Ok(())
}
