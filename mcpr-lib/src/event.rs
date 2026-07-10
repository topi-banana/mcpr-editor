//! フォーマット非依存の論理イベント層。
//!
//! ReplayMod (.mcpr) と Flashback (.zip) はどちらも本質的には
//! 「時刻付き clientbound パケットの列」であり、このモジュールは
//! その共通語彙 ([`Event`]) と読み書きの抽象 ([`EventSource`] /
//! [`EventSink`]) を提供する。
//!
//! フォーマット固有の物理表現（Flashback の `NextTick` による時間進行、
//! `LevelChunkCached` のチャンク外部化など）は各アダプタが吸収し、
//! この層には現れない。

use std::{collections::BTreeSet, fmt, str::FromStr};

use crate::{
    archive::ArchiveReader,
    protocol::{FINISH_CONFIGURATION_PACKET_ID, LOGIN_SUCCESS_PACKET_ID},
};

/// リプレイ内の時刻。ミリ秒で正規化して保持する。
///
/// Flashback は tick (1 tick = 50ms) で時間を表現するため、
/// 換算をこの型に閉じ込める。ms → tick は切り捨てで最大 49ms 落ちる。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Time {
    millis: u64,
}

impl Time {
    pub const MS_PER_TICK: u64 = 50;
    pub const ZERO: Time = Time { millis: 0 };

    pub fn from_millis(millis: u64) -> Self {
        Self { millis }
    }
    pub fn from_ticks(ticks: u64) -> Self {
        Self {
            millis: ticks * Self::MS_PER_TICK,
        }
    }
    pub fn as_millis(&self) -> u64 {
        self.millis
    }
    /// 切り捨てで tick に換算する。
    pub fn as_ticks(&self) -> u64 {
        self.millis / Self::MS_PER_TICK
    }
}

/// リプレイの再生速度倍率。
///
/// `2.0x` は 2 倍速なのでイベント時刻・duration は 1/2 になる。
/// `0.5x` は半速なのでイベント時刻・duration は 2 倍になる。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaybackSpeed {
    multiplier: f64,
}

impl PlaybackSpeed {
    pub const NORMAL: PlaybackSpeed = PlaybackSpeed { multiplier: 1.0 };

    pub fn new(multiplier: f64) -> anyhow::Result<Self> {
        anyhow::ensure!(
            multiplier.is_finite() && multiplier > 0.0,
            "speed must be a finite number greater than 0"
        );
        Ok(Self { multiplier })
    }

    pub fn multiplier(self) -> f64 {
        self.multiplier
    }

    pub fn scale_millis(self, millis: u64) -> u64 {
        let scaled = (millis as f64 / self.multiplier).round();
        if scaled <= 0.0 {
            0
        } else if scaled >= u64::MAX as f64 {
            u64::MAX
        } else {
            scaled as u64
        }
    }

    pub fn scale_time(self, time: Time) -> Time {
        Time::from_millis(self.scale_millis(time.as_millis()))
    }
}

impl Default for PlaybackSpeed {
    fn default() -> Self {
        Self::NORMAL
    }
}

impl fmt::Display for PlaybackSpeed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.multiplier)
    }
}

impl FromStr for PlaybackSpeed {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s.trim().parse()?)
    }
}

/// 接続フェーズ。
///
/// .mcpr ではストリーム内の位置（遷移パケットの前後）として暗黙に、
/// Flashback では action 種別 (`GamePacket` / `ConfigurationPacket`)
/// として明示的に表現されるものを、共通語彙として持ち上げたもの。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum State {
    Handshaking,
    Status,
    Login,
    Configuration,
    Play,
}

impl State {
    /// clientbound パケット `packet_id` を観測した後の次の state。
    ///
    /// 遷移 id は protocol 764 (1.20.2) 以降で安定している値
    /// ([`crate::protocol`] の定数)。それ以前のプロトコルを扱う場合は
    /// ここを protocol_version 依存にする。
    pub fn advance(self, packet_id: i32) -> State {
        match (self, packet_id) {
            (State::Login, LOGIN_SUCCESS_PACKET_ID) => State::Configuration,
            (State::Configuration, FINISH_CONFIGURATION_PACKET_ID) => State::Play,
            _ => self,
        }
    }
}

