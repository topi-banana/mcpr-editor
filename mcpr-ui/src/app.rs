use std::{cmp::Ordering, collections::HashMap, io::Cursor, rc::Rc};

use gloo_file::{
    File as GlooFile,
    callbacks::{FileReader, read_as_bytes},
};
use mcpr_lib::{
    archive::zip::ZipArchiveReader,
    event::{Event as ReplayEvent, EventSource, ReplayFormat, ReplayInfo, State, detect_format},
    flashback::FlashbackReader,
    mcpr::ReplayReader,
    protocol::parse_packet_id,
};
use web_sys::{DragEvent, Event, HtmlDetailsElement, HtmlInputElement};
use yew::prelude::*;

use crate::export::{
    ExportFormat, ExportProgress, MergeInput, export_filename, export_merged, new_replay_uuid,
    trigger_download,
};

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

/// 表示行のイベント種別。論理イベント層の [`ReplayEvent`] のうち
/// 表示に必要な部分のみを保持する。
#[derive(Clone, PartialEq)]
pub enum RowKind {
    Packet { id: i32, state: State },
    Custom { name: String },
}

#[derive(Clone, PartialEq)]
pub struct EventRow {
    pub time_ms: u64,
    pub kind: RowKind,
    pub size: usize,
}

/// フィルタ対象の行カテゴリ。パケットは state ごと、Custom は一括。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    State(State),
    Custom,
}

impl Category {
    /// トグル UI の表示順 (接続フェーズ順 + Custom)。
    const ORDER: [Category; 6] = [
        Category::State(State::Handshaking),
        Category::State(State::Status),
        Category::State(State::Login),
        Category::State(State::Configuration),
        Category::State(State::Play),
        Category::Custom,
    ];

    fn of(kind: &RowKind) -> Category {
        match kind {
            RowKind::Packet { state, .. } => Category::State(*state),
            RowKind::Custom { .. } => Category::Custom,
        }
    }

    /// カテゴリビット集合 ([`EventFilter::hidden`] 等) 内のビット。
    fn bit(&self) -> u8 {
        match self {
            Category::State(State::Handshaking) => 1 << 0,
            Category::State(State::Status) => 1 << 1,
            Category::State(State::Login) => 1 << 2,
            Category::State(State::Configuration) => 1 << 3,
            Category::State(State::Play) => 1 << 4,
            Category::Custom => 1 << 5,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Category::State(s) => state_name(*s),
            Category::Custom => "Custom",
        }
    }
}

/// イベントテーブルの表示フィルタ。
/// (PartialEq は indices を導出する use_memo の依存キーとして使う)
#[derive(Clone, PartialEq, Default)]
struct EventFilter {
    /// 非表示カテゴリのビット集合 ([`Category::bit`]、0 = 全表示)。
    hidden: u8,
    /// イベント検索クエリ (入力欄の原文)。
    query: String,
    /// query の 16 進 packet id 解釈 (マッチ用キャッシュ)。
    query_id: Option<i32>,
    /// query の小文字化 (Custom 名マッチ用キャッシュ)。
    query_lower: String,
}

impl EventFilter {
    fn with_query(&self, query: String) -> Self {
        Self {
            hidden: self.hidden,
            query_id: parse_packet_id(&query),
            query_lower: query.trim().to_lowercase(),
            query,
        }
    }

    fn with_toggled(&self, category: Category) -> Self {
        Self {
            hidden: self.hidden ^ category.bit(),
            ..self.clone()
        }
    }

    fn is_hidden(&self, category: Category) -> bool {
        self.hidden & category.bit() != 0
    }

    fn is_empty(&self) -> bool {
        self.hidden == 0 && self.query_lower.is_empty()
    }

    /// クエリは「event 列の表示」へのマッチ:
    /// 16 進として解釈できれば packet id の一致、Custom は常に名前の部分一致。
    fn matches(&self, row: &EventRow) -> bool {
        if self.is_hidden(Category::of(&row.kind)) {
            return false;
        }
        if self.query_lower.is_empty() {
            return true;
        }
        match &row.kind {
            RowKind::Packet { id, .. } => self.query_id == Some(*id),
            RowKind::Custom { name } => name.contains(&self.query_lower),
        }
    }
}

