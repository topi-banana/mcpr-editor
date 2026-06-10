use std::{
    collections::HashSet,
    io::{self, BufReader, BufWriter, Cursor, Read, Write},
};

use serde::{Deserialize, Serialize};

use crate::{
    archive::{ArchiveReader, ArchiveWriter},
    event::{Event, EventSink, EventSource, ReplayInfo, Time},
    protocol::{
        Deserializer, FINISH_CONFIGURATION_PACKET_ID, LOGIN_SUCCESS_PACKET_ID, Serializer,
        login_success_payload,
    },
};

// 後方互換: State は共通語彙として crate::event に移動した。
pub use crate::event::State;

#[derive(Debug, Clone, PartialEq, PartialOrd)]
pub struct Packet {
    time: u32,
    id: i32,
    data: Box<[u8]>,
}

impl Packet {
    pub fn new(time: u32, id: i32, data: Box<[u8]>) -> Self {
        Self { time, id, data }
    }
    pub fn time(&self) -> u32 {
        self.time
    }
    pub fn time_mut(&mut self) -> &mut u32 {
        &mut self.time
    }
    pub fn id(&self) -> i32 {
        self.id
    }
    pub fn data(&self) -> &[u8] {
        &self.data
    }
    pub fn into_parts(self) -> (u32, i32, Box<[u8]>) {
        (self.time, self.id, self.data)
    }
    pub fn length(&self) -> io::Result<u32> {
        let mut p = Vec::new();
        Cursor::new(&mut p).write_varint(self.id)?;
        Ok(p.len() as u32 + self.data.len() as u32)
    }
    /// from .tmcpr
    pub fn read_from<R: Read>(reader: &mut R) -> io::Result<Option<Self>> {
        let mut header = [0u8; 8];
        match reader.read_exact(&mut header) {
            Ok(()) => {
                let time = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
                let length = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
                let mut data = vec![0u8; length as usize];
                reader.read_exact(&mut data)?;
                let mut cur = Cursor::new(data);
                let packet_id = cur.read_varint()?;
                let mut packet_data = Vec::new();
                cur.read_to_end(&mut packet_data)?;
                Ok(Some(Packet::new(
                    time,
                    packet_id,
                    packet_data.into_boxed_slice(),
                )))
            }
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
            Err(e) => Err(e),
        }
    }
    /// to .tmcpr
    pub fn write_to<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        writer.write_all(&self.time.to_be_bytes())?;
        writer.write_all(&self.length()?.to_be_bytes())?;
        writer.write_varint(self.id)?;
        writer.write_all(&self.data)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(non_snake_case)]
pub struct MetaData {
    pub singleplayer: bool,
    pub serverName: String,
    pub customServerName: String,
    pub duration: u64,
    pub date: u64,
    pub mcversion: String,
    pub fileFormat: String,
    pub fileFormatVersion: u32,
    pub protocol: u32,
    pub generator: String,
    pub selfId: i32,
    pub players: HashSet<uuid::Uuid>,
}

impl Default for MetaData {
    fn default() -> Self {
        Self {
            singleplayer: false,
            serverName: String::new(),
            customServerName: String::new(),
            duration: 0,
            date: 0,
            mcversion: String::new(),
            fileFormat: String::new(),
            fileFormatVersion: 0,
            protocol: 0,
            generator: String::new(),
            selfId: -1,
            players: HashSet::new(),
        }
    }
}

pub struct ReadablePacketStream<R> {
    state: State,
    reader: R,
}
impl<R> ReadablePacketStream<R> {
    pub fn new(state: State, reader: R) -> Self {
        Self { state, reader }
    }
}
impl<R: Read> Iterator for ReadablePacketStream<R> {
    type Item = (State, Packet);
    fn next(&mut self) -> Option<Self::Item> {
        Packet::read_from(&mut self.reader)
            .unwrap_or_default()
            .map(|packet| {
                let old_state = self.state;
                self.state = old_state.advance(packet.id());
                (old_state, packet)
            })
    }
}
pub struct WritablePacketStream<W> {
    writer: W,
}
impl<W> WritablePacketStream<W> {
    fn new(writer: W) -> Self {
        Self { writer }
    }
}
impl<W: Write> WritablePacketStream<W> {
    pub fn push(&mut self, packet: Packet) -> Result<(), io::Error> {
        packet.write_to(&mut self.writer)
    }
}

