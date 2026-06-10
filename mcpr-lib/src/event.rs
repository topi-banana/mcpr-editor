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

use std::collections::HashSet;

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
    /// 遷移 id (Login Success = 0x02, Finish Configuration = 0x03) は
    /// protocol 764 (1.20.2) 以降で安定している値。それ以前の
    /// プロトコルを扱う場合はここを protocol_version 依存にする。
    pub fn advance(self, packet_id: i32) -> State {
        match (self, packet_id) {
            (State::Login, 0x02) => State::Configuration,
            (State::Configuration, 0x03) => State::Play,
            _ => self,
        }
    }
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
    pub players: HashSet<uuid::Uuid>,
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
            players: HashSet::new(),
        }
    }
}

/// リプレイをイベント列として読み出す抽象。
pub trait EventSource {
    /// リプレイ全体のメタ情報。
    fn info(&self) -> &ReplayInfo;
    /// 次のイベント。終端で `Ok(None)`。
    fn next_event(&mut self) -> anyhow::Result<Option<Event>>;
}

/// リプレイをイベント列として書き込む抽象。
pub trait EventSink {
    fn push(&mut self, event: Event) -> anyhow::Result<()>;
    /// 終端処理（メタデータ書き込み・バッファのフラッシュ）。
    /// 2 回以上呼んではならない。
    fn finish(&mut self, info: &ReplayInfo) -> anyhow::Result<()>;
}

impl<T: ?Sized + EventSource> EventSource for &mut T {
    fn info(&self) -> &ReplayInfo {
        (**self).info()
    }
    fn next_event(&mut self) -> anyhow::Result<Option<Event>> {
        (**self).next_event()
    }
}

impl<T: ?Sized + EventSource> EventSource for Box<T> {
    fn info(&self) -> &ReplayInfo {
        (**self).info()
    }
    fn next_event(&mut self) -> anyhow::Result<Option<Event>> {
        (**self).next_event()
    }
}

impl<T: ?Sized + EventSink> EventSink for &mut T {
    fn push(&mut self, event: Event) -> anyhow::Result<()> {
        (**self).push(event)
    }
    fn finish(&mut self, info: &ReplayInfo) -> anyhow::Result<()> {
        (**self).finish(info)
    }
}

impl<T: ?Sized + EventSink> EventSink for Box<T> {
    fn push(&mut self, event: Event) -> anyhow::Result<()> {
        (**self).push(event)
    }
    fn finish(&mut self, info: &ReplayInfo) -> anyhow::Result<()> {
        (**self).finish(info)
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
    fn state_advance() {
        assert_eq!(State::Login.advance(0x02), State::Configuration);
        assert_eq!(State::Configuration.advance(0x03), State::Play);
        // 遷移 id 以外では変化しない
        assert_eq!(State::Login.advance(0x03), State::Login);
        assert_eq!(State::Configuration.advance(0x02), State::Configuration);
        assert_eq!(State::Play.advance(0x02), State::Play);
        assert_eq!(State::Play.advance(0x03), State::Play);
    }
}
