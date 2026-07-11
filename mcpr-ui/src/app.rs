use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::Arc,
};

use gloo_file::{
    File as GlooFile,
    callbacks::{FileReader, read_as_bytes},
};
use mcpr_app::{
    EntryKind, EntryState, EventFilter, ExportFormat, ExportPhase, ExportPlan, FilesAction,
    FilesState, Loaded, PacketSelection, RowKind, SelectionAction, SelectionState, SortDir,
    SortKey, as_merge_inputs, build_export_plan, compute_indices, export_filename, export_merged,
    new_replay_uuid, parse_replay, speed_label, state_name,
};
use web_sys::{DragEvent, Event, HtmlDetailsElement, HtmlInputElement};
use yew::prelude::*;

use crate::export::trigger_download;

const PAGE_SIZE: usize = 200;

// index.html の起動前スクリプトと揃えること。
const THEME_STORAGE_KEY: &str = "mcpr-ui-theme";

#[derive(Clone, Copy, PartialEq)]
pub enum Theme {
    Light,
    Dark,
}

impl Theme {
    fn as_str(self) -> &'static str {
        match self {
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }

    fn toggled(self) -> Self {
        match self {
            Theme::Light => Theme::Dark,
            Theme::Dark => Theme::Light,
        }
    }
}

/// localStorage の保存値、なければ OS の配色設定から初期テーマを決める。
fn initial_theme() -> Theme {
    let Some(window) = web_sys::window() else {
        return Theme::Light;
    };
    if let Ok(Some(storage)) = window.local_storage()
        && let Ok(Some(saved)) = storage.get_item(THEME_STORAGE_KEY)
    {
        match saved.as_str() {
            "light" => return Theme::Light,
            "dark" => return Theme::Dark,
            _ => {}
        }
    }
    match window.match_media("(prefers-color-scheme: dark)") {
        Ok(Some(mql)) if mql.matches() => Theme::Dark,
        _ => Theme::Light,
    }
}

/// <html data-theme="..."> を書き換え、選択を localStorage へ保存する。
fn apply_theme(theme: Theme) {
    let Some(window) = web_sys::window() else {
        return;
    };
    if let Some(root) = window.document().and_then(|d| d.document_element()) {
        let _ = root.set_attribute("data-theme", theme.as_str());
    }
    if let Ok(Some(storage)) = window.local_storage() {
        let _ = storage.set_item(THEME_STORAGE_KEY, theme.as_str());
    }
}

/// [`FilesState`] を Yew の [`Reducible`] で包む器。状態遷移の本体は
/// UI 非依存の [`FilesState::reduce`] にあり、ここでは Yew のフックに
/// 適合させるだけ (orphan rule のため共有クレート側に impl できない)。
#[derive(Default)]
struct Files(FilesState);

impl std::ops::Deref for Files {
    type Target = FilesState;
    fn deref(&self) -> &FilesState {
        &self.0
    }
}

impl Reducible for Files {
    type Action = FilesAction;
    fn reduce(self: Rc<Self>, action: FilesAction) -> Rc<Self> {
        Rc::new(Files(self.0.reduce(action)))
    }
}

/// [`SelectionState`] を Yew の [`Reducible`] で包む器 ([`Files`] と同様)。
#[derive(Default)]
struct Selection(SelectionState);

impl std::ops::Deref for Selection {
    type Target = SelectionState;
    fn deref(&self) -> &SelectionState {
        &self.0
    }
}

impl Reducible for Selection {
    type Action = SelectionAction;
    fn reduce(self: Rc<Self>, action: SelectionAction) -> Rc<Self> {
        Rc::new(Selection(self.0.reduce(action)))
    }
}

fn file_list_to_vec(files: &web_sys::FileList) -> Vec<web_sys::File> {
    (0..files.length()).filter_map(|i| files.get(i)).collect()
}

/// interval 入力ダイアログの開閉と対象。`Add` は新規追加、`Edit(id)` は
/// 既存 interval の値編集 (位置はそのまま)。
#[derive(Clone, Copy, PartialEq)]
enum IntervalDialog {
    Closed,
    Add,
    Edit(u64),
}

/// speed 入力ダイアログの開閉と対象。
#[derive(Clone, Copy, PartialEq)]
enum SpeedDialog {
    Closed,
    Edit(u64),
}