/// .mcpr の tmcpr ストリームを論理イベント列として読み出すアダプタ。
///
/// [`ReadablePacketStream`] と異なり読み取りエラーを EOF と区別して
/// 伝播する。state 遷移 (Login → Configuration → Play) を追跡し、
/// 各パケットに観測時点の state を付与する。
pub struct McprEventSource<R> {
    reader: R,
    state: State,
    info: ReplayInfo,
}

impl<R: Read> McprEventSource<R> {
    pub fn new(reader: R, info: ReplayInfo) -> Self {
        Self {
            reader,
            state: State::Login,
            info,
        }
    }
}

impl<R: Read> EventSource for McprEventSource<R> {
    fn info(&self) -> &ReplayInfo {
        &self.info
    }
    fn next_event(&mut self) -> anyhow::Result<Option<Event>> {
        let Some(packet) = Packet::read_from(&mut self.reader)? else {
            return Ok(None);
        };
        let state = self.state;
        self.state = state.advance(packet.id());
        let (time, id, data) = packet.into_parts();
        Ok(Some(Event::Packet {
            time: Time::from_millis(time as u64),
            state,
            id,
            data,
        }))
    }
}

pub struct ReplayReader<R: ArchiveReader> {
    reader: R,
}

impl<R: ArchiveReader> ReplayReader<R> {
    pub fn new(reader: R) -> Self {
        Self { reader }
    }
    pub fn read_metadata(&mut self) -> anyhow::Result<MetaData> {
        let reader = BufReader::new(self.reader.get_reader("metaData.json")?);
        let metadata = serde_json::from_reader(reader)?;
        Ok(metadata)
    }
    pub fn get_packet_reader<'a>(
        &'a mut self,
    ) -> anyhow::Result<ReadablePacketStream<impl Read + 'a>> {
        let reader = BufReader::new(self.reader.get_reader("recording.tmcpr")?);
        Ok(ReadablePacketStream::new(State::Login, reader))
    }
    /// メタデータを読んだうえで論理イベント列リーダーを開く。
    pub fn event_source<'a>(&'a mut self) -> anyhow::Result<McprEventSource<impl Read + 'a>> {
        let info = ReplayInfo::from(&self.read_metadata()?);
        let reader = BufReader::new(self.reader.get_reader("recording.tmcpr")?);
        Ok(McprEventSource::new(reader, info))
    }
}

pub struct ReplayWriter<W: ArchiveWriter> {
    writer: W,
}

impl<W: ArchiveWriter> ReplayWriter<W> {
    pub fn new(writer: W) -> Self {
        Self { writer }
    }

    pub fn write_metadata(&mut self, metadata: MetaData) -> anyhow::Result<()> {
        let writer = BufWriter::new(self.writer.get_writer("metaData.json")?);
        serde_json::to_writer(writer, &metadata)?;
        Ok(())
    }
    pub fn get_packet_writer<'a>(
        &'a mut self,
    ) -> anyhow::Result<WritablePacketStream<impl Write + 'a>> {
        let writer = BufWriter::new(self.writer.get_writer("recording.tmcpr")?);
        Ok(WritablePacketStream::new(writer))
    }
}