/// ソート対象のカラム。
#[derive(Clone, Copy, PartialEq, Eq)]
enum SortKey {
    Index,
    Time,
    Event,
    State,
    Size,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SortDir {
    Asc,
    Desc,
}

/// event 列の全順序: packet (id 順) → custom (名前順)。
fn event_ord(a: &RowKind, b: &RowKind) -> Ordering {
    match (a, b) {
        (RowKind::Packet { id: a, .. }, RowKind::Packet { id: b, .. }) => a.cmp(b),
        (RowKind::Custom { name: a }, RowKind::Custom { name: b }) => a.cmp(b),
        (RowKind::Packet { .. }, RowKind::Custom { .. }) => Ordering::Less,
        (RowKind::Custom { .. }, RowKind::Packet { .. }) => Ordering::Greater,
    }
}

/// `indices` (元 index 昇順) をカラム値で並べ替える。
/// 安定ソートのため同値は記録順を保つ (Desc は全体の反転)。
fn sort_indices(events: &[EventRow], indices: &mut [usize], key: SortKey, dir: SortDir) {
    match key {
        SortKey::Index => {}
        SortKey::Time => indices.sort_by_key(|&i| events[i].time_ms),
        SortKey::Event => indices.sort_by(|&a, &b| event_ord(&events[a].kind, &events[b].kind)),
        SortKey::State => indices.sort_by_key(|&i| Category::of(&events[i].kind).bit()),
        SortKey::Size => indices.sort_by_key(|&i| events[i].size),
    }
    if dir == SortDir::Desc {
        indices.reverse();
    }
}

#[derive(Clone, PartialEq)]
pub struct Loaded {
    pub filename: String,
    pub format: &'static str,
    pub info: ReplayInfo,
    pub events: Rc<Vec<EventRow>>,
    /// リプレイ中に出現するカテゴリ (トグル UI 用、読み込み時に集計)。
    pub categories: Vec<Category>,
}

/// ファイルエントリの読み込み状態。
#[derive(Clone)]
enum EntryState {
    Loading,
    /// パース後は不変。Rc は merge の use_memo 依存キー (ポインタ比較) も兼ねる。
    /// `bytes` は元ファイルの生バイト列で、書き出し時の再パースに使う
    /// (表示行 [`EventRow`] はパケット body を持たないため)。
    Loaded {
        loaded: Rc<Loaded>,
        bytes: Rc<Vec<u8>>,
    },
    Error(String),
}

/// 連結リストの 1 エントリの中身。ファイルと interval を同列に並べる。
#[derive(Clone)]
enum EntryKind {
    File {
        filename: String,
        state: EntryState,
    },
    /// 連結時に直前までのタイムラインへ加算する空白。入力欄の原文を保持し、
    /// parse 失敗は 0 扱い (旧 interval 入力欄と同じ方針)。
    Interval {
        input: String,
    },
}

/// 連結リストの 1 エントリ。`id` は発番順の安定キー
/// (Yew の key、読み込み中 FileReader の管理、DnD の並べ替えに使う)。
#[derive(Clone)]
struct Entry {
    id: u64,
    kind: EntryKind,
}

/// ファイルと interval の順序付きリスト。
/// 複数の読み込み完了が並行して届くため、クロージャに古い状態を
/// キャプチャしない reducer ([`FilesAction`]) で更新する。
#[derive(Default)]
struct FilesState {
    entries: Vec<Entry>,
}

enum FilesAction {
    /// 読み込み開始 (Loading のファイルエントリを末尾へ追加)。
    Add {
        id: u64,
        filename: String,
    },
    /// 読み込み・パース完了。完了前に削除されていたら no-op。
    Finish {
        id: u64,
        result: Result<(Rc<Loaded>, Rc<Vec<u8>>), String>,
    },
    /// interval エントリを末尾へ追加。
    AddInterval {
        id: u64,
        input: String,
    },
    /// 既存 interval の値を差し替える (ダイアログ編集)。
    SetInterval {
        id: u64,
        input: String,
    },
    Remove {
        id: u64,
    },
    Reorder {
        dragged_id: u64,
        target_id: u64,
    },
}

impl Reducible for FilesState {
    type Action = FilesAction;

