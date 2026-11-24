//! AEP Studio: Tauri shell over the `aep` core. Commands keep their work off the UI thread.
#![allow(non_snake_case)]
pub mod api;
pub mod runner;

use aep::workflow::Profile;
use serde::Serialize;

fn run<T: Serialize + Send + 'static>(
    f: impl FnOnce(std::path::PathBuf) -> anyhow::Result<T> + Send + 'static,
) -> Result<T, String> {
    api::home().and_then(f).map_err(|e| format!("{e:#}"))
}

#[tauri::command(async)]
fn snapshot() -> Result<impl Serialize, String> {
    run(move |h| api::snapshot(&h))
}

#[tauri::command(async)]
fn openProject(project: String) -> Result<impl Serialize, String> {
    run(move |h| api::open_project(&h, &project))
}

#[tauri::command(async)]
fn selectWork(project: String, work: String) -> Result<impl Serialize, String> {
    run(move |h| api::select_work(&h, &project, &work))
}

#[tauri::command(async)]
fn createProject(name: String, repositories: Vec<String>) -> Result<impl Serialize, String> {
    run(move |h| api::create_project(&h, &name, &repositories))
}

#[tauri::command(async)]
fn addRepository(project: String, path: String) -> Result<impl Serialize, String> {
    run(move |h| api::add_repository(&h, &project, &path))
}

#[tauri::command(async)]
fn createWork(
    project: String,
    name: String,
    template: String,
    release: bool,
    repositories: Vec<String>,
) -> Result<impl Serialize, String> {
    run(move |h| api::create_work(&h, &project, &name, &template, release, &repositories))
}

#[tauri::command(async)]
fn workDetail(project: String, work: String) -> Result<impl Serialize, String> {
    run(move |h| api::work_detail(&h, &project, &work))
}

#[tauri::command(async)]
fn attemptView(project: String, work: String, attempt: String) -> Result<impl Serialize, String> {
    run(move |h| api::attempt_view(&h, &project, &work, &attempt))
}

#[tauri::command(async)]
fn updateStage(
    project: String,
    work: String,
    stage: usize,
    note: Option<String>,
    profile: Option<Profile>,
    sandbox: Option<String>,
) -> Result<impl Serialize, String> {
    run(move |h| api::update_stage(&h, &project, &work, stage, note, profile, sandbox))
}

#[tauri::command(async)]
fn runStage(
    project: String,
    work: String,
    stage: usize,
    message: Option<String>,
) -> Result<impl Serialize, String> {
    run(move |h| api::run_stage(&h, &project, &work, stage, message))
}

#[tauri::command(async)]
fn followUp(
    project: String,
    work: String,
    stage: usize,
    message: String,
) -> Result<impl Serialize, String> {
    run(move |h| api::follow_up(&h, &project, &work, stage, &message))
}

#[tauri::command(async)]
fn cancel(project: String, work: String, attempt: String) -> Result<impl Serialize, String> {
    run(move |h| api::cancel(&h, &project, &work, &attempt))
}

#[tauri::command(async)]
fn advance(project: String, work: String, stage: usize) -> Result<impl Serialize, String> {
    run(move |h| api::advance(&h, &project, &work, stage))
}

#[tauri::command(async)]
fn sendBack(
    project: String,
    work: String,
    from: usize,
    to: usize,
    reason: Option<String>,
) -> Result<impl Serialize, String> {
    run(move |h| api::send_back(&h, &project, &work, from, to, reason))
}

#[tauri::command(async)]
fn waive(
    project: String,
    work: String,
    stage: usize,
    reason: String,
) -> Result<impl Serialize, String> {
    run(move |h| api::waive(&h, &project, &work, stage, &reason))
}

#[tauri::command(async)]
fn skip(
    project: String,
    work: String,
    stage: usize,
    reason: String,
) -> Result<impl Serialize, String> {
    run(move |h| api::skip(&h, &project, &work, stage, &reason))
}

