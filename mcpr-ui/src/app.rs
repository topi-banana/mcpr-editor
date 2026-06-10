use std::{io::Cursor, rc::Rc};

use gloo_file::{
    File as GlooFile,
    callbacks::{FileReader, read_as_bytes},
};
use mcpr_lib::{
    archive::zip::ZipArchiveReader,
    mcpr::{MetaData, ReplayReader, State},
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

#[derive(Clone, PartialEq)]
pub struct PacketRow {
    pub index: usize,
    pub time: u32,
    pub id: i32,
    pub size: usize,
    pub state: &'static str,
}

#[derive(Clone, PartialEq)]
pub struct Loaded {
    pub filename: String,
    pub metadata: MetaData,
    pub packets: Rc<[PacketRow]>,
}

#[derive(Clone, PartialEq)]
pub enum ViewState {
    Idle,
    Loading(String),
    // Loaded は MetaData を持ち大きいので Box で包む。
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

fn parse_replay(bytes: Vec<u8>) -> anyhow::Result<(MetaData, Vec<PacketRow>)> {
    let zip = ZipArchiveReader::new(Cursor::new(bytes))?;
    let mut replay = ReplayReader::new(zip);
    let metadata = replay.read_metadata()?;
    let packets: Vec<PacketRow> = replay
        .get_packet_reader()?
        .enumerate()
        .map(|(i, (st, p))| PacketRow {
            index: i,
            time: p.time(),
            id: p.id(),
            size: p.data().len(),
            state: state_name(st),
        })
        .collect();
    Ok((metadata, packets))
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
                    Ok(bytes) => match parse_replay(bytes) {
                        Ok((metadata, packets)) => state.set(ViewState::Loaded(Box::new(Loaded {
                            filename,
                            metadata,
                            packets: Rc::from(packets),
                        }))),
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
                        <p class="text-base-content/70">{ ".mcpr ファイルをドロップ、または" }</p>
                        <input type="file" accept=".mcpr"
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

#[function_component]
fn LoadedView(props: &LoadedViewProps) -> Html {
    let page = use_state(|| 0usize);
    let total = props.data.packets.len();
    let total_pages = total.div_ceil(PAGE_SIZE).max(1);
    let cur_page = (*page).min(total_pages - 1);
    let start = cur_page * PAGE_SIZE;
    let end = (start + PAGE_SIZE).min(total);

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

    let rows = props.data.packets[start..end]
        .iter()
        .map(|p| {
            html! {
                <tr>
                    <td>{ p.index }</td>
                    <td>{ p.time }</td>
                    <td><code>{ format!("0x{:02x}", p.id) }</code></td>
                    <td><span class="badge badge-ghost badge-sm">{ p.state }</span></td>
                    <td>{ p.size }</td>
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
                        <MetaRow label="mcversion" value={props.data.metadata.mcversion.clone()} />
                        <MetaRow label="protocol" value={props.data.metadata.protocol.to_string()} />
                        <MetaRow label="duration (ms)" value={props.data.metadata.duration.to_string()} />
                        <MetaRow label="serverName" value={props.data.metadata.serverName.clone()} />
                        <MetaRow label="singleplayer" value={props.data.metadata.singleplayer.to_string()} />
                        <MetaRow label="players" value={props.data.metadata.players.len().to_string()} />
                        <MetaRow label="packets" value={total.to_string()} />
                    </div>
                </div>
            </div>

            <div class="card bg-base-100 shadow">
                <div class="card-body">
                    <div class="flex items-center justify-between flex-wrap gap-2">
                        <h2 class="card-title">{ "Packets" }</h2>
                        <div class="join">
                            <button class="btn btn-sm join-item" onclick={prev}
                                disabled={cur_page == 0}>{ "Prev" }</button>
                            <button class="btn btn-sm join-item no-animation pointer-events-none">
                                { format!("{} / {} (#{start}–#{})", cur_page + 1, total_pages, end.saturating_sub(1)) }
                            </button>
                            <button class="btn btn-sm join-item" onclick={next}
                                disabled={cur_page + 1 >= total_pages}>{ "Next" }</button>
                        </div>
                    </div>
                    <div class="overflow-x-auto">
                        <table class="table table-zebra table-sm">
                            <thead>
                                <tr>
                                    <th>{ "#" }</th>
                                    <th>{ "time" }</th>
                                    <th>{ "id" }</th>
                                    <th>{ "state" }</th>
                                    <th>{ "size" }</th>
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