#[function_component]
pub fn App() -> Html {
    let files = use_reducer(Files::default);
    // パケット採否 (ファイル id 単位)。エントリの無いファイルは全採用。
    let selection = use_reducer(Selection::default);
    // 未編集ファイルへ渡す全採用の既定値。ポインタを安定させ無駄な再描画を避ける。
    let empty_selection = use_state(|| Arc::new(PacketSelection::default()));
    let selected_file_id = use_state(|| Option::<u64>::None);
    let dragging_file_id = use_state(|| Option::<u64>::None);
    let drop_target_file_id = use_state(|| Option::<u64>::None);
    // ドロップ直後に着地した行を一瞬ハイライトする (タイマーで自動解除)。
    let just_moved_file_id = use_state(|| Option::<u64>::None);
    // 読み込み中の FileReader を id 別に保持。remove で drop され読み込みは中断される。
    let readers = use_mut_ref(HashMap::<u64, FileReader>::new);
    // Entry::id の発番カウンタ。
    let next_id = use_mut_ref(|| 0u64);

    // interval 入力ダイアログ。draft は数値欄の原文を保持し、parse 失敗は 0 扱い。
    let interval_dialog = use_state(|| IntervalDialog::Closed);
    let interval_draft = use_state(String::new);
    let speed_dialog = use_state(|| SpeedDialog::Closed);
    let speed_draft = use_state(String::new);

    // 書き出し設定と進行状態 (None = 書き出し中でない)。
    let export_format = use_state(ExportFormat::default);
    let export_phase = use_state(|| Option::<ExportPhase>::None);
    let export_error = use_state(|| Option::<String>::None);

    let theme = use_state(initial_theme);
    use_effect_with(*theme, |t| apply_theme(*t));

    // アップロードモーダルの開閉状態。0 件時の「Add files」と Files ヘッダの「+」の両方から開く。
    let upload_modal_open = use_state(|| false);
    // Files ヘッダの「+」ドロップダウン。項目クリック後に閉じるため <details> を参照する。
    let upload_dropdown_ref = use_node_ref();

    let on_toggle_theme = {
        let theme = theme.clone();
        Callback::from(move |_: Event| theme.set(theme.toggled()))
    };

    let on_files = {
        let dispatch = files.dispatcher();
        let readers = readers.clone();
        let next_id = next_id.clone();
        Callback::from(move |list: Vec<web_sys::File>| {
            for file in list {
                let id = {
                    let mut n = next_id.borrow_mut();
                    *n += 1;
                    *n
                };
                let filename = file.name();
                dispatch.dispatch(FilesAction::Add {
                    id,
                    filename: filename.clone(),
                });
                let dispatch = dispatch.clone();
                let readers_done = readers.clone();
                let task = read_as_bytes(&GlooFile::from(file), move |result| {
                    readers_done.borrow_mut().remove(&id);
                    let result = match result {
                        Ok(bytes) => {
                            // 書き出し時の再パース用に生バイト列も保持する。
                            let bytes = Arc::new(bytes);
                            parse_replay(filename, &bytes)
                                .map(|loaded| (Arc::new(loaded), bytes))
                                .map_err(|e| format!("parse error: {e}"))
                        }
                        Err(e) => Err(format!("read error: {e:?}")),
                    };
                    dispatch.dispatch(FilesAction::Finish { id, result });
                });
                readers.borrow_mut().insert(id, task);
            }
        })
    };

    let on_input_change = {
        let on_files = on_files.clone();
        Callback::from(move |e: Event| {
            let input: HtmlInputElement = e.target_unchecked_into();
            if let Some(files) = input.files() {
                on_files.emit(file_list_to_vec(&files));
            }
            // 削除後に同じファイルを選び直しても change が発火するようにする。
            input.set_value("");
        })
    };

    let on_drop_handler = {
        let on_files = on_files.clone();
        Callback::from(move |e: DragEvent| {
            e.prevent_default();
            if let Some(dt) = e.data_transfer()
                && let Some(files) = dt.files()
            {
                on_files.emit(file_list_to_vec(&files));
            }
        })
    };

    let on_upload_dragover = Callback::from(|e: DragEvent| e.prevent_default());

    let on_open_modal = {
        let upload_modal_open = upload_modal_open.clone();
        Callback::from(move |_: MouseEvent| upload_modal_open.set(true))
    };
    let on_close_modal = {
        let upload_modal_open = upload_modal_open.clone();
        Callback::from(move |_: MouseEvent| upload_modal_open.set(false))
    };
    let on_upload_files_item = {
        let upload_modal_open = upload_modal_open.clone();
        let upload_dropdown_ref = upload_dropdown_ref.clone();
        Callback::from(move |_: MouseEvent| {
            upload_modal_open.set(true);
            // 項目クリックでは <details> が自動で閉じないため明示的に閉じる。
            if let Some(d) = upload_dropdown_ref.cast::<HtmlDetailsElement>() {
                d.set_open(false);
            }
        })
    };
    // ファイル選択/ドロップ後はモーダルを閉じる (読み込みは非同期に継続)。
    let on_modal_input_change = {
        let on_input_change = on_input_change.clone();
        let upload_modal_open = upload_modal_open.clone();
        Callback::from(move |e: Event| {
            on_input_change.emit(e);
            upload_modal_open.set(false);
        })
    };
    let on_modal_drop = {
        let on_drop_handler = on_drop_handler.clone();
        let upload_modal_open = upload_modal_open.clone();
        Callback::from(move |e: DragEvent| {
            on_drop_handler.emit(e);
            upload_modal_open.set(false);
        })
    };

    // 「+」→「Add Interval」: 空の draft でダイアログを開き、ドロップダウンを閉じる。
    let on_add_interval_item = {
        let interval_dialog = interval_dialog.clone();
        let interval_draft = interval_draft.clone();
        let upload_dropdown_ref = upload_dropdown_ref.clone();
        Callback::from(move |_: MouseEvent| {
            interval_draft.set(String::new());
            interval_dialog.set(IntervalDialog::Add);
            if let Some(d) = upload_dropdown_ref.cast::<HtmlDetailsElement>() {
                d.set_open(false);
            }
        })
    };
    // interval 行の値クリック: 現在値を draft に載せて Edit モードで開く。
    let on_edit_interval = {
        let interval_dialog = interval_dialog.clone();
        let interval_draft = interval_draft.clone();
        Callback::from(move |(id, current): (u64, String)| {
            interval_draft.set(current);
            interval_dialog.set(IntervalDialog::Edit(id));
        })
    };
    let on_interval_draft = {
        let interval_draft = interval_draft.clone();
        Callback::from(move |e: InputEvent| {
            let input: HtmlInputElement = e.target_unchecked_into();
            interval_draft.set(input.value());
        })
    };
    let on_interval_confirm = {
        let dispatch = files.dispatcher();
        let next_id = next_id.clone();
        let interval_dialog = interval_dialog.clone();
        let interval_draft = interval_draft.clone();
        Callback::from(move |_: MouseEvent| {
            let input = (*interval_draft).clone();
            match *interval_dialog {
                IntervalDialog::Add => {
                    let id = {
                        let mut n = next_id.borrow_mut();
                        *n += 1;
                        *n
                    };
                    dispatch.dispatch(FilesAction::AddInterval { id, input });
                }
                IntervalDialog::Edit(id) => {
                    dispatch.dispatch(FilesAction::SetInterval { id, input });
                }
                IntervalDialog::Closed => {}
            }
            interval_dialog.set(IntervalDialog::Closed);
        })
    };
    let on_interval_cancel = {
        let interval_dialog = interval_dialog.clone();
        Callback::from(move |_: MouseEvent| interval_dialog.set(IntervalDialog::Closed))
    };
    let on_edit_speed = {
        let speed_dialog = speed_dialog.clone();
        let speed_draft = speed_draft.clone();
        Callback::from(move |(id, current): (u64, String)| {
            speed_draft.set(current);
            speed_dialog.set(SpeedDialog::Edit(id));
        })
    };
    let on_speed_draft = {
        let speed_draft = speed_draft.clone();
        Callback::from(move |e: InputEvent| {
            let input: HtmlInputElement = e.target_unchecked_into();
            speed_draft.set(input.value());
        })
    };
    let on_speed_confirm = {
        let dispatch = files.dispatcher();
        let speed_dialog = speed_dialog.clone();
        let speed_draft = speed_draft.clone();
        Callback::from(move |_: MouseEvent| {
            if let SpeedDialog::Edit(id) = *speed_dialog {
                dispatch.dispatch(FilesAction::SetSpeed {
                    id,
                    input: (*speed_draft).clone(),
                });
            }
            speed_dialog.set(SpeedDialog::Closed);
        })
    };
    let on_speed_cancel = {
        let speed_dialog = speed_dialog.clone();
        Callback::from(move |_: MouseEvent| speed_dialog.set(SpeedDialog::Closed))
    };

    // 表示中の Loaded ファイル (選択中、なければ先頭)。共有ロジックが返す借用を
    // 下流で move する行があるため、ここで owned へ複製する。
    let selected_file = files
        .active_loaded(*selected_file_id)
        .map(|(id, loaded, speed_input)| (id, loaded.clone(), speed_input.to_string()));
    let active_file_id = selected_file.as_ref().map(|(id, _, _)| *id);

    let on_remove_file = {
        let dispatch = files.dispatcher();
        let selection_dispatch = selection.dispatcher();
        let readers = readers.clone();
        let selected_file_id = selected_file_id.clone();
        Callback::from(move |id: u64| {
            // 読み込み中なら FileReader の drop で中断される。
            readers.borrow_mut().remove(&id);
            if *selected_file_id == Some(id) {
                selected_file_id.set(None);
            }
            dispatch.dispatch(FilesAction::Remove { id });
            selection_dispatch.dispatch(SelectionAction::Remove { file_id: id });
        })
    };

    // ファイルと interval を 1 つのリストに描画する。DnD ハンドラは id ベースで
    // kind 非依存なので両 kind で共有し、interval 行はグリップだけを draggable に
    // する (常設 input が無いので draggable 内 input の不具合は起きない)。
    let file_rows = {
        let mut rows: Vec<Html> = Vec::with_capacity(files.entries.len());
        // 行 index 表示用のファイル連番 (interval は数えない)。
        let mut file_no = 0usize;
        for entry in files.entries.iter() {
            let id = entry.id;
            let is_dragging = *dragging_file_id == Some(id);
            let is_drop_target = *drop_target_file_id == Some(id) && !is_dragging;
            let is_just_moved = *just_moved_file_id == Some(id);
            let dnd_classes = classes!(
                is_dragging.then_some("is-dragging"),
                is_drop_target.then_some("is-drop-target"),
                is_just_moved.then_some("is-just-moved"),
            );
            let on_tab_dragstart = {
                let dragging_file_id = dragging_file_id.clone();
                Callback::from(move |e: DragEvent| {
                    e.stop_propagation();
                    dragging_file_id.set(Some(id));
                    if let Some(dt) = e.data_transfer() {
                        dt.set_effect_allowed("move");
                        let _ = dt.set_data("text/plain", &id.to_string());
                    }
                })
            };
            let on_tab_dragover = {
                let drop_target_file_id = drop_target_file_id.clone();
                Callback::from(move |e: DragEvent| {
                    e.prevent_default();
                    e.stop_propagation();
                    if let Some(dt) = e.data_transfer() {
                        dt.set_drop_effect("move");
                    }
                    drop_target_file_id.set(Some(id));
                })
            };
            let on_tab_drop = {
                let dispatch = files.dispatcher();
                let dragging_file_id = dragging_file_id.clone();
                let drop_target_file_id = drop_target_file_id.clone();
                let just_moved_file_id = just_moved_file_id.clone();
                Callback::from(move |e: DragEvent| {
                    e.prevent_default();
                    e.stop_propagation();
                    let dragged_id = (*dragging_file_id).or_else(|| {
                        e.data_transfer()
                            .and_then(|dt| dt.get_data("text/plain").ok())
                            .and_then(|id| id.parse::<u64>().ok())
                    });
                    if let Some(dragged_id) = dragged_id
                        && dragged_id != id
                    {
                        dispatch.dispatch(FilesAction::Reorder {
                            dragged_id,
                            target_id: id,
                        });
                        // 移動した行を着地ハイライト。CSS アニメーションを次回も
                        // 再生させるため、再生時間ぶん待ってから解除する。
                        just_moved_file_id.set(Some(dragged_id));
                        let just_moved_file_id = just_moved_file_id.clone();
                        yew::platform::spawn_local(async move {
                            gloo_timers::future::TimeoutFuture::new(450).await;
                            just_moved_file_id.set(None);
                        });
                    }
                    dragging_file_id.set(None);
                    drop_target_file_id.set(None);
                })
            };
            let on_tab_dragend = {
                let dragging_file_id = dragging_file_id.clone();
                let drop_target_file_id = drop_target_file_id.clone();
                Callback::from(move |e: DragEvent| {
                    e.stop_propagation();
                    dragging_file_id.set(None);
                    drop_target_file_id.set(None);
                })
            };

            let row = match &entry.kind {
                EntryKind::File {
                    filename, state, ..
                } => {
                    let is_loaded = matches!(state, EntryState::Loaded { .. });
                    let is_active = active_file_id == Some(id);
                    let index = file_no;
                    file_no += 1;
                    let select = {
                        let selected_file_id = selected_file_id.clone();
                        Callback::from(move |_| {
                            if is_loaded {
                                selected_file_id.set(Some(id));
                            }
                        })
                    };
                    let status = match state {
                        EntryState::Loading => html! {
                            <span class="loading loading-spinner loading-xs"></span>
                        },
                        EntryState::Loaded { loaded: l, .. } => html! {
                            <>
                                <span class="mcpr-badge">{ l.format }</span>
                                <span class="mcpr-badge">{ format!("{} ms", l.info.duration_ms) }</span>
                                <span class="mcpr-badge">{ format!("{} events", l.events.len()) }</span>
                            </>
                        },
                        EntryState::Error(msg) => html! {
                            <span class="text-sm truncate text-error" title={msg.clone()}>{ msg }</span>
                        },
                    };
                    html! {
                        <li key={id.to_string()} class={classes!(
                            "mcpr-file-tab-row",
                            is_active.then_some("is-active"),
                            (!is_loaded).then_some("is-unavailable"),
                            dnd_classes,
                        )}>
                            <button type="button" class="mcpr-file-tab"
                                draggable="true"
                                aria-disabled={(!is_loaded).to_string()}
                                onclick={select}
                                ondragstart={on_tab_dragstart}
                                ondragover={on_tab_dragover}
                                ondrop={on_tab_drop}
                                ondragend={on_tab_dragend}>
                                <span class="mcpr-row-index">{ index }</span>
                                <span class="mcpr-filename" title={filename.clone()}>{ filename }</span>
                                <span class="mcpr-file-status">{ status }</span>
                            </button>
                        </li>
                    }
                }
                EntryKind::Interval { input } => {
                    let value_ms = input.trim().parse::<u64>().unwrap_or(0);
                    let edit = {
                        let on_edit_interval = on_edit_interval.clone();
                        let current = input.clone();
                        Callback::from(move |_: MouseEvent| {
                            on_edit_interval.emit((id, current.clone()));
                        })
                    };
                    let remove = {
                        let on_remove_file = on_remove_file.clone();
                        Callback::from(move |_: MouseEvent| on_remove_file.emit(id))
                    };
                    html! {
                        <li key={id.to_string()} class={classes!(
                            "mcpr-file-tab-row", "mcpr-interval-row", dnd_classes,
                        )}>
                            <div class="mcpr-interval-tab"
                                ondragover={on_tab_dragover}
                                ondrop={on_tab_drop}>
                                <span class="mcpr-interval-grip" draggable="true"
                                    ondragstart={on_tab_dragstart}
                                    ondragend={on_tab_dragend}
                                    aria-label="並べ替え" title="ドラッグで並べ替え">
                                    { "⠿" }
                                </span>
                                <span class="mcpr-row-index" aria-hidden="true">{ "⏱" }</span>
                                <button type="button" class="mcpr-interval-value"
                                    onclick={edit} title="クリックで interval を編集">
                                    { format!("{value_ms} ms") }
                                </button>
                                <button type="button"
                                    class="mcpr-icon-button mcpr-interval-remove"
                                    aria-label="間隔を削除" title="間隔を削除"
                                    onclick={remove}>
                                    { "✕" }
                                </button>
                            </div>
                        </li>
                    }
                }
            };
            rows.push(row);
        }
        rows.into_iter().collect::<Html>()
    };

    // Export は全ファイルの読み込み完了時のみ許可する (interval は阻害しない、
    // ファイルが 1 件も無ければ書き出すものが無い)。
    let file_count = files.file_count();
    let all_loaded = files.all_loaded();

    let on_export = {
        let files = files.clone();
        let selection = selection.clone();
        let export_phase = export_phase.clone();
        let export_error = export_error.clone();
        let format = *export_format;
        Callback::from(move |_: MouseEvent| {
            if export_phase.is_some() {
                return;
            }
            // ファイルリストと採否から連結入力を組む (mcpr-app の純関数)。Replay は
            // Arc を持って async ブロックへ move し、借用ビュー (&[MergeInput]) は
            // move 後にブロック内で作る (借用がダングらない)。
            let ExportPlan {
                items: owned,
                total_events: total,
                first_filename,
                file_count,
            } = match build_export_plan(&files, &selection) {
                Ok(Some(plan)) => plan,
                Ok(None) => return,
                Err(msg) => {
                    export_error.set(Some(msg));
                    return;
                }
            };
            let filename = export_filename(&first_filename, file_count > 1, format);
            export_phase.set(Some(ExportPhase::Preparing));
            export_error.set(None);
            let export_phase = export_phase.clone();
            let export_error = export_error.clone();
            // export_merged は一定イベント数ごとにブラウザへ yield するため、
            // async タスクとして流せば書き出し中も進捗バーが再描画される。
            yew::platform::spawn_local(async move {
                let items = as_merge_inputs(&owned);
                let on_progress = {
                    let export_phase = export_phase.clone();
                    move |progress| {
                        export_phase.set(Some(ExportPhase::from_progress(progress, total)));
                    }
                };
                let result = export_merged(&items, format, new_replay_uuid(), on_progress).await;
                match result {
                    Ok(bytes) => trigger_download(&bytes, &filename),
                    Err(e) => export_error.set(Some(format!("export error: {e}"))),
                }
                export_phase.set(None);
            });
        })
    };

    // 出力フォーマットの 2 択。選択中・非選択がひと目で分かるよう、トラックで
    // 囲んだセグメンテッドコントロールにし、選択中セグメントへチェックを付ける
    // (チェック枠は両セグメントに常設し、切り替えで幅がずれないようにする)。
    let format_buttons = ExportFormat::ORDER
        .iter()
        .map(|&f| {
            let export_format = export_format.clone();
            let is_active = *export_format == f;
            html! {
                <button key={f.extension()} type="button"
                    class={classes!("mcpr-segment", is_active.then_some("is-active"))}
                    aria-pressed={is_active.to_string()}
                    onclick={Callback::from(move |_| export_format.set(f))}>
                    <span class="mcpr-segment-check" aria-hidden="true">
                        { if is_active { "✓" } else { "" } }
                    </span>
                    { f.label() }
                </button>
            }
        })
        .collect::<Html>();

    // 進捗バー。イベント処理中は確定値、終端処理 (圧縮) 中は
    // value 無しの不確定表示にする。
    let progress_view = (*export_phase).map(|phase| {
        let (value, label) = match phase {
            ExportPhase::Preparing => (None, "準備中…".to_string()),
            ExportPhase::Events { done, total } => {
                // percent は共有ロジック (mcpr-app)、value 属性用の clamped は web 固有。
                let clamped = done.min(total);
                let percent = phase.percent().unwrap_or(0);
                let label = if done > 0 && percent == 0 {
                    "<1%".to_string()
                } else {
                    format!("{percent}%")
                };
                (Some(clamped.to_string()), label)
            }
            ExportPhase::Finishing => (None, "圧縮中…".to_string()),
        };
        let max = match phase {
            ExportPhase::Preparing => "1".to_string(),
            ExportPhase::Events { total, .. } => total.max(1).to_string(),
            ExportPhase::Finishing => "1".to_string(),
        };
        html! {
            <div class="flex items-center gap-2 grow min-w-40">
                <progress class="progress mcpr-progress" value={value} max={max}></progress>
                <span class="text-xs font-mono shrink-0 mcpr-muted w-12 text-right">
                    { label }
                </span>
            </div>
        }
    });

    // 書き出し行。1 件でも意味がある (フォーマット変換 = CLI の単一入力動作)。
    let export_row = html! {
        <div class="mcpr-divider-row mcpr-export-row text-sm">
            <div class="mcpr-segmented" role="group" aria-label="出力フォーマット">
                { format_buttons }
            </div>
            { progress_view }
            <button class="btn btn-sm mcpr-btn mcpr-btn-primary"
                disabled={!all_loaded || export_phase.is_some()}
                onclick={on_export}>
                { "Export" }
            </button>
        </div>
    };

    let interval_dialog_open = *interval_dialog != IntervalDialog::Closed;
    let (interval_dialog_title, interval_confirm_label) = match *interval_dialog {
        IntervalDialog::Edit(_) => ("interval を編集", "保存"),
        _ => ("interval を追加", "追加"),
    };
    let speed_dialog_open = *speed_dialog != SpeedDialog::Closed;

    // 選択中ファイルのパケット採否と、トグル/全選択のコールバック。
    // active_file_id が無いとき (未選択) は使われない既定値を渡す。
    let loaded_selection = active_file_id
        .and_then(|id| selection.by_file.get(&id).cloned())
        .unwrap_or_else(|| (*empty_selection).clone());
    let on_toggle_packet = {
        let selection_dispatch = selection.dispatcher();
        Callback::from(move |(file_id, index): (u64, usize)| {
            selection_dispatch.dispatch(SelectionAction::Toggle { file_id, index });
        })
    };
    let on_set_all_packets = {
        let selection_dispatch = selection.dispatcher();
        Callback::from(move |(file_id, on): (u64, bool)| {
            selection_dispatch.dispatch(SelectionAction::SetAll { file_id, on });
        })
    };
    let on_set_many_packets = {
        let selection_dispatch = selection.dispatcher();
        Callback::from(
            move |(file_id, indices, on): (u64, Arc<HashSet<usize>>, bool)| {
                selection_dispatch.dispatch(SelectionAction::SetMany {
                    file_id,
                    indices,
                    on,
                });
            },
        )
    };

    html! {
        <div class="mcpr-shell">
            <header class="mcpr-topbar">
                <div class="mcpr-topbar-inner">
                    <div class="mcpr-brand" aria-label="mcpr-ui">
                        <span class="mcpr-logo-mark" aria-hidden="true"></span>
                        <span>{ "mcpr-ui" }</span>
                    </div>
                    <nav class="mcpr-nav-actions" aria-label="Primary">
                        <a class="mcpr-nav-link"
                            href="https://github.com/topi-banana/mcpr-editor"
                            target="_blank" rel="noreferrer">
                            { "GitHub" }
                        </a>
                        <label class="swap swap-rotate mcpr-icon-button"
                            title="ライト/ダークテーマ切り替え" aria-label="ライト/ダークテーマ切り替え">
                            <input type="checkbox"
                                checked={*theme == Theme::Dark}
                                onchange={on_toggle_theme} />
                            // 太陽 = ライト時 (swap-off) / 月 = ダーク時 (swap-on)
                            <svg class="swap-off h-5 w-5 fill-current"
                                xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
                                <path d="M5.64,17l-.71.71a1,1,0,0,0,0,1.41,1,1,0,0,0,1.41,0l.71-.71A1,1,0,0,0,5.64,17ZM5,12a1,1,0,0,0-1-1H3a1,1,0,0,0,0,2H4A1,1,0,0,0,5,12Zm7-7a1,1,0,0,0,1-1V3a1,1,0,0,0-2,0V4A1,1,0,0,0,12,5ZM5.64,7.05a1,1,0,0,0,.7.29,1,1,0,0,0,.71-.29,1,1,0,0,0,0-1.41l-.71-.71A1,1,0,0,0,4.93,6.34Zm12,.29a1,1,0,0,0,.7-.29l.71-.71a1,1,0,1,0-1.41-1.41L17,5.64a1,1,0,0,0,0,1.41A1,1,0,0,0,17.66,7.34ZM21,11H20a1,1,0,0,0,0,2h1a1,1,0,0,0,0-2Zm-9,8a1,1,0,0,0-1,1v1a1,1,0,0,0,2,0V20A1,1,0,0,0,12,19ZM18.36,17A1,1,0,0,0,17,18.36l.71.71a1,1,0,0,0,1.41,0,1,1,0,0,0,0-1.41ZM12,6.5A5.5,5.5,0,1,0,17.5,12,5.51,5.51,0,0,0,12,6.5Zm0,9A3.5,3.5,0,1,1,15.5,12,3.5,3.5,0,0,1,12,15.5Z" />
                            </svg>
                            <svg class="swap-on h-5 w-5 fill-current"
                                xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
                                <path d="M21.64,13a1,1,0,0,0-1.05-.14,8.05,8.05,0,0,1-3.37.73A8.15,8.15,0,0,1,9.08,5.49a8.59,8.59,0,0,1,.25-2A1,1,0,0,0,8,2.36,10.14,10.14,0,1,0,22,14.05,1,1,0,0,0,21.64,13Zm-9.5,6.69A8.14,8.14,0,0,1,7.08,5.22v.27A10.15,10.15,0,0,0,17.22,15.63a9.79,9.79,0,0,0,2.1-.22A8.11,8.11,0,0,1,12.14,19.73Z" />
                            </svg>
                        </label>
                    </nav>
                </div>
            </header>

            <main class="mcpr-page space-y-6">
                if files.entries.is_empty() {
                    <section class="mcpr-empty-state">
                        <div class="space-y-3">
                            <p class="mcpr-eyebrow">{ "REPLAY WORKSPACE" }</p>
                            <h1 class="mcpr-hero-title">{ "Merge Minecraft replay archives." }</h1>
                            <p class="mcpr-hero-copy">
                                { "Drop .mcpr or Flashback .zip files, inspect the event stream, then export the merged archive." }
                            </p>
                        </div>
                        <button type="button" class="btn mcpr-btn mcpr-btn-primary"
                            onclick={on_open_modal.clone()}>
                            { "+ ファイルを追加" }
                        </button>
                    </section>
                }

                if !files.entries.is_empty() {
                    <section class="mcpr-workspace">
                        <aside class="mcpr-workspace-sidebar">
                            <div class="mcpr-panel mcpr-files-panel">
                                <div class="mcpr-panel-body">
                                    <div class="mcpr-section-header">
                                        <h2 class="mcpr-section-title">
                                            { "Files" }
                                            <span class="mcpr-badge">{ file_count }</span>
                                        </h2>
                                        <details class="dropdown dropdown-end" ref={upload_dropdown_ref.clone()}>
                                            <summary class="mcpr-icon-button"
                                                aria-label="追加" title="追加">
                                                { "+" }
                                            </summary>
                                            <ul class="dropdown-content menu mcpr-dropdown-menu">
                                                <li>
                                                    <button type="button" onclick={on_upload_files_item}>
                                                        { "Upload Files" }
                                                    </button>
                                                </li>
                                                <li>
                                                    <button type="button" onclick={on_add_interval_item}>
                                                        { "Add Interval" }
                                                    </button>
                                                </li>
                                            </ul>
                                        </details>
                                    </div>
                                    <ul class="mcpr-file-list">{ file_rows }</ul>
                                    { export_row }
                                    if let Some(msg) = export_error.as_ref() {
                                        <div class="alert text-sm py-2 mcpr-alert">{ msg }</div>
                                    }
                                </div>
                            </div>
                        </aside>
                        <div class="mcpr-workspace-main">
                            if let Some((id, data, speed_input)) = selected_file.as_ref() {
                                <LoadedView key={id.to_string()}
                                    id={*id}
                                    data={data.clone()}
                                    speed_input={speed_input.clone()}
                                    selection={loaded_selection.clone()}
                                    on_toggle={on_toggle_packet.clone()}
                                    on_set_all={on_set_all_packets.clone()}
                                    on_set_many={on_set_many_packets.clone()}
                                    on_edit_speed={on_edit_speed.clone()}
                                    on_remove={on_remove_file.clone()} />
                            } else {
                                <section class="mcpr-panel">
                                    <div class="mcpr-panel-body">
                                        <div class="mcpr-section-header">
                                            <h2 class="mcpr-section-title">{ "Events" }</h2>
                                        </div>
                                        <p class="mcpr-empty-copy">
                                            { "読み込み済みファイルを選択すると、ファイル単位のイベントを表示します。" }
                                        </p>
                                    </div>
                                </section>
                            }
                        </div>
                    </section>
                }

                <div class={classes!("modal", upload_modal_open.then_some("modal-open"))}
                    role="dialog" aria-modal="true">
                    <div class="modal-box mcpr-upload-modal-box">
                        <div class="mcpr-section-header">
                            <h3 class="mcpr-section-title">{ "ファイルを追加" }</h3>
                            <button type="button" class="mcpr-icon-button"
                                aria-label="閉じる" title="閉じる" onclick={on_close_modal.clone()}>
                                { "✕" }
                            </button>
                        </div>
                        <div class="mcpr-dropzone mcpr-modal-dropzone"
                            ondragover={on_upload_dragover}
                            ondrop={on_modal_drop}>
                            <div class="space-y-1">
                                <p class="mcpr-eyebrow">{ "INPUT" }</p>
                                <p class="text-sm text-base-content/70">
                                    { ".mcpr / Flashback (.zip) ファイルをドロップ (複数可)、またはファイルを選択" }
                                </p>
                            </div>
                            <input type="file" accept=".mcpr,.zip" multiple=true
                                class="file-input file-input-bordered w-full mcpr-file-input"
                                onchange={on_modal_input_change} />
                        </div>
                    </div>
                    // 背景クリックで閉じる
                    <div class="modal-backdrop" onclick={on_close_modal}></div>
                </div>

                <div class={classes!("modal", interval_dialog_open.then_some("modal-open"))}
                    role="dialog" aria-modal="true">
                    <div class="modal-box mcpr-interval-dialog-box">
                        <div class="mcpr-section-header">
                            <h3 class="mcpr-section-title">{ interval_dialog_title }</h3>
                            <button type="button" class="mcpr-icon-button"
                                aria-label="閉じる" title="閉じる" onclick={on_interval_cancel.clone()}>
                                { "✕" }
                            </button>
                        </div>
                        <label class="mcpr-field-row">
                            { "interval (ms)" }
                            <input type="number" min="0"
                                class="input input-bordered input-sm w-28 font-mono mcpr-form-input"
                                value={(*interval_draft).clone()}
                                oninput={on_interval_draft} />
                        </label>
                        <div class="flex justify-end">
                            <button type="button" class="btn btn-sm mcpr-btn mcpr-btn-primary"
                                onclick={on_interval_confirm}>
                                { interval_confirm_label }
                            </button>
                        </div>
                    </div>
                    // 背景クリックで閉じる
                    <div class="modal-backdrop" onclick={on_interval_cancel}></div>
                </div>

                <div class={classes!("modal", speed_dialog_open.then_some("modal-open"))}
                    role="dialog" aria-modal="true">
                    <div class="modal-box mcpr-speed-dialog-box">
                        <div class="mcpr-section-header">
                            <h3 class="mcpr-section-title">{ "Change Speed" }</h3>
                            <button type="button" class="mcpr-icon-button"
                                aria-label="閉じる" title="閉じる" onclick={on_speed_cancel.clone()}>
                                { "✕" }
                            </button>
                        </div>
                        <label class="mcpr-field-row">
                            { "speed (x)" }
                            <input type="number" min="0.000001" step="0.1"
                                class="input input-bordered input-sm w-28 font-mono mcpr-form-input"
                                value={(*speed_draft).clone()}
                                oninput={on_speed_draft} />
                        </label>
                        <div class="flex justify-end">
                            <button type="button" class="btn btn-sm mcpr-btn mcpr-btn-primary"
                                onclick={on_speed_confirm}>
                                { "保存" }
                            </button>
                        </div>
                    </div>
                    // 背景クリックで閉じる
                    <div class="modal-backdrop" onclick={on_speed_cancel}></div>
                </div>
            </main>
        </div>
    }
}