#[tauri::command(async)]
fn reopen(project: String, work: String, stage: usize) -> Result<impl Serialize, String> {
    run(move |h| api::reopen(&h, &project, &work, stage))
}

#[tauri::command(async)]
fn autopilotStart(project: String, work: String, until: usize) -> Result<impl Serialize, String> {
    run(move |h| api::autopilot_start(&h, &project, &work, until))
}

#[tauri::command(async)]
fn autopilotStop(project: String, work: String) -> Result<impl Serialize, String> {
    run(move |h| api::autopilot_stop(&h, &project, &work))
}

#[tauri::command(async)]
fn archive(
    project: String,
    work: String,
    memory: String,
    remove_worktree: bool,
) -> Result<impl Serialize, String> {
    run(move |h| api::archive(&h, &project, &work, &memory, remove_worktree))
}

#[tauri::command(async)]
fn zcodeOpen(project: String, work: String, attempt: String) -> Result<impl Serialize, String> {
    run(move |h| api::zcode_open(&h, &project, &work, &attempt))
}

#[tauri::command(async)]
fn deleteWork(project: String, work: String) -> Result<impl Serialize, String> {
    run(move |h| api::delete_work(&h, &project, &work))
}

#[tauri::command(async)]
fn pickFolder(prompt: String) -> Result<impl Serialize, String> {
    api::pick_folder(&prompt).map_err(|e| format!("{e:#}"))
}

#[tauri::command(async)]
fn importScan(path: String) -> Result<impl Serialize, String> {
    api::import_scan(&path).map_err(|e| format!("{e:#}"))
}

#[derive(serde::Deserialize)]
struct ImportBranch {
    branch: String,
    path: String,
}

#[tauri::command(async)]
fn importApply(
    project: String,
    path: String,
    memories: Vec<String>,
    branches: Vec<ImportBranch>,
) -> Result<impl Serialize, String> {
    let branches: Vec<(String, String)> =
        branches.into_iter().map(|b| (b.branch, b.path)).collect();
    run(move |h| api::import_apply(&h, &project, &path, &memories, &branches))
}

#[tauri::command(async)]
fn removeProject(project: String) -> Result<impl Serialize, String> {
    run(move |h| api::remove_project(&h, &project))
}

#[tauri::command(async)]
fn reviseMemory(project: String, id: i64, text: String) -> Result<impl Serialize, String> {
    run(move |h| api::revise_memory(&h, &project, id, &text))
}

#[tauri::command(async)]
fn branchGraph(project: String, path: String, limit: usize) -> Result<impl Serialize, String> {
    run(move |h| api::branch_graph(&h, &project, &path, limit))
}

#[tauri::command(async)]
fn gitTools() -> Result<impl Serialize, String> {
    run(move |h| api::git_tools(&h))
}

#[tauri::command(async)]
fn openGitTool(id: String, path: String) -> Result<impl Serialize, String> {
    run(move |h| api::open_git_tool(&h, &id, &path))
}

#[tauri::command(async)]
fn importSyncPlan(project: String, path: String) -> Result<impl Serialize, String> {
    run(move |h| api::import_sync_plan(&h, &project, &path))
}

#[tauri::command(async)]
fn importSync(
    project: String,
    path: String,
    memories: bool,
    items: bool,
) -> Result<impl Serialize, String> {
    run(move |h| api::import_sync(&h, &project, &path, memories, items))
}

#[tauri::command(async)]
fn tidyStart(
    project: String,
    client: String,
    model: String,
    instruction: String,
) -> Result<impl Serialize, String> {
    run(move |h| api::tidy_start(&h, &project, &client, &model, &instruction))
}

#[tauri::command(async)]
fn tidyStatus(project: String) -> Result<impl Serialize, String> {
    run(move |h| api::tidy_status(&h, &project))
}

#[tauri::command(async)]
fn tidyApply(project: String, job: String, ops: Vec<usize>) -> Result<impl Serialize, String> {
    run(move |h| api::tidy_apply(&h, &project, &job, &ops))
}

