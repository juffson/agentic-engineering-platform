mod view;

use aep::{
    studio::{Artifact, Snapshot, preview},
    workspace::Workspace,
};
use anyhow::Result;
use clap::Parser;
use gpui_kit::{
    component::{
        Theme, ThemeMode,
        input::{self, InputEvent, InputState},
    },
    *,
};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "Inspect AEP run evidence and edit shared agent memory")]
struct Args {
    /// Git working tree to inspect
    #[arg(long)]
    repo: Option<PathBuf>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Runs,
    Memory,
}
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Filter {
    #[default]
    All,
    Pending,
    Attention,
    Current,
    History,
}

fn string<'a>(value: &'a Value, field: &str) -> &'a str {
    value[field].as_str().unwrap_or("")
}

fn pending(status: &str) -> bool {
    status == "awaiting_human_review"
}
fn attention(status: &str) -> bool {
    matches!(status, "failed" | "needs_reconciliation" | "unstarted")
}

struct Studio {
    editor: Option<view::editor::Editor>,
    workspace: Option<Workspace>,
    snapshot: Snapshot,
    page: Page,
    filter: Filter,
    run: Option<String>,
    note: Option<i64>,
    search: Entity<InputState>,
    _subscription: Subscription,
    artifact: Option<(String, Artifact, String)>,
    loading: bool,
    generation: u64,
    repo_error: Option<String>,
}

impl Studio {
    fn new(workspace: Option<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("搜索 ID、任务或内容"));
        let sub = cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        let mut studio = Self {
            editor: None,
            workspace,
            snapshot: Snapshot::default(),
            page: Page::Runs,
            filter: Filter::All,
            run: None,
            note: None,
            search,
            _subscription: sub,
            artifact: None,
            loading: false,
            generation: 0,
            repo_error: None,
        };
        studio.reload(cx);
        studio
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        let Some(ws) = self.workspace.clone() else {
            return;
        };
        self.loading = true;
        self.generation += 1;
        let generation = self.generation;
        let task = cx
            .background_executor()
            .spawn(async move { Snapshot::load(&ws) });
        cx.spawn(async move |this, cx| {
            let snapshot = task.await;
            let _ = this.update(cx, |this, cx| {
                if generation != this.generation {
                    return;
                }
                this.snapshot = snapshot;
                this.loading = false;
                this.artifact = None;
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn choose_repo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dialog = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("打开 Git 仓库".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let selection: Result<_> = async {
                dialog
                    .await??
                    .and_then(|v| v.into_iter().next())
                    .map(|p| Workspace::open(&p))
                    .transpose()
            }
            .await;
            let _ = this.update_in(cx, |this, window, cx| match selection {
                Ok(None) => (),
                Ok(Some(ws)) => {
                    this.workspace = Some(ws);
                    this.snapshot = Snapshot::default();
                    this.run = None;
                    this.note = None;
                    this.repo_error = None;
                    this.loading = false;
                    this.navigate(Page::Runs, window, cx);
                    this.reload(cx);
                }
                Err(error) => {
                    this.repo_error = Some(format!("打开仓库失败：{error:#}"));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn navigate(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        self.page = page;
        self.filter = Filter::All;
        self.artifact = None;
        self.search
            .update(cx, |input, cx| input.set_value("", window, cx));
        cx.notify();
    }

    fn show_artifact(&mut self, id: &str, kind: Artifact, cx: &mut Context<Self>) {
        let Some(ws) = &self.workspace else { return };
        let text = match preview(&ws.data, id, kind) {
            Ok(Some(p)) if p.truncated => format!("仅显示末尾 48 KiB\n\n{}", p.text),
            Ok(Some(p)) if p.text.trim().is_empty() => "文件为空".to_owned(),
            Ok(Some(p)) => p.text,
            Ok(None) => "此次运行尚未生成这份文件。".to_owned(),
            Err(e) => format!("读取失败：{e:#}"),
        };
        self.artifact = Some((id.to_owned(), kind, text));
        cx.notify();
    }
}

actions!(aep_studio, [Quit]);
fn main() -> Result<()> {
    let ws = match Args::parse().repo {
        Some(dir) => Some(Workspace::open(&dir)?),
        None => Workspace::open(std::path::Path::new(".")).ok(),
    };
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            Theme::change(ThemeMode::Dark, None, cx);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
            cx.set_menus([
                Menu::new("AEP Studio").items([MenuItem::action("退出 AEP Studio", Quit)]),
                Menu::new("编辑").items([
                    MenuItem::os_action("剪切", input::Cut, OsAction::Cut),
                    MenuItem::os_action("复制", input::Copy, OsAction::Copy),
                    MenuItem::os_action("粘贴", input::Paste, OsAction::Paste),
                    MenuItem::os_action("全选", input::SelectAll, OsAction::SelectAll),
                ]),
            ]);
            let opts = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1120.), px(730.)), cx)),
                window_min_size: Some(size(px(840.), px(530.))),
                ..Default::default()
            };
            gpui_kit::open_window(opts, cx, move |window, cx| {
                window.set_window_title("AEP Studio");
                cx.new(|cx| Studio::new(ws, window, cx))
            })
            .expect("无法打开 AEP Studio 窗口");
            cx.activate(true);
        });
    Ok(())
}
