//! ルートビュー。ヘッダ(タイトル + テーマ切替)と 2 ペイン(左サイドバー +
//! 右メイン)のレイアウトを組み、[`AppStore`] を `observe` して追随する。
//!
//! web 版 mcpr-ui の `mcpr-shell` / `mcpr-topbar` / `mcpr-workspace` に対応。
//! Phase 4 段階: サイドバー(ファイルリスト + 追加 + 削除 + 選択 + Export)と
//! メタデータパネル。イベントテーブル (Phase 5) はメインペインへ後で足す。

use std::sync::Arc;

use gpui::{
    AnyElement, AppContext as _, ClickEvent, Context, Entity, ExternalPaths, FontWeight,
    InteractiveElement, IntoElement, ParentElement, Render, StatefulInteractiveElement, Styled,
    Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Theme, ThemeMode,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    scroll::ScrollableElement as _,
    table::{DataTable, TableState},
    v_flex,
};
use mcpr_app::{
    EntryKind, EntryState, EventFilter, ExportFormat, ExportPhase, Loaded, SelectionAction,
    speed_label,
};

use crate::{events_table::EventsTableDelegate, settings::Theme as AppTheme, store::AppStore};

pub struct Workspace {
    store: Entity<AppStore>,
    /// 表示中ファイルのイベントテーブル (仮想スクロール状態を保持するため
    /// 永続 Entity。表示ファイルが変わったら作り直す)。
    events_table: Option<EventsTable>,
}

/// イベントテーブルの保持。`file_id` は作成元ファイルで、切り替え検知に使う。
struct EventsTable {
    file_id: u64,
    state: Entity<TableState<EventsTableDelegate>>,
    /// 検索入力 (packet id / custom 名でフィルタ)。ファイルごとに作り直す。
    search: Entity<InputState>,
}

/// 描画用に owned へ落とした 1 行 (store の借用を保持したまま listener を
/// 作れないため、先に必要な値だけ取り出す)。
enum RowDisplay {
    File {
        id: u64,
        filename: String,
        status: FileStatus,
        active: bool,
    },
    Interval {
        id: u64,
        label: String,
    },
}

enum FileStatus {
    Loading,
    Loaded {
        format: &'static str,
        duration_ms: u64,
        events: usize,
    },
    Error(String),
}

/// DnD 並べ替えのドラッグ荷物 (エントリ id)。
#[derive(Clone)]
struct DraggedEntry {
    id: u64,
}

/// ドラッグ中に追従表示する小さなプレビュー。
struct DragPreview(String);

impl Render for DragPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded(px(4.))
            .bg(cx.theme().accent)
            .text_color(cx.theme().accent_foreground)
            .child(self.0.clone())
    }
}

