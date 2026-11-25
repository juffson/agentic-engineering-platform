use super::*;
use sha2::{Digest, Sha256};
use std::{
    io::{Seek, Write},
    path::Component,
};
const MAX_RESULT: u64 = 4 * 1024 * 1024;
const MAX_FILES: u64 = 64 * 1024 * 1024;

fn result(file: &Path) -> Result<String> {
    let source: Box<dyn Read> = if file == Path::new("-") {
        Box::new(io::stdin().lock())
    } else {
        Box::new(fs::File::open(file)?)
    };
    let mut s = String::new();
    source.take(MAX_RESULT + 1).read_to_string(&mut s)?;
    ensure!(
        !s.trim().is_empty() && s.len() as u64 <= MAX_RESULT,
        "结果为空或超过 4 MiB"
    );
    Ok(s)
}
fn artifacts(workspace: &Path, dest: &Path, files: &[PathBuf]) -> Result<Vec<Value>> {
    ensure!(files.len() <= 32, "每次最多 32 个证据文件");
    let mut left = MAX_FILES;
    let mut manifest = vec![];
    for path in files {
        ensure!(
            !path.as_os_str().is_empty()
                && path.components().all(|p| matches!(p, Component::Normal(_))),
            "证据需使用工作区内的相对路径"
        );
        let real = workspace.join(path).canonicalize()?;
        ensure!(
            real.starts_with(workspace) && real.is_file(),
            "证据文件位于工作区之外或不是普通文件"
        );
        let mut source = fs::File::open(real)?;
        let target = dest.join(path);
        fs::create_dir_all(target.parent().unwrap())?;
        let mut out = fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(target)?;
        let size = io::copy(&mut (&mut source).take(left + 1), &mut out)?;
        ensure!(size <= left, "证据总量超过 64 MiB");
        left -= size;
        out.rewind()?;
        let mut hash = Sha256::new();
        io::copy(&mut out, &mut hash)?;
        manifest.push(json!({"path":path,"bytes":size,"sha256":format!("{:x}",hash.finalize())}));
    }
    Ok(manifest)
}
pub(super) fn finish(
    dir: &Path,
    session: &str,
    file: &Path,
    summary: &str,
    files: &[PathBuf],
    blocked: bool,
) -> Result<Value> {
    ensure!(
        !summary.trim().is_empty() && summary.chars().count() <= aep::memory::TEXT_LIMIT,
        "请提供 1 至 1000 字的交接摘要"
    );
    let req = request(dir)?;
    let binding = req.external.as_ref().context("此执行由 Studio 托管")?;
    ensure!(session == binding.session, "会话与领取回执不符");
    let body = result(file)?;
    let gate = matches!(
        req.flow().stages.get(req.stage).context("无效阶段")?.role,
        workflow::Role::Test | workflow::Role::Accept
    );
    ensure!(
        blocked
            || !gate
            || body
                .trim()
                .lines()
                .last()
                .is_some_and(|s| s.trim() == "AEP: PASS"),
        "测试与独立验收要求最后一行 AEP: PASS，未通过时请使用 --blocked"
    );
    let work = dir
        .parent()
        .and_then(Path::parent)
        .context("missing work directory")?;
    let id = dir.file_name().unwrap().to_str().unwrap();
    let stop = Cancellation::new(work, id)?;
    let _guard = stop.lock()?;
    let mut ev = workflow::evidence(dir)?;
    ensure!(ev["status"] != "cancelled", "此领取已取消");
    ensure!(
        ev["read_memory"].is_array() && dir.join("prompt.txt").is_file(),
        "请先读取当前领取的 context"
    );
    let identity = json!({"session":session,"sha256":format!("{:x}",Sha256::digest(body.as_bytes())),"summary":summary,"files":files,"blocked":blocked});
    let dest = dir.join("submission");
    if dest.exists() {
        let prior = aep::workspace::read(&dest.join("manifest.json"))?;
        ensure!(
            prior["identity"] == identity,
            "此次执行已有不同的提交，请新建重试"
        );
        if ev["status"] == "succeeded" || ev["status"] == "blocked" {
            return Ok(ev);
        }
    } else {
        ensure!(ev["status"] == "claimed", "此执行不可提交");
        let tmp = tempfile::tempdir_in(dir)?;
        let archive = artifacts(&binding.workspace, &tmp.path().join("files"), files)?;
        fs::write(tmp.path().join("result.txt"), &body)?;
        save_json(
            tmp.path(),
            "manifest.json",
            &json!({"identity":identity,"artifacts":archive}),
        )?;
        fs::rename(tmp.path(), &dest)?;
    }
    let manifest = aep::workspace::read(&dest.join("manifest.json"))?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(body.as_bytes())?;
    tmp.persist(dir.join("result.txt"))?;
    if !blocked {
        let mut db = Memory::open(work.parent().context("missing project")?)?;
        ev["written_memory"] = json!(db.project_handoff(
            id,
            &format!("{} / {}", req.profile.client.label(), session),
            summary.trim()
        )?);
    } else {
        ev["error"] = json!(summary.trim());
    }
    if let Ok(head) = aep::workspace::commit(&binding.workspace, "HEAD") {
        ev["workspace_head_at_submit"] = json!(head);
    }
    ev["artifacts"] = manifest["artifacts"].clone();
    ev["status"] = json!(if blocked { "blocked" } else { "succeeded" });
    ev["finished_at"] = json!(chrono::Utc::now().to_rfc3339());
    save_json(dir, "evidence.json", &ev)?;
    Ok(ev)
}