/// 複数リプレイ連結時、2 個目以降の入力から除外すべき接続初期化
/// イベントか (mcpr-cli / mcpr-ui 共通の連結規則)。
///
/// Play 以外の全パケット (Login/Configuration の初期化シーケンス) と、
/// クライアントを再 join させてしまう Login (play) パケット
/// ([`crate::protocol::LOGIN_PLAY_PACKET_ID`]) が該当する。
pub fn is_connection_init(state: State, id: i32) -> bool {
    state != State::Play || id == crate::protocol::LOGIN_PLAY_PACKET_ID
}

/// フォーマット非依存の論理イベント。
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// clientbound Minecraft パケット。
    ///
    /// `data` はパケット id を除いた body（`id` + `data` が
    /// Minecraft プロトコルのフレーム内容に一致する）。
    Packet {
        time: Time,
        state: State,
        id: i32,
        data: Box<[u8]>,
    },
    /// パケットに正規化できないフォーマット固有イベント。
    ///
    /// 例: Flashback の `flashback:action/move_entities`。
    /// 生バイトのまま保持し、同フォーマットへの書き戻しでは透過する。
    Custom {
        time: Time,
        name: String,
        data: Box<[u8]>,
    },
}

impl Event {
    pub fn time(&self) -> Time {
        match self {
            Event::Packet { time, .. } | Event::Custom { time, .. } => *time,
        }
    }
    pub fn time_mut(&mut self) -> &mut Time {
        match self {
            Event::Packet { time, .. } | Event::Custom { time, .. } => time,
        }
    }
}

/// フォーマット非依存のリプレイメタ情報（共通最小集合）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReplayInfo {
    pub mc_version: String,
    pub protocol_version: u32,
    pub duration_ms: u64,
    /// Flashback 由来の場合のみ判明する (mcpr メタデータには存在しない)。
    pub data_version: Option<u32>,
    /// mcpr 由来の場合のみ判明する。
    pub players: BTreeSet<uuid::Uuid>,
}

impl From<&crate::mcpr::MetaData> for ReplayInfo {
    fn from(m: &crate::mcpr::MetaData) -> Self {
        Self {
            mc_version: m.mcversion.clone(),
            protocol_version: m.protocol,
            duration_ms: m.duration,
            data_version: None,
            players: m.players.clone(),
        }
    }
}

impl From<&crate::flashback::MetaData> for ReplayInfo {
    fn from(m: &crate::flashback::MetaData) -> Self {
        Self {
            mc_version: m.version_string.clone(),
            protocol_version: m.protocol_version,
            duration_ms: Time::from_ticks(m.total_ticks).as_millis(),
            data_version: Some(m.data_version),
            players: BTreeSet::new(),
        }
    }
}

/// リプレイアーカイブの物理フォーマット。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayFormat {
    ReplayMod,
    Flashback,
}

impl ReplayFormat {
    pub fn name(&self) -> &'static str {
        match self {
            ReplayFormat::ReplayMod => "ReplayMod",
            ReplayFormat::Flashback => "Flashback",
        }
    }
}

/// アーカイブ内のメタデータファイル名でフォーマットを判別する。
/// 拡張子に依存しないため、.zip に固められた .mcpr 相当も正しく扱える。
pub fn detect_format<R: ArchiveReader + ?Sized>(archive: &mut R) -> anyhow::Result<ReplayFormat> {
    if archive.get_reader(crate::mcpr::METADATA_FILE).is_ok() {
        Ok(ReplayFormat::ReplayMod)
    } else if archive.get_reader(crate::flashback::METADATA_FILE).is_ok() {
        Ok(ReplayFormat::Flashback)
    } else {
        anyhow::bail!("metadata file not found: not a ReplayMod / Flashback archive")
    }
}

