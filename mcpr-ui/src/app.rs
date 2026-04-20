use std::{fmt::Write as _, io::Cursor, rc::Rc};

use gloo_file::{
    File as GlooFile,
    callbacks::{FileReader, read_as_bytes},
};
use mcpr_lib::{
    archive::zip::ZipArchiveReader,
    mcpr::{MetaData, ReplayReader, State},
};
use web_sys::{DragEvent, Element, Event, HtmlInputElement, MouseEvent};
use yew::prelude::*;

const PAGE_SIZE: usize = 200;

// タイムライン描画用定数。viewBox 幅は固定 1000、高さはレーン数 × LANE_H。
const TIMELINE_VB_W: u32 = 1000;
const TIMELINE_LANE_H: u32 = 28;
const TIMELINE_TICK_PAD: u32 = 5;

// 状態ごとのレーン順序。存在するものだけを実際に描画する。
const STATE_ORDER: &[&str] = &["Handshaking", "Status", "Login", "Config", "Play"];

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

#[derive(Clone, Copy, PartialEq)]
enum ViewTab {
    Packets,
    Timeline,
}

#[function_component]
fn LoadedView(props: &LoadedViewProps) -> Html {
    let page = use_state(|| 0usize);
    let tab = use_state(|| ViewTab::Packets);
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

    let on_jump = {
        let page = page.clone();
        let tab = tab.clone();
        Callback::from(move |idx: usize| {
            page.set(idx / PAGE_SIZE);
            tab.set(ViewTab::Packets);
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

    let set_packets = {
        let tab = tab.clone();
        Callback::from(move |_| tab.set(ViewTab::Packets))
    };
    let set_timeline = {
        let tab = tab.clone();
        Callback::from(move |_| tab.set(ViewTab::Timeline))
    };
    let tab_packets_cls = if *tab == ViewTab::Packets {
        "tab tab-active"
    } else {
        "tab"
    };
    let tab_timeline_cls = if *tab == ViewTab::Timeline {
        "tab tab-active"
    } else {
        "tab"
    };

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

            <div role="tablist" class="tabs tabs-boxed">
                <a role="tab" class={tab_packets_cls} onclick={set_packets}>{ "Packets" }</a>
                <a role="tab" class={tab_timeline_cls} onclick={set_timeline}>{ "Timeline" }</a>
            </div>

            { match *tab {
                ViewTab::Packets => html! {
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
                },
                ViewTab::Timeline => html! {
                    <TimelineView
                        packets={props.data.packets.clone()}
                        duration={props.data.metadata.duration}
                        on_jump={on_jump} />
                },
            } }
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

fn state_color(s: &str) -> &'static str {
    match s {
        "Handshaking" => "#a78bfa",
        "Status" => "#fbbf24",
        "Login" => "#f87171",
        "Config" => "#34d399",
        "Play" => "#60a5fa",
        _ => "#9ca3af",
    }
}

fn fmt_ms(ms: u64) -> String {
    let total_s = ms / 1000;
    let mm = total_s / 60;
    let ss = total_s % 60;
    let rest = ms % 1000;
    format!("{mm:02}:{ss:02}.{rest:03}")
}

#[derive(Clone, PartialEq)]
struct LaneData {
    state: &'static str,
    color: &'static str,
    // 時間昇順。(time, original packet index)。
    ticks: Vec<(u32, usize)>,
    // SVG path `d` 属性。lane-local 座標 (y は 0..LANE_H)。
    path_d: String,
}

#[derive(Clone, PartialEq)]
struct TimelineData {
    duration_ms: u32,
    vb_h: u32,
    lanes: Vec<LaneData>,
}

fn build_timeline(packets: &[PacketRow], duration_ms: u64) -> TimelineData {
    let duration = duration_ms.max(1) as u32;
    // STATE_ORDER の順番を保ちつつ、実際に存在する state だけ lane 化する。
    let mut lanes: Vec<LaneData> = STATE_ORDER
        .iter()
        .map(|s| LaneData {
            state: s,
            color: state_color(s),
            ticks: Vec::new(),
            path_d: String::new(),
        })
        .collect();

    for p in packets.iter() {
        if let Some(lane) = lanes.iter_mut().find(|l| l.state == p.state) {
            lane.ticks.push((p.time, p.index));
        }
    }
    // 空レーンは除去。
    lanes.retain(|l| !l.ticks.is_empty());

    let y_top = TIMELINE_TICK_PAD;
    let y_bot = TIMELINE_LANE_H - TIMELINE_TICK_PAD;
    let vb_w = TIMELINE_VB_W as f64;
    let dur = duration as f64;
    for lane in lanes.iter_mut() {
        // packets は生成順＝時間順のはずだが保険で sort。
        lane.ticks.sort_by_key(|(t, _)| *t);
        let mut d = String::with_capacity(lane.ticks.len() * 20);
        for (t, _) in lane.ticks.iter() {
            let x = (*t as f64) / dur * vb_w;
            let _ = write!(&mut d, "M{x:.2} {y_top} L{x:.2} {y_bot} ");
        }
        lane.path_d = d;
    }

    let vb_h = (lanes.len() as u32) * TIMELINE_LANE_H;
    TimelineData {
        duration_ms: duration,
        vb_h,
        lanes,
    }
}

#[derive(Clone, Copy, PartialEq)]
struct HoverInfo {
    lane_idx: usize,
    x_svg: f64,
    tick_time: u32,
    packet_index: usize,
    // CSS ピクセル座標 (tooltip 配置用)。
    px_x: i32,
    px_y: i32,
}

#[derive(Properties, PartialEq)]
struct TimelineViewProps {
    packets: Rc<[PacketRow]>,
    duration: u64,
    on_jump: Callback<usize>,
}

#[function_component]
fn TimelineView(props: &TimelineViewProps) -> Html {
    let packets_len = props.packets.len();
    let duration = props.duration;
    let data = {
        let packets = props.packets.clone();
        use_memo((packets_len, duration), move |_| {
            build_timeline(&packets, duration)
        })
    };
    let hover = use_state(|| Option::<HoverInfo>::None);
    // Yew 0.21 はイベント委譲を使うため e.current_target() が配信先になる。
    // 実際のオーバーレイ要素の rect を得るには NodeRef 経由で参照する。
    let overlay_ref = use_node_ref();

    // mouse 座標 → (lane_idx, nearest packet) を解決する共通ロジック。
    // data, mouse の情報から HoverInfo を返す。該当なしなら None。
    let resolve = |e: &MouseEvent, data: &TimelineData, overlay: &NodeRef| -> Option<HoverInfo> {
        if data.lanes.is_empty() {
            return None;
        }
        let el = overlay.cast::<Element>()?;
        let rect = el.get_bounding_client_rect();
        let w = rect.width();
        let h = rect.height();
        if w <= 0.0 || h <= 0.0 {
            return None;
        }
        let ox = e.client_x() as f64 - rect.left();
        let oy = e.client_y() as f64 - rect.top();
        if ox < 0.0 || oy < 0.0 || ox > w || oy > h {
            return None;
        }
        let frac_x = ox / w;
        let frac_y = oy / h;
        let lane_count = data.lanes.len();
        let lane_idx = ((frac_y * lane_count as f64) as usize).min(lane_count - 1);
        let time = (frac_x * data.duration_ms as f64).round() as u32;
        let lane = &data.lanes[lane_idx];
        if lane.ticks.is_empty() {
            return None;
        }
        let pos = lane.ticks.binary_search_by_key(&time, |(t, _)| *t);
        let i = match pos {
            Ok(i) => i,
            Err(i) => {
                if i == 0 {
                    0
                } else if i >= lane.ticks.len() {
                    lane.ticks.len() - 1
                } else {
                    let (t0, _) = lane.ticks[i - 1];
                    let (t1, _) = lane.ticks[i];
                    if time.saturating_sub(t0) <= t1.saturating_sub(time) {
                        i - 1
                    } else {
                        i
                    }
                }
            }
        };
        let (tt, pi) = lane.ticks[i];
        Some(HoverInfo {
            lane_idx,
            x_svg: frac_x * TIMELINE_VB_W as f64,
            tick_time: tt,
            packet_index: pi,
            px_x: ox as i32,
            px_y: oy as i32,
        })
    };

    let onmousemove = {
        let hover = hover.clone();
        let data = data.clone();
        let overlay_ref = overlay_ref.clone();
        Callback::from(move |e: MouseEvent| {
            hover.set(resolve(&e, &data, &overlay_ref));
        })
    };
    let onmouseleave = {
        let hover = hover.clone();
        Callback::from(move |_| hover.set(None))
    };
    let onclick = {
        let data = data.clone();
        let on_jump = props.on_jump.clone();
        let overlay_ref = overlay_ref.clone();
        Callback::from(move |e: MouseEvent| {
            if let Some(info) = resolve(&e, &data, &overlay_ref) {
                on_jump.emit(info.packet_index);
            }
        })
    };

    if data.lanes.is_empty() {
        return html! {
            <div class="card bg-base-100 shadow">
                <div class="card-body">
                    <h2 class="card-title">{ "Timeline" }</h2>
                    <p class="text-base-content/70 text-sm">{ "パケットがありません。" }</p>
                </div>
            </div>
        };
    }

    let vb_h = data.vb_h;
    let viewbox = format!("0 0 {} {}", TIMELINE_VB_W, vb_h);
    let svg_height_px = vb_h as usize * 2; // 1 lane = 28 → 56px 程度確保。

    // レーンの背景 + パケット tick path を描画。
    let lane_els = data
        .lanes
        .iter()
        .enumerate()
        .flat_map(|(i, lane)| {
            let y = (i as u32) * TIMELINE_LANE_H;
            let bg_fill = if i % 2 == 0 { "#f3f4f6" } else { "#e5e7eb" };
            let bg = html! {
                <rect x="0" y={y.to_string()}
                    width={TIMELINE_VB_W.to_string()}
                    height={TIMELINE_LANE_H.to_string()}
                    fill={bg_fill} />
            };
            let path = html! {
                <g transform={format!("translate(0,{y})")}>
                    <path d={lane.path_d.clone()}
                        stroke={lane.color}
                        stroke-width="1"
                        vector-effect="non-scaling-stroke"
                        fill="none" />
                </g>
            };
            vec![bg, path]
        })
        .collect::<Html>();

    // 時刻目盛り (5 本)。
    let axis_ticks = (0..=4)
        .map(|i| {
            let frac = i as f64 / 4.0;
            let ms = (frac * data.duration_ms as f64) as u64;
            html! {
                <div class="text-xs font-mono text-base-content/60 whitespace-nowrap">
                    { fmt_ms(ms) }
                </div>
            }
        })
        .collect::<Html>();

    // レーンラベル (SVG の左に並べる)。
    let labels = data
        .lanes
        .iter()
        .map(|lane| {
            html! {
                <div class="flex items-center gap-2"
                    style={format!("height: {}px;", TIMELINE_LANE_H * 2)}>
                    <span class="inline-block w-3 h-3 rounded-sm"
                        style={format!("background-color: {}", lane.color)} />
                    <span class="text-xs font-mono">{ lane.state }</span>
                </div>
            }
        })
        .collect::<Html>();

    // ホバー時のカーソル線とツールチップ。
    let (cursor_line, tooltip) = if let Some(info) = *hover {
        let x = info.x_svg;
        let line = html! {
            <line x1={format!("{x:.2}")} x2={format!("{x:.2}")}
                y1="0" y2={vb_h.to_string()}
                stroke="#111827" stroke-width="1"
                vector-effect="non-scaling-stroke"
                pointer-events="none"
                stroke-dasharray="3,2" />
        };
        let lane_state = data.lanes.get(info.lane_idx).map(|l| l.state).unwrap_or("");
        let tip = html! {
            <div class="pointer-events-none absolute z-10 rounded bg-neutral text-neutral-content text-xs font-mono px-2 py-1 shadow"
                style={format!(
                    "left: {}px; top: {}px; transform: translate(-50%, -120%); white-space: nowrap;",
                    info.px_x, info.px_y
                )}>
                { format!("#{} / {} ms / {}", info.packet_index, info.tick_time, lane_state) }
            </div>
        };
        (line, tip)
    } else {
        (html! {}, html! {})
    };

    html! {
        <div class="card bg-base-100 shadow">
            <div class="card-body">
                <div class="flex items-center justify-between flex-wrap gap-2">
                    <h2 class="card-title">{ "Timeline" }</h2>
                    <span class="text-xs text-base-content/60">
                        { "クリックで該当パケットへジャンプ" }
                    </span>
                </div>

                <div class="flex gap-3">
                    <div class="flex flex-col shrink-0 pt-0">
                        { labels }
                    </div>

                    <div class="flex-1 min-w-0">
                        <div ref={overlay_ref}
                            class="relative"
                            onmousemove={onmousemove}
                            onmouseleave={onmouseleave}
                            onclick={onclick}>
                            <svg
                                viewBox={viewbox}
                                preserveAspectRatio="none"
                                style={format!("width: 100%; height: {svg_height_px}px; display: block; cursor: crosshair;")}>
                                { lane_els }
                                { cursor_line }
                            </svg>
                            { tooltip }
                        </div>
                        <div class="flex justify-between mt-1">
                            { axis_ticks }
                        </div>
                    </div>
                </div>
            </div>
        </div>
    }
}
