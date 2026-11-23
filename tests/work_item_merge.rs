#[path = "support/work_item_merge.rs"]
mod fixture;
use aep::workspace::{commit, git};
use anyhow::{Result, ensure};
use serde_json::Value;
use std::{
    fs,
    process::{Command, Output, Stdio},
};
fn command(f: &fixture::Fixture, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_aep"));
    c.current_dir(&f.repo)
        .env_remove("AEP_PROJECT")
        .env_remove("AEP_WORK")
        .env_remove("AEP_PROJECT_HOME")
        .args(["project", "--home"])
        .arg(&f.home)
        .args(["--project", &f.delivery.project, "--work", &f.delivery.work])
        .args(args);
    c
}
fn parse(out: Output) -> Result<Value> {
    ensure!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(serde_json::from_slice(&out.stdout)?)
}
#[test]
fn cli_preview_confirm_reopen_and_concurrent_confirmation() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let f = fixture::create(tmp.path())?;
    let refs = git(&f.repo, &["for-each-ref"])?;
    let reflog_before = git(&f.repo, &["reflog", "show", "--format=%H", "target"])?;
    let archive = f.delivery.directory(&f.home);
    let mut sealed = Vec::new();
    let mut directories = vec![archive.clone()];
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            if path.is_dir() {
                directories.push(path);
            } else {
                sealed.push((path.clone(), fs::read(path)?));
            }
        }
    }
    let bytes = fs::read(f.home.join("projects.json"))?;
    let args = [
        "merge-preview",
        "--delivery",
        &f.delivery.run,
        "--target",
        "target",
    ];
    let p = parse(command(&f, &args).output()?)?;
    let digest = p["diff_sha256"].as_str().unwrap();
    assert_eq!(p["candidate"]["sha"], f.candidate);
    assert_eq!(p["branch"]["tip"], f.base);
    assert_eq!(
        p["stat"],
        git(
            &f.repo,
            &[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                "--stat",
                &f.base,
                &f.candidate,
                "--"
            ],
        )?
    );
    assert_eq!(refs, git(&f.repo, &["for-each-ref"])?);
    assert_eq!(bytes, fs::read(f.home.join("projects.json"))?);
    let bad = command(
        &f,
        &[
            "merge",
            "--delivery",
            &f.delivery.run,
            "--target",
            "target",
            "--diff-sha256",
            "stale",
        ],
    )
    .output()?;
    assert!(!bad.status.success());
    assert_eq!(commit(&f.repo, "target")?, f.base);
    let args = [
        "merge",
        "--delivery",
        &f.delivery.run,
        "--target",
        "target",
        "--diff-sha256",
        digest,
    ];
    let a = command(&f, &args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let b = command(&f, &args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let (a, b) = (a.wait_with_output()?, b.wait_with_output()?);
    ensure!(
        a.status.success() || b.status.success(),
        "both concurrent confirmations failed"
    );
    let r = parse(command(&f, &args).output()?)?;
    for output in [a, b].into_iter().filter(|o| o.status.success()) {
        assert_eq!(parse(output)?, r);
    }
    assert_eq!(r["merged_sha"], f.candidate);
    assert_eq!(r["mode"], "update-ref");
    for field in [
        "repository",
        "target_branch",
        "before_sha",
        "candidate_sha",
        "merged_sha",
        "mode",
        "diff_sha256",
        "files_changed",
        "insertions",
        "deletions",
        "intent_at",
        "merged_at",
        "operator",
    ] {
        assert!(!r[field].is_null(), "missing receipt field: {field}");
    }
    let reflog_after = git(&f.repo, &["reflog", "show", "--format=%H", "target"])?;
    assert_eq!(
        reflog_after.lines().count(),
        reflog_before.lines().count() + 1
    );
    for (path, bytes) in sealed {
        assert_eq!(fs::read(path)?, bytes);
    }
    let receipt = fs::read(archive.join("merge.json"))?;
    assert_eq!(parse(command(&f, &args).output()?)?, r);
    assert_eq!(
        git(&f.repo, &["reflog", "show", "--format=%H", "target"])?,
        reflog_after
    );
    assert_eq!(fs::read(archive.join("merge.json"))?, receipt);
    let status = parse(command(&f, &["status"]).output()?)?;
    assert_eq!(status["deliveries"][0]["merge"], r);
    let timeline = parse(command(&f, &["timeline"]).output()?)?;
    assert_eq!(timeline["merge_events"][0], r);
    let snapshot = f.delivery.snapshot(&f.home)?;
    assert_eq!(snapshot.merge_events.len(), 1);
    assert_eq!(commit(&f.repo, "HEAD")?, f.base);
    assert_eq!(fs::read_to_string(f.repo.join("file.txt"))?, "base\n");
    Ok(())
}

#[test]
fn cli_rejects_changed_candidate_and_unaccepted_workspace_changes() -> Result<()> {
    for scenario in ["changed-ref", "untracked", "committed"] {
        let tmp = tempfile::tempdir()?;
        let f = fixture::create(tmp.path())?;
        let preview_args = [
            "merge-preview",
            "--delivery",
            &f.delivery.run,
            "--target",
            "target",
        ];
        let p = parse(command(&f, &preview_args).output()?)?;
        let stage = tmp.path().join("stage");
        let reason = if scenario == "changed-ref" {
            git(
                &f.repo,
                &["update-ref", "refs/aep/runs/relay-fixture-r2-a1", &f.base],
            )?;
            "已验收提交引用已改变"
        } else {
            fs::write(stage.join("unaccepted.txt"), "not accepted\n")?;
            if scenario == "committed" {
                git(&stage, &["add", "unaccepted.txt"])?;
                git(&stage, &["commit", "-m", "unaccepted change"])?;
            }
            "验收后有未纳入的改动"
        };
        let refs = git(&f.repo, &["for-each-ref"])?;
        let catalog = fs::read(f.home.join("projects.json"))?;
        for output in [
            command(&f, &preview_args).output()?,
            command(
                &f,
                &[
                    "merge",
                    "--delivery",
                    &f.delivery.run,
                    "--target",
                    "target",
                    "--diff-sha256",
                    p["diff_sha256"].as_str().unwrap(),
                ],
            )
            .output()?,
        ] {
            assert!(!output.status.success(), "{scenario} was accepted");
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(error.contains(reason), "{scenario}: {error}");
            if scenario != "changed-ref" {
                assert!(error.contains("unaccepted.txt"), "{error}");
                assert!(error.contains(stage.to_str().unwrap()), "{error}");
            }
        }
        assert_eq!(git(&f.repo, &["for-each-ref"])?, refs);
        assert_eq!(fs::read(f.home.join("projects.json"))?, catalog);
        assert!(!f.delivery.directory(&f.home).join("merge.json").exists());
    }
    Ok(())
}

#[test]
fn cli_lists_only_local_branch_states_and_marks_dirty_main() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let f = fixture::create(tmp.path())?;
    let tree = git(&f.repo, &["rev-parse", "main^{tree}"])?;
    let diverged = git(
        &f.repo,
        &["commit-tree", tree.trim(), "-p", &f.base, "-m", "diverged"],
    )?;
    git(&f.repo, &["branch", "diverged", diverged.trim()])?;
    git(&f.repo, &["branch", "linked", &f.base])?;
    git(&f.repo, &["branch", "aep-stage/internal", &f.base])?;
    git(
        &f.repo,
        &[
            "worktree",
            "add",
            tmp.path().join("linked").to_str().unwrap(),
            "linked",
        ],
    )?;
    git(
        &f.repo,
        &[
            "update-ref",
            "refs/remotes/origin/remote-only",
            &f.candidate,
        ],
    )?;
    let args = ["merge-targets", "--delivery", &f.delivery.run];
    let branches = parse(command(&f, &args).output()?)?;
    let rows = branches.as_array().unwrap();
    for (name, status) in [
        ("target", "fast_forward"),
        ("build", "already_contained"),
        ("diverged", "diverged"),
        ("linked", "unwritable"),
    ] {
        let row = rows.iter().find(|r| r["name"] == name).unwrap();
        assert_eq!(row["status"], status, "{name}");
        if name == "diverged" {
            assert_eq!(row["merge_base"], f.base);
        }
    }
    assert!(
        rows.iter()
            .all(|r| !r["name"].as_str().unwrap().contains("remote-only"))
    );
    assert!(
        rows.iter()
            .find(|r| r["name"] == "aep-stage/internal")
            .unwrap()["internal"]
            == true
    );
    fs::write(f.repo.join("untracked.txt"), "dirty main\n")?;
    let branches = parse(command(&f, &args).output()?)?;
    assert_eq!(
        branches
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == "main")
            .unwrap()["status"],
        "unwritable"
    );
    Ok(())
}