#[derive(Properties)]
struct LoadedViewProps {
    id: u64,
    data: Arc<Loaded>,
    speed_input: String,
    /// このファイルのパケット採否。
    selection: Arc<PacketSelection>,
    /// (file_id, 元 index) で 1 件の採否を反転。
    on_toggle: Callback<(u64, usize)>,
    /// (file_id, on) で全件を一括採否。
    on_set_all: Callback<(u64, bool)>,
    /// (file_id, 元 index 集合, on) でドラッグ範囲選択した複数件をまとめて採否。
    on_set_many: Callback<(u64, Arc<HashSet<usize>>, bool)>,
    on_edit_speed: Callback<(u64, String)>,
    on_remove: Callback<u64>,
}

/// events の深い比較 (数百万行になり得る) を避け、data と selection の
/// ポインタ同一性だけで再描画を判定する (コールバックは再描画に影響しない)。
impl PartialEq for LoadedViewProps {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && Arc::ptr_eq(&self.data, &other.data)
            && self.speed_input == other.speed_input
            && Arc::ptr_eq(&self.selection, &other.selection)
    }
}

/// use_memo の依存キー用に Arc をポインタ同一性で比較するラッパ。
/// (events の深い比較は数百万行に及ぶため避ける)
struct ArcPtr<T>(Arc<T>);