/// 論理イベント列を .mcpr アーカイブとして書き出す Sink。
///
/// ReplayMod の再生互換のため、ソースに存在しない接続フェーズ遷移
/// パケットを合成する:
/// - 最初のイベントが Login state でない場合 (= Flashback 由来)、
///   Login Success (0x02) をダミーの profile で合成する
/// - Configuration → Play の境目に Finish Configuration (0x03) が
///   無ければ合成する
///
/// `Event::Custom` (Flashback 独自 action) はパケットに対応物が
/// 無いためスキップし、件数を [`Self::skipped_custom`] で報告する。
///
/// tmcpr は内部バッファに構築し、[`EventSink::finish`] で
/// recording.tmcpr → metaData.json の順にアーカイブへ書き出す
/// (zip アーカイブは同時に 1 エントリしか開けないため)。
pub struct McprEventSink<W: ArchiveWriter> {
    archive: W,
    buffer: Vec<u8>,
    protocol_version: u32,
    written_state: State,
    last_time: u32,
    skipped_custom: usize,
    finished: bool,
}

impl<W: ArchiveWriter> McprEventSink<W> {
    /// `protocol_version` は遷移パケット合成の形式判定に使う
    /// (通常はソースの [`ReplayInfo::protocol_version`])。
    pub fn new(archive: W, protocol_version: u32) -> Self {
        Self {
            archive,
            buffer: Vec::new(),
            protocol_version,
            written_state: State::Login,
            last_time: 0,
            skipped_custom: 0,
            finished: false,
        }
    }
    /// パケットへ変換できずスキップした Custom イベントの件数。
    pub fn skipped_custom(&self) -> usize {
        self.skipped_custom
    }
    pub fn into_archive(self) -> W {
        self.archive
    }

    /// `written_state` から `target` まで遷移パケットを合成する。
    fn advance_to(&mut self, target: State, time: u32) -> anyhow::Result<()> {
        loop {
            match (self.written_state, target) {
                (state, target) if state == target => return Ok(()),
                (State::Login, State::Configuration | State::Play) => {
                    let payload =
                        login_success_payload(self.protocol_version, &uuid::Uuid::nil(), "Player")?;
                    Packet::new(time, LOGIN_SUCCESS_PACKET_ID, payload.into())
                        .write_to(&mut self.buffer)?;
                    self.written_state = State::Configuration;
                }
                (State::Configuration, State::Play) => {
                    Packet::new(time, FINISH_CONFIGURATION_PACKET_ID, Box::new([]))
                        .write_to(&mut self.buffer)?;
                    self.written_state = State::Play;
                }
                (state, target) => {
                    anyhow::bail!(
                        "cannot transition from {:?} back to {:?}: \
                         .mcpr stream must advance monotonically",
                        state,
                        target
                    );
                }
            }
        }
    }
}