#[test]
fn cli_stale_preview_after_appended_commit_preserves_target_and_catalog() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let f = fixture::create(tmp.path())?;
    let p = parse(
        command(
            &f,
            &[
                "merge-preview",
                "--delivery",
                &f.delivery.run,
                "--target",
                "target",
            ],
        )
        .output()?,
    )?;
    let tree = git(&f.repo, &["rev-parse", "target^{tree}"])?;
    let appended = git(
        &f.repo,
        &[
            "commit-tree",
            tree.trim(),
            "-p",
            &f.base,
            "-m",
            "target advanced",
        ],
    )?;
    git(
        &f.repo,
        &["update-ref", "refs/heads/target", appended.trim(), &f.base],
    )?;
    let refs = git(&f.repo, &["for-each-ref"])?;
    let catalog = fs::read(f.home.join("projects.json"))?;
    let output = command(
        &f,
        &[
            "merge",
            "--delivery",
            &f.delivery.run,
            "--target",
            "target",
            "--diff-sha256",
            p["diff_sha256"].as_str().unwrap(),
        ],
    )
    .output()?;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("重新查看 Diff"));
    assert_eq!(commit(&f.repo, "target")?, appended.trim());
    assert_eq!(git(&f.repo, &["for-each-ref"])?, refs);
    assert_eq!(fs::read(f.home.join("projects.json"))?, catalog);
    assert!(!f.delivery.directory(&f.home).join("merge.json").exists());
    Ok(())
}

