//! アプリ状態の単一保持点 (`Entity<AppStore>`)。
//!
//! web 版 mcpr-ui の `App` コンポーネントが持つフック群に対応する。状態遷移は
//! UI 非依存の [`mcpr_app`] の純 `reduce` をそのまま呼び、`cx.notify()` で
//! 購読ビュー ([`crate::workspace`] 等) を再描画させる。View は状態を分散して
//! 持たず、この Store を `cx.observe` して追随する。
//!
//! ファイル読み込み・書き出しの async は Entity である Store 自身が持ち、
//! `cx.spawn` (foreground) と `cx.background_spawn` (別スレッド) を組み合わせる。

use std::{path::PathBuf, sync::Arc};

use futures::StreamExt;
use gpui::{AppContext as _, Context, PathPromptOptions};
use mcpr_app::{
    ExportFormat, ExportPhase, ExportPlan, ExportProgress, FilesAction, FilesState, Loaded,
    SelectionAction, SelectionState, as_merge_inputs, build_export_plan, export_filename,
    export_merged_blocking, new_replay_uuid, parse_replay,
};

use crate::settings::{Settings, Theme};

pub struct AppStore {
    pub files: FilesState,
    pub selection: SelectionState,
    /// 詳細表示するファイル (None = 先頭の Loaded を自動選択)。
    pub selected_file_id: Option<u64>,
    pub export_format: ExportFormat,
    /// 書き出し進行状態 (None = 書き出し中でない)。
    pub export_phase: Option<ExportPhase>,
    pub export_error: Option<String>,
    /// 配色テーマ (設定ファイルへ永続化)。
    pub theme: Theme,
    /// Entry::id の発番カウンタ。
    next_id: u64,
}

impl AppStore {
    pub fn new(settings: &Settings) -> Self {
        Self {
            files: FilesState::default(),
            selection: SelectionState::default(),
            selected_file_id: None,
            export_format: ExportFormat::default(),
            export_phase: None,
            export_error: None,
            theme: settings.theme,
            next_id: 0,
        }
    }

    /// 安定キー (Entry::id / 選択キー) を発番する。
    pub fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// ファイルリストの状態遷移を適用して再描画を促す。
    pub fn dispatch_files(&mut self, action: FilesAction, cx: &mut Context<Self>) {
        self.files = self.files.reduce(action);
        cx.notify();
    }

    /// パケット採否の状態遷移を適用して再描画を促す。
    pub fn dispatch_selection(&mut self, action: SelectionAction, cx: &mut Context<Self>) {
        self.selection = self.selection.reduce(action);
        cx.notify();
    }

    /// 詳細表示するファイルを切り替える。
    pub fn select_file(&mut self, id: u64, cx: &mut Context<Self>) {
        self.selected_file_id = Some(id);
        cx.notify();
    }

    /// ファイル (と対応する選択) を削除する。表示中なら選択も解除。
    pub fn remove_file(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.selected_file_id == Some(id) {
            self.selected_file_id = None;
        }
        self.files = self.files.reduce(FilesAction::Remove { id });
        self.selection = self
            .selection
            .reduce(SelectionAction::Remove { file_id: id });
        cx.notify();
    }

    /// テーマを反転して永続化する (Theme::change によるグローバル反映は
    /// 呼び出し側 View が行う。ここでは状態と設定ファイルのみ)。
    pub fn toggle_theme(&mut self, cx: &mut Context<Self>) {
        self.theme = self.theme.toggled();
        Settings { theme: self.theme }.save();
        cx.notify();
    }

    /// 書き出し可能か (全ファイル Loaded かつ 1 件以上、かつ書き出し中でない)。
    pub fn can_export(&self) -> bool {
        self.export_phase.is_none() && self.files.all_loaded()
    }

    /// 出力フォーマットを切り替える。
    pub fn set_export_format(&mut self, format: ExportFormat, cx: &mut Context<Self>) {
        self.export_format = format;
        cx.notify();
    }

    /// 詳細表示する Loaded ファイル (選択中、なければ先頭の Loaded)。
    /// 返り値は (id, データ, 速度入力原文)。解決規則は mcpr-app 側で web 版と共有。
    pub fn active_loaded(&self) -> Option<(u64, Arc<Loaded>, String)> {
        self.files
            .active_loaded(self.selected_file_id)
            .map(|(id, loaded, speed)| (id, loaded.clone(), speed.to_string()))
    }

    /// 表示中ファイルの id だけを解決する (データを複製しない軽量版)。
    pub fn active_file_id(&self) -> Option<u64> {
        self.files
            .active_loaded(self.selected_file_id)
            .map(|(id, ..)| id)
    }

    /// interval エントリを末尾へ追加する (ms)。
    pub fn add_interval(&mut self, ms: u64, cx: &mut Context<Self>) {
        let id = self.next_id();
        self.dispatch_files(
            FilesAction::AddInterval {
                id,
                input: ms.to_string(),
            },
            cx,
        );
    }

