use aep::workspace::{commit, git, validate_task_target};
use std::{fs, path::Path};
fn fixture(repo: &Path) -> String {
    git(repo, &["init", "-qb", "main"]).unwrap();
    git(repo, &["config", "user.name", "AEP Test"]).unwrap();
    git(repo, &["config", "user.email", "aep@example.invalid"]).unwrap();
    git(repo, &["config", "commit.gpgsign", "false"]).unwrap();
    save(repo, "source.txt", "initial")
}
fn save(repo: &Path, file: &str, body: &str) -> String {
    fs::create_dir_all(repo.join(file).parent().unwrap()).unwrap();
    fs::write(repo.join(file), body).unwrap();
    git(repo, &["add", "--", file]).unwrap();
    git(repo, &["commit", "-qm", file]).unwrap();
    commit(repo, "HEAD").unwrap()
}
#[test]
fn accepts_the_pinned_task_files_including_nested_fixtures() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    let base = fixture(repo);
    save(repo, "specs/task-a/spec.md", "task");
    let spec = save(repo, "specs/task-a/fixtures/check.txt", "fixture");
    assert!(validate_task_target(repo, &base, &spec, "task-a").is_ok());
}
#[test]
fn rejects_divergent_targets_and_unrelated_paths() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    let base = fixture(repo);
    let spec = save(repo, "specs/task-a-extra/spec.md", "another task");
    assert!(validate_task_target(repo, &base, &spec, "task-a").is_err());
    git(repo, &["switch", "-qc", "other", &base]).unwrap();
    let target = save(repo, "source.txt", "different");
    assert!(validate_task_target(repo, &target, &spec, "task-a-extra").is_err());
    assert!(validate_task_target(repo, &base, &spec, "../task").is_err());
}
#[test]
fn rejects_moving_a_product_file_under_the_task() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    let base = fixture(repo);
    fs::create_dir_all(repo.join("specs/task-a")).unwrap();
    git(repo, &["mv", "source.txt", "specs/task-a/source.txt"]).unwrap();
    git(repo, &["commit", "-qm", "move unrelated file"]).unwrap();
    assert!(validate_task_target(repo, &base, "HEAD", "task-a").is_err());
}