#[test]
fn cli_keeps_two_archives_of_one_work_item_and_their_receipts_independent() -> Result<()> {
    use aep::project::{Library, save_json};
    let tmp = tempfile::tempdir()?;
    let f = fixture::create(tmp.path())?;
    let mut second = f.delivery.clone();
    second.run = "delivery-second".into();
    let original_dir = f.delivery.directory(&f.home);
    let second_dir = second.directory(&f.home);
    let mut directories = vec![original_dir.clone()];
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            let relative = path.strip_prefix(&original_dir)?.to_str().unwrap();
            let destination = second_dir.join(relative.replace(&f.delivery.run, &second.run));
            if path.is_dir() {
                fs::create_dir_all(destination)?;
                directories.push(path);
            } else {
                fs::create_dir_all(destination.parent().unwrap())?;
                fs::write(
                    destination,
                    fs::read_to_string(path)?.replace(&f.delivery.run, &second.run),
                )?;
            }
        }
    }
    let mut history: Value = serde_json::from_slice(&fs::read(second_dir.join("timeline.json"))?)?;
    history["deliveries"]
        .as_array_mut()
        .unwrap()
        .insert(0, serde_json::to_value(&f.delivery)?);
    save_json(&second_dir, "timeline.json", &history)?;
    let mut library = Library::open(&f.home)?;
    library.change(&f.home, |l| {
        l.project_mut()?
            .selected_mut()?
            .deliveries
            .push(second.clone());
        Ok(())
    })?;
    let mut receipts = Vec::new();
    for (delivery, target) in [(&f.delivery, "target"), (&second, "main")] {
        let p = parse(
            command(
                &f,
                &[
                    "merge-preview",
                    "--delivery",
                    &delivery.run,
                    "--target",
                    target,
                ],
            )
            .output()?,
        )?;
        let r = parse(
            command(
                &f,
                &[
                    "merge",
                    "--delivery",
                    &delivery.run,
                    "--target",
                    target,
                    "--diff-sha256",
                    p["diff_sha256"].as_str().unwrap(),
                ],
            )
            .output()?,
        )?;
        assert_eq!(r["delivery_run"], delivery.run);
        assert_eq!(r["target_branch"], target);
        assert_eq!(r["merged_sha"], f.candidate);
        receipts.push(r);
    }
    let status = parse(command(&f, &["status"]).output()?)?;
    for (index, delivery) in [&f.delivery, &second].into_iter().enumerate() {
        let saved: Value =
            serde_json::from_slice(&fs::read(delivery.directory(&f.home).join("merge.json"))?)?;
        assert_eq!(saved, receipts[index]);
        assert_eq!(status["deliveries"][index]["merge"], receipts[index]);
        delivery.snapshot(&f.home)?;
    }
    assert_eq!(commit(&f.repo, "target")?, f.candidate);
    assert_eq!(commit(&f.repo, "main")?, f.candidate);
    Ok(())
}
