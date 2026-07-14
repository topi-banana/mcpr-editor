//! 表示用のイベント行モデルと、リプレイのパース。
//!
//! 論理イベント層の [`mcpr_lib::event::Event`] のうち、表示に必要な部分だけを
//! 保持する軽量な [`EventRow`] へ読み出す。パケット body は持たないため、
//! 書き出し時は元バイト列を再パースする (mcpr-app::export 側)。

use std::{io::Cursor, sync::Arc};

use mcpr_lib::{
    archive::zip::ZipArchiveReader,
    event::{Event as ReplayEvent, EventSource, ReplayFormat, ReplayInfo, State, detect_format},
    flashback::FlashbackReader,
    mcpr::ReplayReader,
};

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
    pub const ORDER: [Category; 6] = [
        Category::State(State::Handshaking),
        Category::State(State::Status),
        Category::State(State::Login),
        Category::State(State::Configuration),
        Category::State(State::Play),
        Category::Custom,
    ];

    pub fn of(kind: &RowKind) -> Category {
        match kind {
            RowKind::Packet { state, .. } => Category::State(*state),
            RowKind::Custom { .. } => Category::Custom,
        }
    }

    /// カテゴリビット集合 ([`crate::filter::EventFilter`] 等) 内のビット。
    pub fn bit(&self) -> u8 {
        match self {
            Category::State(State::Handshaking) => 1 << 0,
            Category::State(State::Status) => 1 << 1,
            Category::State(State::Login) => 1 << 2,
            Category::State(State::Configuration) => 1 << 3,
            Category::State(State::Play) => 1 << 4,
            Category::Custom => 1 << 5,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Category::State(s) => state_name(*s),
            Category::Custom => "Custom",
        }
    }
}

pub fn state_name(s: State) -> &'static str {
    match s {
        State::Handshaking => "Handshaking",
        State::Status => "Status",
        State::Login => "Login",
        State::Configuration => "Config",
        State::Play => "Play",
    }
}

/// パース後は不変のリプレイ表示データ。`events` の Arc は再描画判定の
/// ポインタ比較 (フロント側) と、バックグラウンド処理への move を兼ねる。
#[derive(Clone, PartialEq)]
pub struct Loaded {
    pub filename: String,
    pub format: &'static str,
    pub info: ReplayInfo,
    pub events: Arc<Vec<EventRow>>,
    /// リプレイ中に出現するカテゴリ (トグル UI 用、読み込み時に集計)。
    pub categories: Vec<Category>,
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

/// zip バイト列をフォーマット判定してパースし、表示データ [`Loaded`] を返す。
/// `Cursor` ベースなのでブラウザ (`Vec<u8>`) でも native (ファイル読み込み後の
/// `Vec<u8>`) でも同じコードで動く。
pub fn parse_replay(filename: String, bytes: &[u8]) -> anyhow::Result<Loaded> {
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
        events: Arc::new(rows),
        categories,
    })
}
