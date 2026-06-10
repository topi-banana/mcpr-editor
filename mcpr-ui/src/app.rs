use std::{io::Cursor, rc::Rc};

use gloo_file::{
    File as GlooFile,
    callbacks::{FileReader, read_as_bytes},
};
use mcpr_lib::{
    archive::zip::ZipArchiveReader,
    event::{Event as ReplayEvent, EventSource, ReplayFormat, ReplayInfo, State, detect_format},
    flashback::FlashbackReader,
    mcpr::ReplayReader,
};
use web_sys::{DragEvent, Event, HtmlInputElement};
use yew::prelude::*;

const PAGE_SIZE: usize = 200;

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

#[derive(Clone, PartialEq)]
pub struct Loaded {
    pub filename: String,
    pub format: &'static str,
    pub info: ReplayInfo,
    pub events: Rc<Vec<EventRow>>,
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

/// 論理イベント列を表示用の行へ読み出す。
fn collect_events<S: EventSource>(mut source: S) -> anyhow::Result<(ReplayInfo, Vec<EventRow>)> {
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
    Ok((info, rows))
}

fn parse_replay(bytes: Vec<u8>) -> anyhow::Result<(&'static str, ReplayInfo, Vec<EventRow>)> {
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
    let (info, rows) = collect_events(source)?;
    Ok((format.name(), info, rows))
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
                        Ok((format, info, events)) => {
                            state.set(ViewState::Loaded(Box::new(Loaded {
                                filename,
                                format,
                                info,
                                events: Rc::new(events),
                            })))
                        }
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

#[function_component]
fn LoadedView(props: &LoadedViewProps) -> Html {
    let page = use_state(|| 0usize);
    let total = props.data.events.len();
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

    let rows = props.data.events[start..end]
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let (event, state) = match &row.kind {
                RowKind::Packet { id, state } => (
                    html! { <code>{ format!("0x{id:02x}") }</code> },
                    html! { <span class="badge badge-ghost badge-sm">{ state_name(*state) }</span> },
                ),
                RowKind::Custom { name } => (
                    html! { <code>{ name.clone() }</code> },
                    html! { <span class="text-base-content/40">{ "—" }</span> },
                ),
            };
            html! {
                <tr>
                    <td>{ start + i }</td>
                    <td>{ row.time_ms }</td>
                    <td>{ event }</td>
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
                        <MetaRow label="events" value={total.to_string()} />
                    </div>
                </div>
            </div>

            <div class="card bg-base-100 shadow">
                <div class="card-body">
                    <div class="flex items-center justify-between flex-wrap gap-2">
                        <h2 class="card-title">{ "Events" }</h2>
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
                                    <th>{ "time (ms)" }</th>
                                    <th>{ "event" }</th>
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