/// リプレイをイベント列として読み出す抽象。
pub trait EventSource {
    /// リプレイ全体のメタ情報。
    fn info(&self) -> &ReplayInfo;
    /// 次のイベント。終端で `Ok(None)`。
    fn next_event(&mut self) -> anyhow::Result<Option<Event>>;
    /// イベント列を Iterator として消費する。
    fn events(&mut self) -> Events<'_, Self>
    where
        Self: Sized,
    {
        Events { source: self }
    }
}

/// [`EventSource::events`] が返す Iterator ブリッジ。
pub struct Events<'a, S> {
    source: &'a mut S,
}

impl<S: EventSource> Iterator for Events<'_, S> {
    type Item = anyhow::Result<Event>;
    fn next(&mut self) -> Option<Self::Item> {
        self.source.next_event().transpose()
    }
}

/// リプレイをイベント列として書き込む抽象。
pub trait EventSink {
    fn push(&mut self, event: Event) -> anyhow::Result<()>;
    /// 終端処理（メタデータ書き込み・バッファのフラッシュ）。
    /// 2 回以上呼んではならない。
    fn finish(&mut self, info: &ReplayInfo) -> anyhow::Result<()>;
}

impl<T: ?Sized + EventSource> EventSource for Box<T> {
    fn info(&self) -> &ReplayInfo {
        (**self).info()
    }
    fn next_event(&mut self) -> anyhow::Result<Option<Event>> {
        (**self).next_event()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_conversion() {
        assert_eq!(Time::from_ticks(0).as_millis(), 0);
        assert_eq!(Time::from_ticks(20).as_millis(), 1000);
        assert_eq!(Time::from_millis(1000).as_ticks(), 20);
        // 切り捨て
        assert_eq!(Time::from_millis(49).as_ticks(), 0);
        assert_eq!(Time::from_millis(50).as_ticks(), 1);
        assert_eq!(Time::from_millis(99).as_ticks(), 1);
    }

    #[test]
    fn playback_speed_scales_millis_by_inverse_multiplier() {
        let fast = PlaybackSpeed::new(2.0).unwrap();
        let slow = PlaybackSpeed::new(0.5).unwrap();

        assert_eq!(fast.scale_millis(1000), 500);
        assert_eq!(slow.scale_millis(1000), 2000);
        assert_eq!(PlaybackSpeed::new(3.0).unwrap().scale_millis(1000), 333);
    }

    #[test]
    fn playback_speed_rejects_invalid_multiplier() {
        for speed in [0.0, -1.0, f64::INFINITY, f64::NAN] {
            assert!(PlaybackSpeed::new(speed).is_err());
        }
        assert!("0".parse::<PlaybackSpeed>().is_err());
        assert!("NaN".parse::<PlaybackSpeed>().is_err());
    }

    #[test]
    fn state_advance() {
        assert_eq!(State::Login.advance(0x02), State::Configuration);
        assert_eq!(State::Configuration.advance(0x03), State::Play);
        // 遷移 id 以外では変化しない
        assert_eq!(State::Login.advance(0x03), State::Login);
        assert_eq!(State::Configuration.advance(0x02), State::Configuration);
        assert_eq!(State::Play.advance(0x02), State::Play);
        assert_eq!(State::Play.advance(0x03), State::Play);
    }

    #[test]
    fn connection_init_predicate() {
        use crate::protocol::LOGIN_PLAY_PACKET_ID;
        // Play の通常パケットだけが連結 2 個目以降でも残る
        assert!(!is_connection_init(State::Play, 0x2c));
        assert!(is_connection_init(State::Play, LOGIN_PLAY_PACKET_ID));
        assert!(is_connection_init(State::Login, 0x02));
        assert!(is_connection_init(State::Configuration, 0x2c));
        assert!(is_connection_init(State::Handshaking, 0x00));
        assert!(is_connection_init(State::Status, 0x00));
    }
}
