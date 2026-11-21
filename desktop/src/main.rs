use anyhow::Result;
use clap::Parser;
use gpui_kit::{component::input, *};
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "Inspect AEP run evidence and edit shared agent memory")]
struct Args {
    /// Git working tree to inspect
    #[arg(long)]
    repo: Option<PathBuf>,
}

actions!(aep_studio, [Quit]);
fn main() -> Result<()> {
    let repo = Args::parse().repo;
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            aep_studio::init(cx);
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
            aep_studio::open_project_window(repo, opts, cx).expect("无法打开 AEP Studio 窗口");
            cx.activate(true);
        });
    Ok(())
}
