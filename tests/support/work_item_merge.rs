use aep::{
    project::{
        Kind, Library,
        archive::{Delivery, run_directory},
        save_json,
    },
    workflow::Template,
    workspace::{Workspace, commit, git},
};
use anyhow::Result;
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
};

pub struct Fixture {
    pub repo: PathBuf,
    pub home: PathBuf,
    pub delivery: Delivery,
    pub base: String,
    pub candidate: String,
}
/// A sealed four-step chain with a real retained candidate and clean downstream checkout.
pub fn create(root: &Path) -> Result<Fixture> {
    let repo = root.join("repo");
    fs::create_dir_all(&repo)?;
    git(&repo, &["init", "-b", "main"])?;
    git(&repo, &["config", "user.name", "Merge Test"])?;
    git(&repo, &["config", "user.email", "merge@example.test"])?;
    fs::write(repo.join("file.txt"), "base\n")?;
    git(&repo, &["add", "."])?;
    git(&repo, &["commit", "-m", "base"])?;
    let base = commit(&repo, "HEAD")?;
    git(&repo, &["checkout", "-b", "build"])?;
    fs::write(repo.join("file.txt"), "base\naccepted change\n")?;
    git(&repo, &["add", "."])?;
    git(&repo, &["commit", "-m", "candidate"])?;
    let candidate = commit(&repo, "HEAD")?;
    git(&repo, &["branch", "target", &base])?;
    git(
        &repo,
        &[
            "update-ref",
            "refs/aep/runs/relay-fixture-r2-a1",
            &candidate,
        ],
    )?;
    git(&repo, &["checkout", "main"])?;
    git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "https://unreachable.invalid/repo.git",
        ],
    )?;
    let stage = root.join("stage");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--detach",
            stage.to_str().unwrap(),
            &candidate,
        ],
    )?;
    let home = Workspace::open(&repo)?.data.join("studio-projects");
    let mut lib = Library::default();
    lib.change(&home, |l| {
        l.create("Merge fixture", vec![repo.clone()])?;
        l.project_mut()?
            .add_work("归档合并自测", Kind::Feature, Template::Software)
    })?;
    let p = lib.project().unwrap();
    let w = p.selected().unwrap();
    let delivery = Delivery {
        project: p.id.clone(),
        work: w.id.clone(),
        run: "delivery-fixture".into(),
        accepted_run: "accept-fixture".into(),
        archived_at: "2026-11-23T00:00:00Z".into(),
        attempts: 4,
        merge: None,
        merge_history: vec![],
    };
    let d = &delivery;
    let transfers:Vec<_>=[(3,"relay-fixture-r2-a1",Some(candidate.clone())),(4,"test-fixture",None),(5,"accept-fixture",None)]
        .into_iter().map(|(from,run,c)|json!({"project":d.project,"work":d.work,"from":from,"to":from+1,"run":run,"repository":repo,"candidate":c,"decision_id":null,"decision":"approved","acceptance":"PASS","confirmed_at":"now"})).collect();
    let runs = [
        "relay-fixture-r2-a1",
        "test-fixture",
        "accept-fixture",
        "delivery-fixture",
    ];
    let rounds:Vec<_>=runs.iter().enumerate().map(|(i,r)|json!({"record":"workflow","run":r,"repository":repo,"stage_index":i+3,"round":if i==0{Some(2)}else{None},"attempt":1,"status":if i==0{"awaiting_human_review"}else{"succeeded"},"decision":null,"read_stage_transfer":if i==0{serde_json::Value::Null}else{transfers[i-1].clone()},"agents":[]})).collect();
    let directory = d.directory(&home);
    fs::create_dir_all(&directory)?;
    save_json(&directory, "manifest.json", &json!({"delivery":d}))?;
    save_json(
        &directory,
        "timeline.json",
        &json!({"deliveries":[d],"work_id":d.work,"work_name":"归档合并自测","stages":[],"rounds":rounds,"stage_transfers":transfers,"errors":[]}),
    )?;
    for (i, r) in runs.iter().enumerate() {
        let folder = directory.join(run_directory(i, r));
        fs::create_dir_all(&folder)?;
        let ev = if i == 0 {
            json!({"run_id":r,"candidate_sha":candidate,"spec_sha":base,"check_exit_code":0,"target":"target"})
        } else {
            json!({"run_id":r,"status":"succeeded","workspace":stage,"read_stage_transfer":transfers[i-1]})
        };
        save_json(&folder, "evidence.json", &ev)?;
    }
    lib.change(&home, |l| {
        let w = l.project_mut()?.selected_mut()?;
        w.stage = w.template.stages().len();
        w.deliveries.push(delivery.clone());
        Ok(())
    })?;
    Ok(Fixture {
        repo,
        home,
        delivery,
        base,
        candidate,
    })
}