impl Workspace {
    pub fn new(store: Entity<AppStore>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&store, |_this, _store, cx| cx.notify()).detach();
        let mode = theme_mode(store.read(cx).theme);
        Theme::change(mode, None, cx);
        Self {
            store,
            events_table: None,
        }
    }

    /// 表示中ファイルに合わせてイベントテーブルの Entity を用意する。
    /// 表示ファイルが変わった (または無くなった) ときだけ作り直す。
    fn sync_events_table(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 毎再描画で走るため、まずは複製の要らない id だけで一致判定する
        // (active_loaded は speed 文字列を確保し Loaded を clone するため使わない)。
        let Some(id) = self.store.read(cx).active_file_id() else {
            self.events_table = None;
            return;
        };
        if self.events_table.as_ref().map(|t| t.file_id) == Some(id) {
            return;
        }
        // 表示ファイルが変わったときだけ、必要な events (Arc) だけを取り出す。
        let events = {
            let store = self.store.read(cx);
            store
                .files
                .active_loaded(store.selected_file_id)
                .map(|(_, loaded, _)| loaded.events.clone())
        };
        let Some(events) = events else { return };
        let store = self.store.downgrade();
        let state =
            cx.new(|cx| TableState::new(EventsTableDelegate::new(store, id, events), window, cx));
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder("filter: 0x2c / move_entities"));
        cx.subscribe_in(&search, window, Self::on_search_change)
            .detach();
        self.events_table = Some(EventsTable {
            file_id: id,
            state,
            search,
        });
    }

    /// テーブルのフィルタ (カテゴリ/検索) を差し替えて再描画する。
    fn set_events_filter(&mut self, filter: EventFilter, cx: &mut Context<Self>) {
        if let Some(t) = &self.events_table {
            t.state.update(cx, |state, cx| {
                state.delegate_mut().set_filter(filter);
                state.refresh(cx);
            });
        }
    }

    /// 対象ファイルの全パケットを一括採否する。
    fn set_all_packets(&mut self, file_id: u64, on: bool, cx: &mut Context<Self>) {
        self.store.update(cx, |store, cx| {
            store.dispatch_selection(SelectionAction::SetAll { file_id, on }, cx);
        });
    }

    /// フィルタ/ソートで表示中の行だけをまとめて採否する
    /// (web 版のドラッグ範囲選択に相当する一括操作)。
    fn set_visible_packets(&mut self, file_id: u64, on: bool, cx: &mut Context<Self>) {
        // 絞り込みが無く全行が対象なら、全 index を集合へ積む O(N) を避けて
        // O(1) の全採否で済ませる (SetMany は全件を flipped へ入れてしまう)。
        let Some((covers_all, indices)) = self.events_table.as_ref().map(|t| {
            let delegate = t.state.read(cx).delegate();
            if delegate.covers_all_rows() {
                (true, std::collections::HashSet::new())
            } else {
                (false, delegate.visible_indices_set())
            }
        }) else {
            return;
        };
        if covers_all {
            self.set_all_packets(file_id, on, cx);
        } else if !indices.is_empty() {
            self.store.update(cx, |store, cx| {
                store.set_many_packets(file_id, indices, on, cx)
            });
        }
    }

    /// 検索入力の変更でフィルタのクエリを更新する (カテゴリ選択は保持)。
    fn on_search_change(
        &mut self,
        state: &Entity<InputState>,
        event: &InputEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let InputEvent::Change = event {
            let text = state.read(cx).value().to_string();
            let filter = self
                .events_table
                .as_ref()
                .map(|t| t.state.read(cx).delegate().filter().with_query(text));
            if let Some(filter) = filter {
                self.set_events_filter(filter, cx);
            }
        }
    }

    fn on_toggle_theme(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.store.update(cx, |store, cx| store.toggle_theme(cx));
        let mode = theme_mode(self.store.read(cx).theme);
        Theme::change(mode, None, cx);
    }

    fn on_add_files(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.store.update(cx, |store, cx| store.open_files(cx));
    }

    fn on_add_interval(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        // 既定 1000ms の空白を末尾へ追加 (連結時のギャップ)。
        self.store
            .update(cx, |store, cx| store.add_interval(1000, cx));
    }

    fn on_export(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.store.update(cx, |store, cx| store.export(cx));
    }

    /// 左サイドバー: ファイル/interval リスト + 追加 + Export 行。
    fn render_sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let store = self.store.read(cx);
        let active_id = store.active_file_id();
        let rows: Vec<RowDisplay> = store
            .files
            .entries
            .iter()
            .map(|entry| match &entry.kind {
                EntryKind::File {
                    filename, state, ..
                } => RowDisplay::File {
                    id: entry.id,
                    filename: filename.clone(),
                    status: match state {
                        EntryState::Loading => FileStatus::Loading,
                        EntryState::Loaded { loaded, .. } => FileStatus::Loaded {
                            format: loaded.format,
                            duration_ms: loaded.info.duration_ms,
                            events: loaded.events.len(),
                        },
                        EntryState::Error(msg) => FileStatus::Error(msg.clone()),
                    },
                    active: active_id == Some(entry.id),
                },
                EntryKind::Interval { input } => RowDisplay::Interval {
                    id: entry.id,
                    label: format!("{} ms", input.trim().parse::<u64>().unwrap_or(0)),
                },
            })
            .collect();
        let file_count = store.files.file_count();
        let can_export = store.can_export();
        let export_format = store.export_format;
        let export_phase = store.export_phase;
        let export_error = store.export_error.clone();

        // 行要素は for ループで先に組む (map クロージャに &mut cx を閉じ込めると
        // 借用が escape するため)。
        let mut row_els = Vec::with_capacity(rows.len());
        for row in rows {
            row_els.push(self.render_row(row, cx));
        }

        v_flex()
            .w(px(320.))
            .h_full()
            .p_3()
            .gap_2()
            .border_r_1()
            .border_color(cx.theme().border)
            // ヘッダ
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .child(format!("Files ({file_count})")),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new("add-files")
                                    .label("+ Add")
                                    .on_click(cx.listener(Self::on_add_files)),
                            )
                            .child(
                                Button::new("add-interval")
                                    .label("+ Interval")
                                    .on_click(cx.listener(Self::on_add_interval)),
                            ),
                    ),
            )
            // リスト
            .child(
                v_flex()
                    .id("file-list")
                    .flex_1()
                    .gap_1()
                    .overflow_y_scrollbar()
                    .children(row_els),
            )
            // Export 行
            .child(self.render_export_row(export_format, can_export, export_phase, cx))
            .children(
                export_error.map(|msg| div().text_sm().text_color(cx.theme().danger).child(msg)),
            )
            .into_any_element()
    }

    /// 行末尾の削除ボタン (File/Interval 共通)。
    fn remove_button(&self, id: u64, cx: &mut Context<Self>) -> Button {
        Button::new(("remove", id as usize))
            .ghost()
            .label("✕")
            .on_click(cx.listener(move |this, _ev, _window, cx| {
                this.store.update(cx, |store, cx| store.remove_file(id, cx));
            }))
    }

    fn render_row(&self, row: RowDisplay, cx: &mut Context<Self>) -> AnyElement {
        match row {
            RowDisplay::File {
                id,
                filename,
                status,
                active,
            } => {
                let status_el = match status {
                    FileStatus::Loading => div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("loading…")
                        .into_any_element(),
                    FileStatus::Loaded {
                        format,
                        duration_ms,
                        events,
                    } => h_flex()
                        .gap_1()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format.to_string())
                        .child(format!("{duration_ms} ms"))
                        .child(format!("{events} events"))
                        .into_any_element(),
                    FileStatus::Error(msg) => div()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(msg)
                        .into_any_element(),
                };
                h_flex()
                    .id(id as usize)
                    .w_full()
                    .px_2()
                    .py_1()
                    .gap_2()
                    .justify_between()
                    .items_center()
                    .rounded(px(4.))
                    .when(active, |this| this.bg(cx.theme().accent))
                    .hover(|this| this.bg(cx.theme().muted))
                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                        this.store.update(cx, |store, cx| store.select_file(id, cx));
                    }))
                    // DnD 並べ替え: この行をドラッグ荷物にし、ドロップで Reorder。
                    .on_drag(DraggedEntry { id }, |dragged, _offset, _window, cx| {
                        cx.new(|_| DragPreview(format!("↕ #{}", dragged.id)))
                    })
                    .on_drop(
                        cx.listener(move |this, dragged: &DraggedEntry, _window, cx| {
                            this.store
                                .update(cx, |store, cx| store.reorder(dragged.id, id, cx));
                        }),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .overflow_hidden()
                            .child(div().text_sm().truncate().child(filename))
                            .child(status_el),
                    )
                    .child(self.remove_button(id, cx))
                    .into_any_element()
            }
            RowDisplay::Interval { id, label } => h_flex()
                .id(id as usize)
                .w_full()
                .px_2()
                .py_1()
                .gap_2()
                .justify_between()
                .items_center()
                .rounded(px(4.))
                .bg(cx.theme().muted)
                .child(div().text_sm().child(format!("⏱ {label}")))
                .child(self.remove_button(id, cx))
                .into_any_element(),
        }
    }

    fn render_export_row(
        &self,
        export_format: ExportFormat,
        can_export: bool,
        export_phase: Option<ExportPhase>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let progress_label = export_phase.map(phase_label);
        v_flex()
            .gap_2()
            .pt_2()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .gap_1()
                    .child(format_button(ExportFormat::Mcpr, export_format, cx))
                    .child(format_button(ExportFormat::Flashback, export_format, cx)),
            )
            .children(progress_label.map(|label| {
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(label)
            }))
            .child(
                Button::new("export")
                    .primary()
                    .label("Export")
                    .disabled(!can_export)
                    .on_click(cx.listener(Self::on_export)),
            )
            .into_any_element()
    }

    /// 右メイン: 選択中ファイルのメタデータ + イベントテーブル。
    fn render_main(&self, cx: &mut Context<Self>) -> AnyElement {
        let active = self.store.read(cx).active_loaded();
        match active {
            Some((id, loaded, speed_input)) => v_flex()
                .flex_1()
                .h_full()
                .p_4()
                .gap_4()
                .child(self.render_metadata(id, &loaded, &speed_input, cx))
                .child(self.render_events_table(&loaded, cx))
                .into_any_element(),
            None => v_flex()
                .flex_1()
                .h_full()
                .p_4()
                .child(
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .child("読み込み済みファイルを選択すると詳細を表示します。"),
                )
                .into_any_element(),
        }
    }

    /// 仮想スクロールのイベントテーブル + ツールバー
    /// (カテゴリフィルタトグル / 全選択・全解除 / 件数)。
    fn render_events_table(&self, loaded: &Arc<Loaded>, cx: &mut Context<Self>) -> AnyElement {
        let Some(t) = &self.events_table else {
            return div().into_any_element();
        };
        let file_id = t.file_id;
        let total = loaded.events.len();
        // 各カテゴリボタンの listener へ move するため Arc で包み、ボタンごとの
        // clone を String 2 本の複製ではなく参照カウント増だけにする。
        let filter = Arc::new(t.state.read(cx).delegate().filter().clone());
        let selected = self
            .store
            .read(cx)
            .selection
            .by_file
            .get(&file_id)
            .map_or(total, |s| s.on_count(total));

        // カテゴリトグル (非表示でないカテゴリを primary で強調)。
        let mut cat_buttons: Vec<AnyElement> = Vec::new();
        for &cat in &loaded.categories {
            let active = !filter.is_hidden(cat);
            let f = filter.clone();
            let btn = Button::new(("cat", cat.bit() as usize)).label(cat.label());
            let btn = if active { btn.primary() } else { btn };
            cat_buttons.push(
                btn.on_click(cx.listener(move |this, _ev, _window, cx| {
                    this.set_events_filter(f.with_toggled(cat), cx);
                }))
                .into_any_element(),
            );
        }

        v_flex()
            .flex_1()
            .min_h(px(0.))
            .gap_2()
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!("Events ({selected} / {total})")),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(Button::new("sel-all").label("All").on_click(cx.listener(
                                move |this, _ev, _window, cx| {
                                    this.set_all_packets(file_id, true, cx);
                                },
                            )))
                            .child(Button::new("sel-none").label("None").on_click(cx.listener(
                                move |this, _ev, _window, cx| {
                                    this.set_all_packets(file_id, false, cx);
                                },
                            )))
                            .child(Button::new("sel-shown-on").label("On (shown)").on_click(
                                cx.listener(move |this, _ev, _window, cx| {
                                    this.set_visible_packets(file_id, true, cx);
                                }),
                            ))
                            .child(Button::new("sel-shown-off").label("Off (shown)").on_click(
                                cx.listener(move |this, _ev, _window, cx| {
                                    this.set_visible_packets(file_id, false, cx);
                                }),
                            )),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        h_flex()
                            .gap_1()
                            .flex_wrap()
                            .child(div())
                            .children(cat_buttons),
                    )
                    .child(div().flex_1())
                    .child(div().w(px(240.)).child(Input::new(&t.search))),
            )
            .child(DataTable::new(&t.state))
            .into_any_element()
    }

    fn render_metadata(
        &self,
        file_id: u64,
        loaded: &Arc<Loaded>,
        speed_input: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let info = &loaded.info;
        v_flex()
            .gap_1()
            .text_sm()
            .child(div().font_weight(FontWeight::SEMIBOLD).child("Metadata"))
            .child(meta_row("File", loaded.filename.clone(), cx))
            .child(meta_row("format", loaded.format.to_string(), cx))
            .child(meta_row("mcversion", info.mc_version.clone(), cx))
            .child(meta_row("protocol", info.protocol_version.to_string(), cx))
            .child(meta_row("duration (ms)", info.duration_ms.to_string(), cx))
            .child(meta_row(
                "dataVersion",
                info.data_version
                    .map_or_else(|| "—".to_string(), |v| v.to_string()),
                cx,
            ))
            .child(self.render_speed_row(file_id, speed_input, cx))
            .child(meta_row("players", info.players.len().to_string(), cx))
            .child(meta_row("events", loaded.events.len().to_string(), cx))
            .into_any_element()
    }

    /// speed 行: −/値/+ で 0.5 刻みに調整 (最小 0.5x)。web 版の Change Speed に相当。
    fn render_speed_row(
        &self,
        file_id: u64,
        speed_input: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current: f64 = speed_input.trim().parse().unwrap_or(1.0);
        let dec = (current - 0.5).max(0.5);
        let inc = current + 0.5;
        h_flex()
            .gap_2()
            .items_center()
            .child(
                div()
                    .w(px(120.))
                    .text_color(cx.theme().muted_foreground)
                    .child("speed:"),
            )
            .child(
                Button::new(("speed-dec", file_id as usize))
                    .label("−")
                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                        this.store.update(cx, |store, cx| {
                            store.set_speed(file_id, dec.to_string(), cx)
                        });
                    })),
            )
            .child(div().min_w(px(48.)).child(speed_label(speed_input)))
            .child(
                Button::new(("speed-inc", file_id as usize))
                    .label("+")
                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                        this.store.update(cx, |store, cx| {
                            store.set_speed(file_id, inc.to_string(), cx)
                        });
                    })),
            )
            .into_any_element()
    }
}