    fn reduce(self: Rc<Self>, action: FilesAction) -> Rc<Self> {
        // Entry の clone は Rc + String のみで軽量。
        let mut entries = self.entries.clone();
        match action {
            FilesAction::Add { id, filename } => entries.push(Entry {
                id,
                kind: EntryKind::File {
                    filename,
                    state: EntryState::Loading,
                },
            }),
            FilesAction::Finish { id, result } => {
                if let Some(EntryKind::File { state, .. }) =
                    entries.iter_mut().find(|e| e.id == id).map(|e| &mut e.kind)
                {
                    *state = match result {
                        Ok((loaded, bytes)) => EntryState::Loaded { loaded, bytes },
                        Err(msg) => EntryState::Error(msg),
                    };
                }
            }
            FilesAction::AddInterval { id, input } => entries.push(Entry {
                id,
                kind: EntryKind::Interval { input },
            }),
            FilesAction::SetInterval { id, input } => {
                if let Some(EntryKind::Interval { input: slot }) =
                    entries.iter_mut().find(|e| e.id == id).map(|e| &mut e.kind)
                {
                    *slot = input;
                }
            }
            FilesAction::Remove { id } => entries.retain(|e| e.id != id),
            FilesAction::Reorder {
                dragged_id,
                target_id,
            } => {
                if dragged_id != target_id
                    && let Some(from) = entries.iter().position(|e| e.id == dragged_id)
                    && let Some(to) = entries.iter().position(|e| e.id == target_id)
                {
                    let entry = entries.remove(from);
                    entries.insert(to.min(entries.len()), entry);
                }
            }
        }
        Rc::new(FilesState { entries })
    }
}

fn state_name(s: State) -> &'static str {
    match s {
        State::Handshaking => "Handshaking",
        State::Status => "Status",
        State::Login => "Login",
        State::Configuration => "Config",
        State::Play => "Play",
    }
}

/// 行列中に出現するカテゴリを表示順 ([`Category::ORDER`]) で集計する。
pub(crate) fn categories_of(rows: &[EventRow]) -> Vec<Category> {
    let mut seen = 0u8;
    for row in rows {
        seen |= Category::of(&row.kind).bit();
    }
    Category::ORDER
        .iter()
        .copied()
        .filter(|c| seen & c.bit() != 0)
        .collect()
}

