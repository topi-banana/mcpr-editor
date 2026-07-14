// Windows のリリースビルドでは GUI とは別にコンソール窓が開くのを抑止する
// (既定の console サブシステムを windows サブシステムへ切り替える)。
// debug ビルドでは残して起動ログ (WSL/X11 まわり等) を見えるようにする。
// この属性は Windows 以外では無視されるため cfg でのプラットフォーム分岐は不要。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! mcpr-gpui: gpui + gpui-component による native UI。
//!
//! アプリロジック(連結・書き出し・フィルタ・状態遷移)は `mcpr-app` を通じて
//! web 版 `mcpr-ui` と共有し、この crate は gpui の View 層だけを持つ。
//!
//! WSL2/WSLg で起動する場合は Wayland の UnsupportedVersion を避けるため
//! X11 を強制する:
//!   WAYLAND_DISPLAY= ZED_ALLOW_EMULATED_GPU=1 cargo run -p mcpr-gpui

mod events_table;
mod settings;
mod store;
mod workspace;

use std::path::PathBuf;

use gpui::*;
use gpui_component::Root;

use crate::{settings::Settings, store::AppStore, workspace::Workspace};

fn main() {
    // コマンドライン引数のファイルは起動時に読み込む
    // (`cargo run -p mcpr-gpui -- a.mcpr b.zip`)。
    let cli_files: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();

    gpui_platform::application().run(move |cx| {
        // GPUI Component の機能を使う前に必ず呼ぶ (テーマ等のグローバル初期化)。
        gpui_component::init(cx);

        let settings = Settings::load();
        let cli_files = cli_files.clone();

        cx.spawn(async move |cx| {
            cx.open_window(WindowOptions::default(), |window, cx| {
                let store = cx.new(|_| AppStore::new(&settings));
                if !cli_files.is_empty() {
                    store.update(cx, |store, cx| store.load_paths(cli_files, cx));
                }
                let workspace = cx.new(|cx| Workspace::new(store, window, cx));
                // ウィンドウ直下の最上位は Root にする (gpui-component の要件)。
                cx.new(|cx| Root::new(workspace, window, cx))
            })
            .expect("failed to open window");
        })
        .detach();
    });
}