impl<T> PartialEq for ArcPtr<T> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

#[function_component]
fn LoadedView(props: &LoadedViewProps) -> Html {
    let page = use_state(|| 0usize);
    let filter = use_state(EventFilter::default);
    let sort = use_state(|| Option::<(SortKey, SortDir)>::None);
    // 全選択チェックボックスの中間状態 (indeterminate) は属性に無く DOM 直設定が要る。
    let header_check_ref = use_node_ref();
    // 行のドラッグ範囲選択。`mark_anchor` はドラッグ開始行のページ内位置 (押下中のみ
    // Some)、`marked` は選択済みの元 index 集合で、一括 On/Off の対象とハイライト表示に
    // 使う。export 採否 (`PacketSelection`) とは別概念の、UI だけの一時状態。
    let mark_anchor = use_state(|| Option::<usize>::None);
    let marked = use_state(|| Arc::new(HashSet::<usize>::new()));

    // 表示する行の元 index 列 (None = 全行を記録順のまま)。
    // filter / sort / events が変わった時だけ全行を走査する。
    let indices = use_memo(
        (ArcPtr(props.data.events.clone()), (*filter).clone(), *sort),
        |(events, filter, sort)| compute_indices(&events.0, filter, *sort),
    );
    let indices: &Option<Vec<usize>> = &indices;

    let all = &props.data.events;
    let total_all = all.len();
    let shown = indices.as_ref().map_or(total_all, Vec::len);
    let total_pages = shown.div_ceil(PAGE_SIZE).max(1);
    let cur_page = (*page).min(total_pages - 1);
    let start = cur_page * PAGE_SIZE;
    let end = (start + PAGE_SIZE).min(shown);

    // 現在ページの各表示位置 (0 始まり) → 元 index。ドラッグ範囲を元 index へ写すのに使う。
    let page_orig: Rc<Vec<usize>> = Rc::new(
        (start..end)
            .map(|pos| indices.as_ref().map_or(pos, |v| v[pos]))
            .collect(),
    );

    // パケット採否の集計 (全選択チェックボックスとカウント表示用)。
    let selected_count = props.selection.on_count(total_all);
    let all_selected = selected_count == total_all;
    let none_selected = selected_count == 0;
    let header_indeterminate = !all_selected && !none_selected;
    {
        // indeterminate は HTML 属性に無いため、描画後に DOM へ直接書く。
        let header_check_ref = header_check_ref.clone();
        use_effect_with(header_indeterminate, move |&indeterminate| {
            if let Some(input) = header_check_ref.cast::<HtmlInputElement>() {
                input.set_indeterminate(indeterminate);
            }
        });
    }

    // フィルタ変更時はページを先頭へ戻す。
    let apply_filter = {
        let filter = filter.clone();
        let page = page.clone();
        Callback::from(move |next: EventFilter| {
            page.set(0);
            filter.set(next);
        })
    };

    // ヘッダクリックで 昇順 → 降順 → 解除 を巡回する。
    let on_sort = {
        let sort = sort.clone();
        let page = page.clone();
        Callback::from(move |key: SortKey| {
            let next = match *sort {
                Some((k, SortDir::Asc)) if k == key => Some((key, SortDir::Desc)),
                Some((k, SortDir::Desc)) if k == key => None,
                _ => Some((key, SortDir::Asc)),
            };
            page.set(0);
            sort.set(next);
        })
    };

    // width: table-fixed レイアウトでの列幅 (None = 残り幅を使う)。
    // ソートで表示内容が入れ替わっても列幅が動かないようにする。
    let sortable_th = |label: &'static str, key: SortKey, width: Option<&'static str>| -> Html {
        let onclick = {
            let on_sort = on_sort.clone();
            Callback::from(move |_| on_sort.emit(key))
        };
        let indicator = match *sort {
            Some((k, SortDir::Asc)) if k == key => "▲",
            Some((k, SortDir::Desc)) if k == key => "▼",
            _ => "",
        };
        html! {
            <th class={classes!("cursor-pointer", "select-none", width)} {onclick}>
                { label }
                <span class="inline-block w-3 ml-0.5 text-primary">{ indicator }</span>
            </th>
        }
    };

    let prev = {
        let page = page.clone();
        Callback::from(move |_| {
            if *page > 0 {
                page.set(*page - 1);
            }
        })
    };
    let next = {
        let page = page.clone();
        Callback::from(move |_| {
            if *page + 1 < total_pages {
                page.set(*page + 1);
            }
        })
    };

    let category_buttons = props
        .data
        .categories
        .iter()
        .map(|cat| {
            let active = !filter.is_hidden(*cat);
            let onclick = {
                let apply_filter = apply_filter.clone();
                let filter = (*filter).clone();
                let cat = *cat;
                Callback::from(move |_| apply_filter.emit(filter.with_toggled(cat)))
            };
            html! {
                <button class={classes!(
                        "btn", "btn-xs", "mcpr-btn",
                        if active { "mcpr-btn-primary" } else { "mcpr-btn-secondary opacity-70" })}
                    {onclick}>
                    { cat.label() }
                </button>
            }
        })
        .collect::<Html>();

    let on_query = {
        let apply_filter = apply_filter.clone();
        let filter = (*filter).clone();
        Callback::from(move |e: InputEvent| {
            let input: HtmlInputElement = e.target_unchecked_into();
            apply_filter.emit(filter.with_query(input.value()));
        })
    };

    let on_remove = {
        let on_remove = props.on_remove.clone();
        let id = props.id;
        Callback::from(move |_| on_remove.emit(id))
    };
    let speed_display = speed_label(&props.speed_input);
    let speed_invalid = speed_display == "invalid";
    let on_edit_speed = {
        let on_edit_speed = props.on_edit_speed.clone();
        let id = props.id;
        let current = props.speed_input.clone();
        Callback::from(move |_: MouseEvent| on_edit_speed.emit((id, current.clone())))
    };

    // 全選択チェックボックス: 1 件でも未選択なら全選択、全選択済みなら全解除。
    let on_toggle_all = {
        let on_set_all = props.on_set_all.clone();
        let id = props.id;
        Callback::from(move |_: Event| on_set_all.emit((id, !all_selected)))
    };

    // ドラッグの終了 (マウスアップ)。アンカーを外せば以後の hover で範囲が伸びない。
    let on_mark_up = {
        let mark_anchor = mark_anchor.clone();
        Callback::from(move |_: MouseEvent| mark_anchor.set(None))
    };
    let on_clear_marks = {
        let mark_anchor = mark_anchor.clone();
        let marked = marked.clone();
        Callback::from(move |_: MouseEvent| {
            mark_anchor.set(None);
            marked.set(Arc::new(HashSet::new()));
        })
    };
    let on_mark_on = {
        let on_set_many = props.on_set_many.clone();
        let marked = marked.clone();
        let id = props.id;
        Callback::from(move |_: MouseEvent| {
            if !marked.is_empty() {
                on_set_many.emit((id, (*marked).clone(), true));
            }
        })
    };
    let on_mark_off = {
        let on_set_many = props.on_set_many.clone();
        let marked = marked.clone();
        let id = props.id;
        Callback::from(move |_: MouseEvent| {
            if !marked.is_empty() {
                on_set_many.emit((id, (*marked).clone(), false));
            }
        })
    };

    let rows = (0..page_orig.len())
        .map(|pi| {
            let orig = page_orig[pi];
            let row = &all[orig];
            let checked = props.selection.is_on(orig);
            let is_marked = marked.contains(&orig);
            let on_check = {
                let on_toggle = props.on_toggle.clone();
                let id = props.id;
                Callback::from(move |_: Event| on_toggle.emit((id, orig)))
            };
            // 行本体の押下でドラッグ範囲選択を開始 (その行だけを選択状態にする)。
            // 既定のテキスト選択は prevent_default で抑止する。
            let on_row_down = {
                let mark_anchor = mark_anchor.clone();
                let marked = marked.clone();
                Callback::from(move |e: MouseEvent| {
                    e.prevent_default();
                    mark_anchor.set(Some(pi));
                    marked.set(Arc::new(HashSet::from([orig])));
                })
            };
            // ドラッグ中に別の行へ入ったら、アンカーからその行までを選択し直す。
            // マウスアップを取りこぼした後の hover で誤って伸びないよう、主ボタンの
            // 保持を `buttons` で確認し、離れていればドラッグを終了する。
            let on_row_enter = {
                let mark_anchor = mark_anchor.clone();
                let marked = marked.clone();
                let page_orig = page_orig.clone();
                Callback::from(move |e: MouseEvent| {
                    let Some(anchor) = *mark_anchor else {
                        return;
                    };
                    if e.buttons() & 1 == 0 {
                        mark_anchor.set(None);
                        return;
                    }
                    let (lo, hi) = if anchor <= pi {
                        (anchor, pi)
                    } else {
                        (pi, anchor)
                    };
                    let set: HashSet<usize> = page_orig[lo..=hi].iter().copied().collect();
                    marked.set(Arc::new(set));
                })
            };
            let (event, state) = match &row.kind {
                RowKind::Packet { id, state } => (
                    html! { <code>{ format!("0x{id:02x}") }</code> },
                    html! { <span class="mcpr-badge">{ state_name(*state) }</span> },
                ),
                RowKind::Custom { name } => (
                    // truncate されても hover (title) で全名を確認できる
                    html! { <code title={name.clone()}>{ name.clone() }</code> },
                    html! { <span class="mcpr-muted">{ "—" }</span> },
                ),
            };
            html! {
                <tr class={classes!(
                        (!checked).then_some("is-excluded"),
                        is_marked.then_some("is-selected"),
                    )}
                    onmousedown={on_row_down}
                    onmouseenter={on_row_enter}>
                    // チェックボックス操作は行ドラッグと衝突するため、押下を行へ伝播させない。
                    <td class="mcpr-check-cell"
                        onmousedown={Callback::from(|e: MouseEvent| e.stop_propagation())}>
                        <input type="checkbox" class="checkbox checkbox-sm mcpr-row-check"
                            checked={checked} onchange={on_check}
                            aria-label="このパケットを書き出しに含める" />
                    </td>
                    <td>{ orig }</td>
                    <td>{ row.time_ms }</td>
                    <td class="truncate">{ event }</td>
                    <td>{ state }</td>
                    <td>{ row.size }</td>
                </tr>
            }
        })
        .collect::<Html>();

    html! {
        <>
            <section class="mcpr-panel">
                <div class="mcpr-panel-body">
                    <div class="mcpr-section-header">
                        <h2 class="mcpr-section-title">{ "Metadata" }</h2>
                        <button type="button"
                            class="btn btn-sm mcpr-btn mcpr-btn-danger"
                            title="このファイルを削除"
                            onclick={on_remove}>
                            { "Delete" }
                        </button>
                    </div>
                    <div class="mcpr-meta-grid">
                        <MetaRow label="File" value={props.data.filename.clone()} />
                        <MetaRow label="format" value={props.data.format.to_string()} />
                        <MetaRow label="mcversion" value={props.data.info.mc_version.clone()} />
                        <MetaRow label="protocol" value={props.data.info.protocol_version.to_string()} />
                        <MetaRow label="duration (ms)" value={props.data.info.duration_ms.to_string()} />
                        <MetaRow label="dataVersion" value={
                            props.data.info.data_version.map_or_else(|| "—".to_string(), |v| v.to_string())
                        } />
                        <div class="mcpr-meta-row mcpr-meta-speed-row">
                            <span class="mcpr-meta-label">{ "speed:" }</span>
                            <button type="button"
                                class={classes!(
                                    "mcpr-meta-speed-button",
                                    speed_invalid.then_some("is-invalid"),
                                )}
                                onclick={on_edit_speed}
                                title="Change Speed">
                                <span class="mcpr-meta-speed-value">{ speed_display }</span>
                                <span class="mcpr-meta-speed-action">{ "Change" }</span>
                            </button>
                        </div>
                        <MetaRow label="players" value={props.data.info.players.len().to_string()} />
                        <MetaRow label="events" value={total_all.to_string()} />
                    </div>
                </div>
            </section>

            <section class="mcpr-panel">
                <div class="mcpr-panel-body">
                    <div class="mcpr-section-header">
                        <h2 class="mcpr-section-title">
                            { "Events" }
                            <span class="mcpr-badge">
                                { if shown == total_all {
                                    total_all.to_string()
                                } else {
                                    format!("{shown} / {total_all}")
                                } }
                            </span>
                            if !all_selected {
                                <span class="mcpr-badge mcpr-badge-selected"
                                    title="書き出しに含めるパケット数">
                                    { format!("{selected_count} selected") }
                                </span>
                            }
                        </h2>
                        <div class="join">
                            <button class="btn btn-sm join-item mcpr-btn mcpr-btn-secondary" onclick={prev}
                                disabled={cur_page == 0}>{ "Prev" }</button>
                            <button class="btn btn-sm join-item no-animation pointer-events-none mcpr-btn mcpr-btn-secondary">
                                { format!("{} / {}", cur_page + 1, total_pages) }
                            </button>
                            <button class="btn btn-sm join-item mcpr-btn mcpr-btn-secondary" onclick={next}
                                disabled={cur_page + 1 >= total_pages}>{ "Next" }</button>
                        </div>
                    </div>
                    <div class="flex items-center flex-wrap gap-1">
                        { category_buttons }
                        <input type="text"
                            class="input input-bordered input-sm font-mono w-full sm:w-64 sm:ml-auto mcpr-form-input"
                            placeholder="filter: 0x2c / move_entities"
                            value={filter.query.clone()}
                            oninput={on_query} />
                    </div>
                    <div class="mcpr-bulk-bar">
                        <span class="mcpr-muted text-xs">
                            { if marked.is_empty() {
                                "行をドラッグして範囲選択 → まとめて On / Off".to_string()
                            } else {
                                format!("{} 行を選択中", marked.len())
                            } }
                        </span>
                        <div class="join">
                            <button type="button" class="btn btn-sm join-item mcpr-btn mcpr-btn-primary"
                                disabled={marked.is_empty()} onclick={on_mark_on}>
                                { "On" }
                            </button>
                            <button type="button" class="btn btn-sm join-item mcpr-btn mcpr-btn-secondary"
                                disabled={marked.is_empty()} onclick={on_mark_off}>
                                { "Off" }
                            </button>
                        </div>
                        <button type="button" class="btn btn-sm mcpr-btn mcpr-btn-secondary"
                            disabled={marked.is_empty()} onclick={on_clear_marks}>
                            { "選択解除" }
                        </button>
                    </div>
                    <div class={classes!("mcpr-table-wrap", mark_anchor.is_some().then_some("is-selecting"))}
                        onmouseup={on_mark_up}>
                        <table class="mcpr-table">
                            <thead>
                                <tr>
                                    <th class="mcpr-check-cell w-10">
                                        <input type="checkbox" ref={header_check_ref}
                                            class="checkbox checkbox-sm mcpr-row-check"
                                            checked={all_selected} onchange={on_toggle_all}
                                            aria-label="すべてのパケットを選択 / 解除" />
                                    </th>
                                    { sortable_th("#", SortKey::Index, Some("w-24")) }
                                    { sortable_th("time (ms)", SortKey::Time, Some("w-28")) }
                                    { sortable_th("event", SortKey::Event, None) }
                                    { sortable_th("state", SortKey::State, Some("w-28")) }
                                    { sortable_th("size", SortKey::Size, Some("w-24")) }
                                </tr>
                            </thead>
                            <tbody>{ rows }</tbody>
                        </table>
                    </div>
                </div>
            </section>
        </>
    }
}

#[derive(Properties, PartialEq)]
struct MetaRowProps {
    label: &'static str,
    value: String,
}

#[function_component]
fn MetaRow(props: &MetaRowProps) -> Html {
    html! {
        <div class="mcpr-meta-row">
            <span class="mcpr-meta-label">{ props.label }{ ":" }</span>
            <span class="font-mono text-base-content truncate">{ &props.value }</span>
        </div>
    }
}