fn meta_row(label: &str, value: String, cx: &mut Context<Workspace>) -> AnyElement {
    h_flex()
        .gap_2()
        .child(
            div()
                .w(px(120.))
                .text_color(cx.theme().muted_foreground)
                .child(format!("{label}:")),
        )
        .child(div().child(value))
        .into_any_element()
}

fn format_button(
    format: ExportFormat,
    current: ExportFormat,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let active = format == current;
    let button = Button::new(match format {
        ExportFormat::Mcpr => "fmt-mcpr",
        ExportFormat::Flashback => "fmt-flashback",
    })
    .label(format.label());
    // 選択中は primary で強調 (Button は Styled でないため if で分岐)。
    let button = if active { button.primary() } else { button };
    button
        .on_click(cx.listener(move |this, _ev, _window, cx| {
            this.store
                .update(cx, |store, cx| store.set_export_format(format, cx));
        }))
        .into_any_element()
}

fn phase_label(phase: ExportPhase) -> String {
    match phase {
        ExportPhase::Preparing => "準備中…".to_string(),
        // percent 算出は mcpr-app に集約 (web 版と共有)。
        ExportPhase::Events { .. } => format!("{}%", phase.percent().unwrap_or(0)),
        ExportPhase::Finishing => "圧縮中…".to_string(),
    }
}

fn theme_mode(theme: AppTheme) -> ThemeMode {
    match theme {
        AppTheme::Light => ThemeMode::Light,
        AppTheme::Dark => ThemeMode::Dark,
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 表示中ファイルに合わせてテーブル Entity を用意 (切り替え時のみ作り直す)。
        self.sync_events_table(window, cx);

        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            // OS からのファイルドロップで読み込む。
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _window, cx| {
                let paths = paths.paths().to_vec();
                this.store
                    .update(cx, |store, cx| store.load_paths(paths, cx));
            }))
            .child(
                h_flex()
                    .w_full()
                    .px_4()
                    .py_2()
                    .justify_between()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("mcpr editor"))
                    .child(
                        Button::new("toggle-theme")
                            .label("Theme")
                            .on_click(cx.listener(Self::on_toggle_theme)),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .w_full()
                    .child(self.render_sidebar(cx))
                    .child(self.render_main(cx)),
            )
    }
}
