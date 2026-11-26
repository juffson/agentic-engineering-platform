//! CLI 集成：从多仓库文件夹创建项目 / 添加仓库（specs/multi-repo-folder-project §10）。
use anyhow::{Result, ensure};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn invoke(home: &Path, cwd: &Path, args: &[&str]) -> Result<Output> {
    Ok(Command::new(env!("CARGO_BIN_EXE_aep"))
        .current_dir(cwd)
        .env_remove("AEP_PROJECT")
        .env_remove("AEP_WORK")
        .env_remove("AEP_PROJECT_HOME")
        .args(["project", "--home"])
        .arg(home)
        .args(args)
        .output()?)
}
fn call(home: &Path, cwd: &Path, args: &[&str]) -> Result<Value> {
    let o = invoke(home, cwd, args)?;
    ensure!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    Ok(serde_json::from_slice(&o.stdout)?)
}
fn repo(path: &Path) -> Result<PathBuf> {
    fs::create_dir_all(path)?;
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "a@example.com"],
        vec!["config", "user.name", "a"],
        vec!["commit", "--allow-empty", "-m", "init"],
    ] {
        assert!(
            Command::new("git").arg("-C").arg(path).args(&args).status()?.success(),
            "git {args:?} in {}",
            path.display()
        );
    }
    Ok(path.canonicalize()?)
}
/// 工作区 + 2 个第 1 层仓库 + 1 个第 3 层仓库。
struct Workspace {
    _t: tempfile::TempDir,
    root: PathBuf,
    a: PathBuf,
    b: PathBuf,
    deep: PathBuf,
}
fn workspace() -> Result<Workspace> {
    let t = tempfile::tempdir()?;
    let root = t.path().join("market");
    let a = repo(&root.join("genki"))?;
    let b = repo(&root.join("genki-api"))?;
    let deep = repo(&root.join("services/quote/quote-data-service"))?;
    fs::write(root.join("AGENTS.md"), "团队约定")?;
    Ok(Workspace { _t: t, root: root.canonicalize()?, a, b, deep })
}

#[test]
fn create_without_all_lists_the_repositories_and_creates_nothing() -> Result<()> {
    let w = workspace()?;
    let home = w._t.path().join("home");
    let o = invoke(&home, w.a.as_path(), &["create", "行情", "--repository", w.root.to_str().unwrap()])?;
    assert_eq!(o.status.code(), Some(1), "AC-9: exit code 1");
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(stderr.contains(&format!("{} 不是 git 仓库，在它下面找到 2 个仓库（查找 2 层）", w.root.display())), "{stderr}");
    assert!(stderr.contains("genki "), "{stderr}");
    assert!(stderr.contains("genki-api "), "{stderr}");
    assert!(stderr.contains("加 --all 全部添加，或用 --repository 逐个指定"), "{stderr}");
    assert!(!stderr.contains("fatal"), "AC-3: no git output");
    // 项目没有创建。
    assert_eq!(call(&home, w.a.as_path(), &["list"])?, serde_json::json!([]));
    Ok(())
}

#[test]
fn create_with_all_records_the_workspace_root() -> Result<()> {
    let w = workspace()?;
    let home = w._t.path().join("home");
    let created = call(&home, w.a.as_path(), &[
        "create", "行情", "--repository", w.root.to_str().unwrap(), "--all",
    ])?;
    assert_eq!(created["workspace_root"], serde_json::json!(w.root));
    assert_eq!(
        created["repositories"],
        serde_json::json!([w.a, w.b]),
        "深度 2 不含第 3 层"
    );
    assert!(
        !created["repositories"].as_array().unwrap().contains(&serde_json::json!(w.deep)),
        "第 3 层的仓库不在默认深度内"
    );
    let list = call(&home, w.a.as_path(), &["list"])?;
    assert_eq!(list[0]["repositories"].as_array().unwrap().len(), 2);
    Ok(())
}