impl<W: ArchiveWriter> EventSink for McprEventSink<W> {
    fn push(&mut self, event: Event) -> anyhow::Result<()> {
        match event {
            Event::Packet {
                time,
                state,
                id,
                data,
            } => {
                // tmcpr の time は u32 ms
                let time = u32::try_from(time.as_millis()).unwrap_or(u32::MAX);
                self.advance_to(state, time)?;
                Packet::new(time, id, data).write_to(&mut self.buffer)?;
                self.written_state = self.written_state.advance(id);
                self.last_time = self.last_time.max(time);
            }
            Event::Custom { .. } => self.skipped_custom += 1,
        }
        Ok(())
    }
    fn finish(&mut self, info: &ReplayInfo) -> anyhow::Result<()> {
        if self.finished {
            anyhow::bail!("McprEventSink::finish called twice");
        }
        self.finished = true;
        {
            let mut writer = self.archive.get_writer("recording.tmcpr")?;
            writer.write_all(&self.buffer)?;
            writer.flush()?;
        }
        let metadata = MetaData {
            duration: info.duration_ms.max(self.last_time as u64),
            mcversion: info.mc_version.clone(),
            fileFormat: "MCPR".to_string(),
            fileFormatVersion: 14,
            protocol: info.protocol_version,
            generator: "mcpr-lib".to_string(),
            players: info.players.clone(),
            ..Default::default()
        };
        let writer = BufWriter::new(self.archive.get_writer("metaData.json")?);
        serde_json::to_writer(writer, &metadata)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (time, id, data) の列から tmcpr バイト列を合成する。
    fn build_tmcpr(packets: &[(u32, i32, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        for (time, id, data) in packets {
            Packet::new(*time, *id, (*data).into())
                .write_to(&mut buf)
                .unwrap();
        }
        buf
    }

    #[test]
    fn packet_roundtrip() {
        let packet = Packet::new(1234, 0x2c, vec![1, 2, 3].into_boxed_slice());
        let mut buf = Vec::new();
        packet.write_to(&mut buf).unwrap();
        let read = Packet::read_from(&mut Cursor::new(&buf)).unwrap().unwrap();
        assert_eq!(packet, read);
        assert_eq!(Packet::read_from(&mut Cursor::new(&buf[..3])).unwrap(), None);
    }

    #[test]
    fn event_source_tracks_state() {
        // Login(0x00) -> LoginSuccess(0x02) -> RegistryData(0x07)
        //   -> FinishConfiguration(0x03) -> Play(0x2c)
        let buf = build_tmcpr(&[
            (0, 0x00, &[1]),
            (0, 0x02, &[2]),
            (10, 0x07, &[3]),
            (10, 0x03, &[]),
            (60, 0x2c, &[4, 5]),
        ]);
        let info = ReplayInfo::default();
        let mut source = McprEventSource::new(Cursor::new(buf), info);

        let mut events = Vec::new();
        while let Some(event) = source.next_event().unwrap() {
            events.push(event);
        }
        let states: Vec<State> = events
            .iter()
            .map(|e| match e {
                Event::Packet { state, .. } => *state,
                _ => panic!("unexpected custom event"),
            })
            .collect();
        assert_eq!(
            states,
            vec![
                State::Login,
                State::Login,
                State::Configuration,
                State::Configuration,
                State::Play,
            ]
        );
        match &events[4] {
            Event::Packet { time, id, data, .. } => {
                assert_eq!(time.as_millis(), 60);
                assert_eq!(*id, 0x2c);
                assert_eq!(data.as_ref(), &[4, 5]);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn event_source_propagates_error() {
        // ヘッダはあるが body が足りない → EOF でなくエラー
        let mut buf = build_tmcpr(&[(0, 0x00, &[1, 2, 3])]);
        buf.truncate(buf.len() - 2);
        let mut source = McprEventSource::new(Cursor::new(buf), ReplayInfo::default());
        assert!(source.next_event().is_err());
    }

    /// テスト用のメモリ上アーカイブ。
    #[derive(Default)]
    struct MemArchive(std::collections::HashMap<String, Vec<u8>>);
    impl ArchiveWriter for MemArchive {
        fn get_writer<'this>(
            &'this mut self,
            filename: &str,
        ) -> anyhow::Result<Box<dyn Write + 'this>> {
            Ok(Box::new(self.0.entry(filename.to_string()).or_default()))
        }
    }

    fn packet_event(time_ms: u64, state: State, id: i32, data: &[u8]) -> Event {
        Event::Packet {
            time: Time::from_millis(time_ms),
            state,
            id,
            data: data.into(),
        }
    }

    fn read_back(tmcpr: &[u8]) -> Vec<(State, Packet)> {
        ReadablePacketStream::new(State::Login, Cursor::new(tmcpr.to_vec())).collect()
    }

    #[test]
    fn event_sink_passes_through_mcpr_stream() {
        // 遷移パケットを含む完全な mcpr ストリームには何も合成しない
        let events = vec![
            packet_event(0, State::Login, 0x00, &[1]),
            packet_event(0, State::Login, 0x02, &[2]),
            packet_event(10, State::Configuration, 0x07, &[3]),
            packet_event(10, State::Configuration, 0x03, &[]),
            packet_event(60, State::Play, 0x2c, &[4]),
        ];
        let mut sink = McprEventSink::new(MemArchive::default(), 774);
        for event in events.clone() {
            sink.push(event).unwrap();
        }
        sink.finish(&ReplayInfo::default()).unwrap();
        let archive = sink.into_archive();
        let packets = read_back(&archive.0["recording.tmcpr"]);
        assert_eq!(packets.len(), 5);
        let ids: Vec<i32> = packets.iter().map(|(_, p)| p.id()).collect();
        assert_eq!(ids, vec![0x00, 0x02, 0x07, 0x03, 0x2c]);
    }

    #[test]
    fn event_sink_synthesizes_transitions_for_flashback_stream() {
        // Flashback 由来: Configuration から始まり、遷移パケットを含まない
        let mut sink = McprEventSink::new(MemArchive::default(), 774);
        sink.push(packet_event(0, State::Configuration, 0x07, &[3]))
            .unwrap();
        sink.push(packet_event(0, State::Play, 0x2b, &[4])).unwrap();
        sink.push(Event::Custom {
            time: Time::from_millis(50),
            name: "flashback:action/move_entities".to_string(),
            data: vec![9].into(),
        })
        .unwrap();
        sink.finish(&ReplayInfo::default()).unwrap();
        assert_eq!(sink.skipped_custom(), 1);

        let archive = sink.into_archive();
        let packets = read_back(&archive.0["recording.tmcpr"]);
        // Login Success / Finish Configuration が合成され、state 遷移が成立する
        let expected: Vec<(State, i32)> = vec![
            (State::Login, 0x02),
            (State::Configuration, 0x07),
            (State::Configuration, 0x03),
            (State::Play, 0x2b),
        ];
        assert_eq!(
            packets
                .iter()
                .map(|(s, p)| (*s, p.id()))
                .collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn event_sink_synthesizes_both_transitions_without_configuration() {
        // configuration なしでいきなり Play が来ても 0x02 + 0x03 を連続合成
        let mut sink = McprEventSink::new(MemArchive::default(), 774);
        sink.push(packet_event(0, State::Play, 0x2c, &[1])).unwrap();
        sink.finish(&ReplayInfo::default()).unwrap();
        let archive = sink.into_archive();
        let ids: Vec<i32> = read_back(&archive.0["recording.tmcpr"])
            .iter()
            .map(|(_, p)| p.id())
            .collect();
        assert_eq!(ids, vec![0x02, 0x03, 0x2c]);
    }

    #[test]
    fn event_sink_rejects_backwards_transition() {
        let mut sink = McprEventSink::new(MemArchive::default(), 774);
        sink.push(packet_event(0, State::Play, 0x2c, &[])).unwrap();
        let err = sink
            .push(packet_event(0, State::Configuration, 0x07, &[]))
            .unwrap_err();
        assert!(err.to_string().contains("transition"));
    }

    #[test]
    fn event_sink_writes_metadata_from_info() {
        let mut sink = McprEventSink::new(MemArchive::default(), 774);
        sink.push(packet_event(12345, State::Play, 0x2c, &[]))
            .unwrap();
        let info = ReplayInfo {
            mc_version: "1.21.11".to_string(),
            protocol_version: 774,
            duration_ms: 6150,
            data_version: Some(4671),
            players: HashSet::new(),
        };
        sink.finish(&info).unwrap();
        let archive = sink.into_archive();
        let metadata: MetaData =
            serde_json::from_slice(&archive.0["metaData.json"]).unwrap();
        assert_eq!(metadata.protocol, 774);
        assert_eq!(metadata.mcversion, "1.21.11");
        assert_eq!(metadata.fileFormat, "MCPR");
        assert_eq!(metadata.fileFormatVersion, 14);
        // 実際に書いた最終 time の方が大きければそちらを採用
        assert_eq!(metadata.duration, 12345);
    }
}