/// 論理イベント列を表示用の行へ読み出し、出現カテゴリも集計する。
fn collect_events<S: EventSource>(
    mut source: S,
) -> anyhow::Result<(ReplayInfo, Vec<EventRow>, Vec<Category>)> {
    let info = source.info().clone();
    let rows = source
        .events()
        .map(|event| {
            event.map(|event| {
                let (time, kind, size) = match event {
                    ReplayEvent::Packet {
                        time,
                        state,
                        id,
                        data,
                    } => (time, RowKind::Packet { id, state }, data.len()),
                    ReplayEvent::Custom { time, name, data } => {
                        (time, RowKind::Custom { name }, data.len())
                    }
                };
                EventRow {
                    time_ms: time.as_millis(),
                    kind,
                    size,
                }
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let categories = categories_of(&rows);
    Ok((info, rows, categories))
}

fn parse_replay(filename: String, bytes: &[u8]) -> anyhow::Result<Loaded> {
    let mut zip = ZipArchiveReader::new(Cursor::new(bytes))?;
    let format = detect_format(&mut zip)?;
    // McprEventSource は reader を借用するため、match の外で生かす
    let mut mcpr_reader;
    let source: Box<dyn EventSource + '_> = match format {
        ReplayFormat::ReplayMod => {
            mcpr_reader = ReplayReader::new(zip);
            Box::new(mcpr_reader.event_source()?)
        }
        ReplayFormat::Flashback => Box::new(FlashbackReader::new(zip).event_source(true)?),
    };
    let (info, rows, categories) = collect_events(source)?;
    Ok(Loaded {
        filename,
        format: format.name(),
        info,
        events: Rc::new(rows),
        categories,
    })
}

fn file_list_to_vec(files: &web_sys::FileList) -> Vec<web_sys::File> {
    (0..files.length()).filter_map(|i| files.get(i)).collect()
}

/// 書き出しの進行状態 (進捗バー表示用)。None = 書き出し中でない。
#[derive(Clone, Copy, PartialEq)]
enum ExportPhase {
    /// zip 再パースなど、まだイベント処理件数を出せない開始直後。
    Preparing,
    /// イベント処理中。total はパース済み表示行数の合計
    /// (export は同じソースを再パースするため一致する)。
    Events { done: u64, total: u64 },
    /// zip 圧縮・メタデータ書き込み中 (一括で走るため進捗は刻めない)。
    Finishing,
}

/// interval 入力ダイアログの開閉と対象。`Add` は新規追加、`Edit(id)` は
/// 既存 interval の値編集 (位置はそのまま)。
#[derive(Clone, Copy, PartialEq)]
enum IntervalDialog {
    Closed,
    Add,
    Edit(u64),
}

/// 書き出し用に並び順のまま組む所有エントリ。借用版 [`MergeInput`] と違い、
/// Replay は Rc を保持して async タスクへ move できる。
enum OwnedItem {
    Replay(Rc<Vec<u8>>),
    Interval(u64),
}

#[function_component]
pub fn App() -> Html {
    let files = use_reducer(FilesState::default);
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
                            let bytes = Rc::new(bytes);
                            parse_replay(filename, &bytes)
                                .map(|loaded| (Rc::new(loaded), bytes))
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

    // Loaded なファイルだけを選択対象にする (interval は対象外)。
    let find_loaded = |want: Option<u64>| {
        files.entries.iter().find_map(|entry| match &entry.kind {
            EntryKind::File {
                state: EntryState::Loaded { loaded, .. },
                ..
            } if want.is_none_or(|id| entry.id == id) => Some((entry.id, loaded.clone())),
            _ => None,
        })
    };
    let selected_file = (*selected_file_id)
        .and_then(|selected_id| find_loaded(Some(selected_id)))
        .or_else(|| find_loaded(None));
    let active_file_id = selected_file.as_ref().map(|(id, _)| *id);

    let on_remove_file = {
        let dispatch = files.dispatcher();
        let readers = readers.clone();
        let selected_file_id = selected_file_id.clone();
        Callback::from(move |id: u64| {
            // 読み込み中なら FileReader の drop で中断される。
            readers.borrow_mut().remove(&id);
            if *selected_file_id == Some(id) {
                selected_file_id.set(None);
            }
            dispatch.dispatch(FilesAction::Remove { id });
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
                EntryKind::File { filename, state } => {
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
    let file_count = files
        .entries
        .iter()
        .filter(|e| matches!(e.kind, EntryKind::File { .. }))
        .count();
    let all_loaded = file_count > 0
        && files.entries.iter().all(|e| match &e.kind {
            EntryKind::File { state, .. } => matches!(state, EntryState::Loaded { .. }),
            EntryKind::Interval { .. } => true,
        });

    let on_export = {
        let files = files.clone();
        let export_phase = export_phase.clone();
        let export_error = export_error.clone();
        let format = *export_format;
        Callback::from(move |_: MouseEvent| {
            if export_phase.is_some() {
                return;
            }
            // 並び順のまま所有列を組む。Replay は Rc を持って async ブロックへ move し、
            // 借用ビュー (&[MergeInput]) は move 後にブロック内で作る (借用がダングらない)。
            let mut owned: Vec<OwnedItem> = Vec::new();
            let mut total = 0u64;
            let mut first_filename: Option<String> = None;
            let mut file_count = 0usize;
            for entry in &files.entries {
                match &entry.kind {
                    EntryKind::File {
                        filename,
                        state: EntryState::Loaded { loaded, bytes },
                    } => {
                        total += loaded.events.len() as u64;
                        first_filename.get_or_insert_with(|| filename.clone());
                        file_count += 1;
                        owned.push(OwnedItem::Replay(bytes.clone()));
                    }
                    // ボタンの disabled と同条件の保険 (全ファイル Loaded のときだけ)。
                    EntryKind::File { .. } => return,
                    EntryKind::Interval { input } => {
                        owned.push(OwnedItem::Interval(input.trim().parse().unwrap_or(0)));
                    }
                }
            }
            let Some(first_filename) = first_filename else {
                return;
            };
            let filename = export_filename(&first_filename, file_count > 1, format);
            export_phase.set(Some(ExportPhase::Preparing));
            export_error.set(None);
            let export_phase = export_phase.clone();
            let export_error = export_error.clone();
            // export_merged は一定イベント数ごとにブラウザへ yield するため、
            // async タスクとして流せば書き出し中も進捗バーが再描画される。
            yew::platform::spawn_local(async move {
                let items: Vec<MergeInput> = owned
                    .iter()
                    .map(|it| match it {
                        OwnedItem::Replay(b) => MergeInput::Replay(b.as_slice()),
                        OwnedItem::Interval(ms) => MergeInput::Interval(*ms),
                    })
                    .collect();
                let on_progress = {
                    let export_phase = export_phase.clone();
                    move |progress| {
                        export_phase.set(Some(match progress {
                            ExportProgress::Events { processed } => ExportPhase::Events {
                                done: processed,
                                total,
                            },
                            ExportProgress::Finishing => ExportPhase::Finishing,
                        }));
                    }
                };
                let result =
                    export_merged(&items, format, new_replay_uuid(), on_progress).await;
                match result {
                    Ok(bytes) => trigger_download(&bytes, &filename),
                    Err(e) => export_error.set(Some(format!("export error: {e}"))),
                }
                export_phase.set(None);
            });
        })
    };

    let format_buttons = ExportFormat::ORDER
        .iter()
        .map(|&f| {
            let export_format = export_format.clone();
            let class = if *export_format == f {
                "btn btn-sm join-item mcpr-btn mcpr-btn-primary"
            } else {
                "btn btn-sm join-item mcpr-btn mcpr-btn-secondary"
            };
            html! {
                <button key={f.extension()} class={class}
                    onclick={Callback::from(move |_| export_format.set(f))}>
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
                let clamped = if total == 0 { 0 } else { done.min(total) };
                let percent = clamped
                    .saturating_mul(100)
                    .checked_div(total)
                    .unwrap_or(if done == 0 { 0 } else { 100 });
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
            <div class="join">{ format_buttons }</div>
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
                            if let Some((id, data)) = selected_file.as_ref() {
                                <LoadedView key={id.to_string()}
                                    id={*id}
                                    data={data.clone()}
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
            </main>
        </div>
    }
}

#[derive(Properties)]
struct LoadedViewProps {
    id: u64,
    data: Rc<Loaded>,
    on_remove: Callback<u64>,
}

/// events の深い比較 (数百万行になり得る) を避け、merge 結果の
/// ポインタ同一性だけで再描画を判定する。
impl PartialEq for LoadedViewProps {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && Rc::ptr_eq(&self.data, &other.data)
    }
}

/// use_memo の依存キー用に Rc をポインタ同一性で比較するラッパ。
/// (events の深い比較は数百万行に及ぶため避ける)
struct RcPtr<T>(Rc<T>);

impl<T> PartialEq for RcPtr<T> {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

#[function_component]
fn LoadedView(props: &LoadedViewProps) -> Html {
    let page = use_state(|| 0usize);
    let filter = use_state(EventFilter::default);
    let sort = use_state(|| Option::<(SortKey, SortDir)>::None);

    // 表示する行の元 index 列 (None = 全行を記録順のまま)。
    // filter / sort / events が変わった時だけ全行を走査する。
    let indices = use_memo(
        (RcPtr(props.data.events.clone()), (*filter).clone(), *sort),
        |(events, filter, sort)| {
            if filter.is_empty() && sort.is_none() {
                return None;
            }
            let events = &events.0;
            let mut matched: Vec<usize> = if filter.is_empty() {
                (0..events.len()).collect()
            } else {
                let mut v = Vec::with_capacity(events.len());
                v.extend(
                    events
                        .iter()
                        .enumerate()
                        .filter(|(_, row)| filter.matches(row))
                        .map(|(i, _)| i),
                );
                v.shrink_to_fit();
                v
            };
            if let Some((key, dir)) = *sort {
                sort_indices(events, &mut matched, key, dir);
            }
            Some(matched)
        },
    );
    let indices: &Option<Vec<usize>> = &indices;

    let all = &props.data.events;
    let total_all = all.len();
    let shown = indices.as_ref().map_or(total_all, Vec::len);
    let total_pages = shown.div_ceil(PAGE_SIZE).max(1);
    let cur_page = (*page).min(total_pages - 1);
    let start = cur_page * PAGE_SIZE;
    let end = (start + PAGE_SIZE).min(shown);

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

    let rows = (start..end)
        .map(|pos| {
            let orig = indices.as_ref().map_or(pos, |v| v[pos]);
            let row = &all[orig];
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
                <tr>
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
                    <div class="mcpr-table-wrap">
                        <table class="mcpr-table">
                            <thead>
                                <tr>
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

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(id: i32, state: State) -> EventRow {
        EventRow {
            time_ms: 0,
            kind: RowKind::Packet { id, state },
            size: 0,
        }
    }

    fn custom(name: &str) -> EventRow {
        EventRow {
            time_ms: 0,
            kind: RowKind::Custom {
                name: name.to_string(),
            },
            size: 0,
        }
    }

    fn loading_file(id: u64) -> Entry {
        Entry {
            id,
            kind: EntryKind::File {
                filename: format!("{id}.mcpr"),
                state: EntryState::Loading,
            },
        }
    }

    fn interval_entry(id: u64, input: &str) -> Entry {
        Entry {
            id,
            kind: EntryKind::Interval {
                input: input.to_string(),
            },
        }
    }

    fn entry_ids(state: &FilesState) -> Vec<u64> {
        state.entries.iter().map(|entry| entry.id).collect()
    }

    #[test]
    fn file_reorder_moves_dragged_entry_to_drop_target_boundary() {
        let state = Rc::new(FilesState {
            entries: vec![loading_file(1), loading_file(2), loading_file(3)],
        });

        let state = state.reduce(FilesAction::Reorder {
            dragged_id: 1,
            target_id: 3,
        });
        assert_eq!(entry_ids(&state), vec![2, 3, 1]);

        let state = state.reduce(FilesAction::Reorder {
            dragged_id: 1,
            target_id: 2,
        });
        assert_eq!(entry_ids(&state), vec![1, 2, 3]);
    }

    #[test]
    fn interval_reorders_among_files_like_a_file() {
        // ファイル・ファイル・interval を並べ、interval を id ベースで先頭へ。
        let state = Rc::new(FilesState {
            entries: vec![loading_file(1), loading_file(2), interval_entry(3, "500")],
        });

        let state = state.reduce(FilesAction::Reorder {
            dragged_id: 3,
            target_id: 1,
        });
        assert_eq!(entry_ids(&state), vec![3, 1, 2]);
    }

    #[test]
    fn set_interval_updates_only_matching_interval() {
        let state = Rc::new(FilesState {
            entries: vec![loading_file(1), interval_entry(2, "500")],
        });

        let state = state.reduce(FilesAction::SetInterval {
            id: 2,
            input: "1200".to_string(),
        });

        let inputs: Vec<Option<&str>> = state
            .entries
            .iter()
            .map(|e| match &e.kind {
                EntryKind::Interval { input } => Some(input.as_str()),
                EntryKind::File { .. } => None,
            })
            .collect();
        assert_eq!(inputs, vec![None, Some("1200")]);
    }

    #[test]
    fn default_filter_shows_everything() {
        let f = EventFilter::default();
        assert!(f.is_empty());
        assert!(f.matches(&packet(0x2c, State::Play)));
        assert!(f.matches(&custom("flashback:action/move_entities")));
    }

    #[test]
    fn hidden_category_drops_rows() {
        let f = EventFilter::default().with_toggled(Category::State(State::Play));
        assert!(!f.matches(&packet(0x2c, State::Play)));
        assert!(f.matches(&packet(0x07, State::Configuration)));
        assert!(f.matches(&custom("flashback:action/move_entities")));
        // 再トグルで元に戻る
        let f = f.with_toggled(Category::State(State::Play));
        assert!(f.is_empty());
    }

    #[test]
    fn hex_query_matches_packet_id() {
        for q in ["0x2c", "2c", " 0x2C "] {
            let f = EventFilter::default().with_query(q.to_string());
            assert!(f.matches(&packet(0x2c, State::Play)), "query {q:?}");
            assert!(!f.matches(&packet(0x2b, State::Play)), "query {q:?}");
        }
    }

    #[test]
    fn text_query_matches_custom_name_case_insensitive() {
        for q in ["move", "MOVE"] {
            let f = EventFilter::default().with_query(q.to_string());
            assert!(f.matches(&custom("flashback:action/move_entities")));
            assert!(!f.matches(&custom("flashback:action/next_tick")));
            // 16 進として解釈できないクエリはパケットに一致しない
            assert!(!f.matches(&packet(0x2c, State::Play)));
        }
    }

    #[test]
    fn query_and_category_combine() {
        let f = EventFilter::default()
            .with_toggled(Category::State(State::Play))
            .with_query("0x07".to_string());
        // クエリは一致するがカテゴリが非表示
        assert!(!f.matches(&packet(0x07, State::Play)));
        assert!(f.matches(&packet(0x07, State::Configuration)));
    }

    fn sorted(events: &[EventRow], key: SortKey, dir: SortDir) -> Vec<usize> {
        let mut indices: Vec<usize> = (0..events.len()).collect();
        sort_indices(events, &mut indices, key, dir);
        indices
    }

    #[test]
    fn sort_by_time_is_stable() {
        let mut events = vec![
            packet(0x01, State::Play),
            packet(0x02, State::Play),
            packet(0x03, State::Play),
        ];
        events[0].time_ms = 100;
        // index 1, 2 は同時刻 → 記録順を保つ
        events[1].time_ms = 50;
        events[2].time_ms = 50;
        assert_eq!(sorted(&events, SortKey::Time, SortDir::Asc), vec![1, 2, 0]);
        assert_eq!(sorted(&events, SortKey::Time, SortDir::Desc), vec![0, 2, 1]);
    }

    #[test]
    fn sort_by_event_orders_packets_before_customs() {
        let events = vec![
            custom("flashback:action/move_entities"),
            packet(0x2c, State::Play),
            custom("flashback:action/accurate_player_position"),
            packet(0x07, State::Configuration),
        ];
        // packet (id 順) → custom (名前順)
        assert_eq!(
            sorted(&events, SortKey::Event, SortDir::Asc),
            vec![3, 1, 2, 0]
        );
    }

    #[test]
    fn sort_by_state_follows_phase_order() {
        let events = vec![
            custom("flashback:action/move_entities"),
            packet(0x2c, State::Play),
            packet(0x02, State::Login),
            packet(0x07, State::Configuration),
        ];
        // Login → Config → Play → Custom
        assert_eq!(
            sorted(&events, SortKey::State, SortDir::Asc),
            vec![2, 3, 1, 0]
        );
    }

    #[test]
    fn sort_by_index_desc_reverses() {
        let events = vec![packet(0x01, State::Play), packet(0x02, State::Play)];
        assert_eq!(sorted(&events, SortKey::Index, SortDir::Asc), vec![0, 1]);
        assert_eq!(sorted(&events, SortKey::Index, SortDir::Desc), vec![1, 0]);
    }
}