#[test]
fn add_repo_expands_the_folder_and_keeps_existing_unique() -> Result<()> {
    let w = workspace()?;
    let home = w._t.path().join("home");
    let a = w.a.as_path();
    call(&home, a, &["create", "行情", "--repository", w.a.to_str().unwrap()])?;
    call(&home, a, &["add-work", "迁移"])?;
    call(&home, a, &["bind"])?;
    // 不带 --all：列表 + 「逐个 add-repo」提示。
    let o = invoke(&home, a, &["add-repo", w.root.to_str().unwrap()])?;
    assert_eq!(o.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(stderr.contains("加 --all 全部添加，或逐个 add-repo"), "{stderr}");
    // 带 --all：a 已在项目里，b 是新增；选中事项时两个都进入事项。
    let out = call(&home, a, &["add-repo", w.root.to_str().unwrap(), "--all"])?;
    assert_eq!(out["added"], serde_json::json!([w.b]));
    assert_eq!(out["existing"], serde_json::json!([w.a]));
    assert_eq!(out["workspace_root"], serde_json::json!(w.root));
    // 再来一次：existing 里两个都在，没有重复添加。
    let again = call(&home, a, &["add-repo", w.root.to_str().unwrap(), "--all"])?;
    assert_eq!(again["added"], serde_json::json!([]));
    assert_eq!(again["existing"], serde_json::json!([w.a, w.b]));
    let list = call(&home, a, &["list"])?[0]["repositories"].clone();
    assert_eq!(list, serde_json::json!([w.a, w.b]));
    // 选中事项时，添加的仓库同时进入事项（R16）。
    let saved: Value =
        serde_json::from_str(&fs::read_to_string(home.join("projects.json"))?)?;
    let item = &saved["projects"][0]["work"][0];
    assert_eq!(item["repositories"], serde_json::json!([w.a, w.b]));
    Ok(())
}

#[test]
fn empty_folder_and_depth_range_are_errors() -> Result<()> {
    let w = workspace()?;
    let home = w._t.path().join("home");
    let empty = w._t.path().join("empty");
    fs::create_dir(&empty)?;
    let o = invoke(&home, w.a.as_path(), &["create", "空", "--repository", empty.to_str().unwrap()])?;
    assert_eq!(o.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(stderr.contains("这个文件夹和它的子目录里没有 git 仓库（已查找 2 层）"), "{stderr}");
    let o = invoke(&home, w.a.as_path(), &[
        "create", "深", "--repository", w.root.to_str().unwrap(), "--depth", "5",
    ])?;
    assert!(!o.status.success());
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("--depth 只能是 1 到 4"),
        "depth range message"
    );
    Ok(())
}

#[test]
fn plain_repository_with_all_keeps_today_s_output() -> Result<()> {
    let w = workspace()?;
    let home = w._t.path().join("home");
    let a = w.a.as_path();
    call(&home, a, &["create", "行情", "--repository", w.a.to_str().unwrap(), "--all"])?;
    // --all 对普通仓库不起作用：输出仍是仓库路径。
    let out = call(&home, a, &["add-repo", w.a.to_str().unwrap(), "--all"])?;
    assert_eq!(out, serde_json::json!(w.a));
    Ok(())
}

#[test]
fn a_second_workspace_keeps_the_first_root() -> Result<()> {
    let w = workspace()?;
    let other = tempfile::tempdir()?;
    let x = repo(&other.path().join("x"))?;
    let home = w._t.path().join("home");
    let a = w.a.as_path();
    call(&home, a, &["create", "行情", "--repository", w.root.to_str().unwrap(), "--all"])?;
    let out = call(&home, a, &["add-repo", other.path().to_str().unwrap(), "--all"])?;
    assert_eq!(out["workspace_root"], serde_json::json!(w.root), "Q3: 原值保留");
    assert_eq!(out["note"], serde_json::json!(format!("项目工作区仍是 {}", w.root.display())));
    assert_eq!(out["added"], serde_json::json!([x]));
    Ok(())
}
