use std::{cmp::Ordering, io::Cursor, rc::Rc};

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
use web_sys::{DragEvent, Event, HtmlInputElement};
use yew::prelude::*;

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
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
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

#[derive(Clone, PartialEq)]
pub enum ViewState {
    Idle,
    Loading(String),
    // Loaded は他 variant より大きいので Box で包む。
    Loaded(Box<Loaded>),
    Error(String),
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

/// 論理イベント列を表示用の行へ読み出し、出現カテゴリも集計する。
fn collect_events<S: EventSource>(
    mut source: S,
) -> anyhow::Result<(ReplayInfo, Vec<EventRow>, Vec<Category>)> {
    let info = source.info().clone();
    let mut seen = 0u8;
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
                seen |= Category::of(&kind).bit();
                EventRow {
                    time_ms: time.as_millis(),
                    kind,
                    size,
                }
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let categories = Category::ORDER
        .iter()
        .copied()
        .filter(|c| seen & c.bit() != 0)
        .collect();
    Ok((info, rows, categories))
}

fn parse_replay(filename: String, bytes: Vec<u8>) -> anyhow::Result<Loaded> {
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

#[function_component]
pub fn App() -> Html {
    let state = use_state(|| ViewState::Idle);
    // 読み込み中の FileReader 保持用。借り換え時に古いものは drop される。
    let reader_slot = use_mut_ref(|| Option::<FileReader>::None);

    let theme = use_state(initial_theme);
    use_effect_with(*theme, |t| apply_theme(*t));

    let on_toggle_theme = {
        let theme = theme.clone();
        Callback::from(move |_: Event| theme.set(theme.toggled()))
    };

    let on_file = {
        let state = state.clone();
        let reader_slot = reader_slot.clone();
        Callback::from(move |file: web_sys::File| {
            let filename = file.name();
            state.set(ViewState::Loading(filename.clone()));
            let state = state.clone();
            let slot = reader_slot.clone();
            let task = read_as_bytes(&GlooFile::from(file), move |result| {
                *slot.borrow_mut() = None;
                match result {
                    Ok(bytes) => match parse_replay(filename, bytes) {
                        Ok(loaded) => state.set(ViewState::Loaded(Box::new(loaded))),
                        Err(e) => state.set(ViewState::Error(format!("parse error: {e}"))),
                    },
                    Err(e) => state.set(ViewState::Error(format!("read error: {e:?}"))),
                }
            });
            *reader_slot.borrow_mut() = Some(task);
        })
    };

    let on_input_change = {
        let on_file = on_file.clone();
        Callback::from(move |e: Event| {
            let input: HtmlInputElement = e.target_unchecked_into();
            if let Some(files) = input.files()
                && let Some(file) = files.get(0)
            {
                on_file.emit(file);
            }
        })
    };

    let on_drop_handler = {
        let on_file = on_file.clone();
        Callback::from(move |e: DragEvent| {
            e.prevent_default();
            if let Some(dt) = e.data_transfer()
                && let Some(files) = dt.files()
                && let Some(file) = files.get(0)
            {
                on_file.emit(file);
            }
        })
    };

    let on_dragover = Callback::from(|e: DragEvent| e.prevent_default());

    html! {
        <div class="min-h-screen bg-base-200 p-6">
            <div class="max-w-6xl mx-auto space-y-6">
                <header class="flex items-center justify-between">
                    <h1 class="text-2xl font-bold">{ "mcpr-ui" }</h1>
                    <div class="flex items-center gap-3">
                        <a class="link link-hover text-sm"
                            href="https://github.com/topi-banana/mcpr-editor"
                            target="_blank" rel="noreferrer">
                            { "github" }
                        </a>
                        <label class="swap swap-rotate btn btn-ghost btn-circle btn-sm"
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
                    </div>
                </header>

                <div class="card bg-base-100 shadow border-2 border-dashed border-base-300"
                    ondragover={on_dragover}
                    ondrop={on_drop_handler}>
                    <div class="card-body items-center text-center gap-3">
                        <p class="text-base-content/70">{ ".mcpr / Flashback (.zip) ファイルをドロップ、または" }</p>
                        <input type="file" accept=".mcpr,.zip"
                            class="file-input file-input-bordered w-full max-w-xs"
                            onchange={on_input_change} />
                    </div>
                </div>

                { match &*state {
                    ViewState::Idle => html!{},
                    ViewState::Loading(name) => html! {
                        <div class="alert">
                            <span class="loading loading-spinner loading-sm"></span>
                            <span>{ format!("{name} を読み込み中...") }</span>
                        </div>
                    },
                    ViewState::Error(msg) => html! {
                        <div class="alert alert-error">
                            <span>{ msg }</span>
                        </div>
                    },
                    ViewState::Loaded(loaded) => html! {
                        <LoadedView data={(**loaded).clone()} />
                    },
                } }
            </div>
        </div>
    }
}

#[derive(Properties, PartialEq)]
struct LoadedViewProps {
    data: Loaded,
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
                <span class="text-primary inline-block w-3 ml-0.5">{ indicator }</span>
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
                <button class={classes!("btn", "btn-xs",
                        if active { "btn-primary" } else { "btn-ghost opacity-60" })}
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

    let rows = (start..end)
        .map(|pos| {
            let orig = indices.as_ref().map_or(pos, |v| v[pos]);
            let row = &all[orig];
            let (event, state) = match &row.kind {
                RowKind::Packet { id, state } => (
                    html! { <code>{ format!("0x{id:02x}") }</code> },
                    html! { <span class="badge badge-ghost badge-sm">{ state_name(*state) }</span> },
                ),
                RowKind::Custom { name } => (
                    // truncate されても hover (title) で全名を確認できる
                    html! { <code title={name.clone()}>{ name.clone() }</code> },
                    html! { <span class="text-base-content/40">{ "—" }</span> },
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
            <div class="card bg-base-100 shadow">
                <div class="card-body">
                    <h2 class="card-title">{ "Metadata" }</h2>
                    <div class="grid grid-cols-1 sm:grid-cols-2 gap-y-1 gap-x-6 text-sm">
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
            </div>

            <div class="card bg-base-100 shadow">
                <div class="card-body">
                    <div class="flex items-center justify-between flex-wrap gap-2">
                        <h2 class="card-title">
                            { "Events" }
                            <span class="badge badge-ghost">
                                { if shown == total_all {
                                    total_all.to_string()
                                } else {
                                    format!("{shown} / {total_all}")
                                } }
                            </span>
                        </h2>
                        <div class="join">
                            <button class="btn btn-sm join-item" onclick={prev}
                                disabled={cur_page == 0}>{ "Prev" }</button>
                            <button class="btn btn-sm join-item no-animation pointer-events-none">
                                { format!("{} / {}", cur_page + 1, total_pages) }
                            </button>
                            <button class="btn btn-sm join-item" onclick={next}
                                disabled={cur_page + 1 >= total_pages}>{ "Next" }</button>
                        </div>
                    </div>
                    <div class="flex items-center flex-wrap gap-1">
                        { category_buttons }
                        <input type="text"
                            class="input input-bordered input-sm font-mono w-64 ml-auto"
                            placeholder="filter: 0x2c / move_entities"
                            value={filter.query.clone()}
                            oninput={on_query} />
                    </div>
                    <div class="overflow-x-auto">
                        <table class="table table-zebra table-sm table-fixed min-w-[42rem]">
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
            </div>
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
        <div>
            <span class="font-semibold text-base-content/70 mr-2">{ props.label }{ ":" }</span>
            <span class="font-mono">{ &props.value }</span>
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
