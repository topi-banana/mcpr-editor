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
                    <a class="link link-hover text-sm"
                        href="https://github.com/topi-banana/mcpr-editor"
                        target="_blank" rel="noreferrer">
                        { "github" }
                    </a>
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