    /// 対象ファイルの再生速度倍率を差し替える (入力原文)。
    pub fn set_speed(&mut self, file_id: u64, input: String, cx: &mut Context<Self>) {
        self.dispatch_files(FilesAction::SetSpeed { id: file_id, input }, cx);
    }

    /// エントリを DnD で並べ替える (dragged を target の位置へ)。
    pub fn reorder(&mut self, dragged_id: u64, target_id: u64, cx: &mut Context<Self>) {
        self.dispatch_files(
            FilesAction::Reorder {
                dragged_id,
                target_id,
            },
            cx,
        );
    }

    /// 指定した複数の元 index をまとめて採否する (フィルタ中の一括 On/Off)。
    pub fn set_many_packets(
        &mut self,
        file_id: u64,
        indices: std::collections::HashSet<usize>,
        on: bool,
        cx: &mut Context<Self>,
    ) {
        self.dispatch_selection(
            SelectionAction::SetMany {
                file_id,
                indices: Arc::new(indices),
                on,
            },
            cx,
        );
    }

    /// ファイル選択ダイアログを開き、選ばれたファイルを非同期に読み込む。
    /// web 版の gloo-file 読み込み + `FilesAction::Add/Finish` と同型。
    pub fn open_files(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: None,
        });
        cx.spawn(async move |this, cx| {
            let paths = match rx.await {
                Ok(Ok(Some(paths))) => paths,
                _ => return,
            };
            for path in paths {
                this.update(cx, |store, cx| store.load_path(path, cx)).ok();
            }
        })
        .detach();
    }

    /// OS からドロップされたパス列を読み込む (ドロップゾーン相当)。
    pub fn load_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        for path in paths {
            self.load_path(path, cx);
        }
    }

    /// 1 ファイルを読み込む: `Add` を dispatch し、背景スレッドで
    /// `read + parse_replay` して `Finish` を dispatch する。
    fn load_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let filename = path.file_name().map_or_else(
            || path.to_string_lossy().into_owned(),
            |s| s.to_string_lossy().into_owned(),
        );
        let id = self.next_id();
        self.dispatch_files(
            FilesAction::Add {
                id,
                filename: filename.clone(),
            },
            cx,
        );
        cx.spawn(async move |this, cx| {
            // 重い read + parse は別スレッドで (数百万イベントの zip 再パース)。
            let result = cx
                .background_spawn(async move {
                    let bytes = std::fs::read(&path).map_err(|e| format!("read error: {e}"))?;
                    parse_replay(filename, &bytes)
                        .map(|loaded| (Arc::new(loaded), Arc::new(bytes)))
                        .map_err(|e| format!("parse error: {e}"))
                })
                .await;
            this.update(cx, |store, cx| {
                store.dispatch_files(FilesAction::Finish { id, result }, cx);
            })
            .ok();
        })
        .detach();
    }

    /// 連結・書き出しを行う。保存先を先に尋ね、背景スレッドで
    /// `export_merged_blocking` を回し、進捗を channel 経由で受けて表示を更新する。
    pub fn export(&mut self, cx: &mut Context<Self>) {
        if !self.can_export() {
            return;
        }
        // ファイルリストと採否から連結入力を組む (web 版と共有する mcpr-app の純関数)。
        let ExportPlan {
            items: owned,
            total_events: total,
            first_filename,
            file_count,
        } = match build_export_plan(&self.files, &self.selection) {
            Ok(Some(plan)) => plan,
            Ok(None) => return,
            Err(msg) => {
                self.export_error = Some(msg);
                cx.notify();
                return;
            }
        };
        let format = self.export_format;
        let suggested = export_filename(&first_filename, file_count > 1, format);
        let dir = dirs::download_dir().unwrap_or_else(|| PathBuf::from("."));
        let rx = cx.prompt_for_new_path(&dir, Some(&suggested));

        self.export_phase = Some(ExportPhase::Preparing);
        self.export_error = None;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let path = match rx.await {
                Ok(Ok(Some(path))) => path,
                // キャンセル/失敗 → 進捗表示を畳む。
                _ => {
                    this.update(cx, |store, cx| {
                        store.export_phase = None;
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            // 進捗は channel 経由で foreground へ届ける。
            let (tx, mut prog_rx) = futures::channel::mpsc::unbounded::<ExportProgress>();
            let bg = cx.background_spawn(async move {
                let inputs = as_merge_inputs(&owned);
                export_merged_blocking(&inputs, format, new_replay_uuid(), |p| {
                    let _ = tx.unbounded_send(p);
                })
            });
            // tx が drop される (= bg 完了) と None が来てループが終わる。
            while let Some(progress) = prog_rx.next().await {
                this.update(cx, |store, cx| {
                    store.export_phase = Some(ExportPhase::from_progress(progress, total));
                    cx.notify();
                })
                .ok();
            }
            let result = bg.await;
            this.update(cx, |store, cx| {
                match result {
                    Ok(bytes) => {
                        if let Err(e) = std::fs::write(&path, bytes) {
                            store.export_error = Some(format!("write error: {e}"));
                        }
                    }
                    Err(e) => store.export_error = Some(format!("export error: {e}")),
                }
                store.export_phase = None;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}