#[tauri::command(async)]
fn draftStart(
    project: String,
    client: String,
    model: String,
    request: String,
    repository: Option<String>,
) -> Result<impl Serialize, String> {
    run(move |h| {
        api::draft_start(
            &h,
            &project,
            &client,
            &model,
            &request,
            repository.as_deref(),
        )
    })
}

#[tauri::command(async)]
fn draftStatus(project: String) -> Result<impl Serialize, String> {
    run(move |h| api::draft_status(&h, &project))
}

#[tauri::command(async)]
fn cleanup(project: String, work: String, force: bool) -> Result<impl Serialize, String> {
    run(move |h| api::cleanup(&h, &project, &work, force))
}

#[tauri::command(async)]
fn unarchive(project: String, work: String) -> Result<impl Serialize, String> {
    run(move |h| api::unarchive(&h, &project, &work))
}

#[tauri::command(async)]
fn mergePreview(
    project: String,
    work: String,
    target: Option<String>,
) -> Result<impl Serialize, String> {
    run(move |h| api::merge_preview(&h, &project, &work, target))
}

#[tauri::command(async)]
fn mergeConfirm(
    project: String,
    work: String,
    target: String,
    digest: String,
) -> Result<impl Serialize, String> {
    run(move |h| api::merge_confirm(&h, &project, &work, &target, &digest))
}

#[tauri::command(async)]
fn memories(project: String) -> Result<impl Serialize, String> {
    run(move |h| api::memories(&h, &project))
}

#[tauri::command(async)]
fn addMemory(project: String, text: String) -> Result<impl Serialize, String> {
    run(move |h| api::add_memory(&h, &project, &text))
}

#[tauri::command(async)]
fn reveal(path: String) -> Result<impl Serialize, String> {
    run(move |_| api::reveal(&path))
}

#[tauri::command(async)]
fn history(project: String, work: String) -> Result<impl Serialize, String> {
    run(move |h| api::history(&h, &project, &work))
}

#[tauri::command(async)]
fn handoffs(project: String, work: String) -> Result<impl Serialize, String> {
    run(move |h| api::handoffs(&h, &project, &work))
}

#[tauri::command(async)]
fn sendHandoff(
    project: String,
    work: String,
    to: String,
    note: String,
) -> Result<impl Serialize, String> {
    run(move |h| api::send_handoff(&h, &project, &work, &to, &note))
}

#[tauri::command(async)]
fn zcodeModels(refresh: Option<bool>) -> Result<impl Serialize, String> {
    run(move |_| api::zcode_models(refresh.unwrap_or(false)))
}

#[tauri::command(async)]
fn repoMemories(path: String) -> Result<impl Serialize, String> {
    run(move |_| api::repo_memories(&path))
}

#[tauri::command(async)]
fn addRepoMemory(path: String, text: String) -> Result<impl Serialize, String> {
    run(move |_| api::add_repo_memory(&path, &text))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn start() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            snapshot,
            openProject,
            selectWork,
            createProject,
            addRepository,
            createWork,
            workDetail,
            attemptView,
            updateStage,
            runStage,
            followUp,
            cancel,
            advance,
            sendBack,
            waive,
            skip,
            reopen,
            autopilotStart,
            autopilotStop,
            archive,
            unarchive,
            cleanup,
            draftStart,
            draftStatus,
            tidyStart,
            tidyStatus,
            tidyApply,
            importSyncPlan,
            importSync,
            branchGraph,
            gitTools,
            openGitTool,
            reviseMemory,
            removeProject,
            importScan,
            importApply,
            pickFolder,
            deleteWork,
            zcodeOpen,
            mergePreview,
            mergeConfirm,
            memories,
            addMemory,
            reveal,
            history,
            handoffs,
            sendHandoff,
            zcodeModels,
            repoMemories,
            addRepoMemory
        ])
        .run(tauri::generate_context!())
        .expect("AEP Studio failed to start");
}
