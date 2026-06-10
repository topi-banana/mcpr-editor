use std::{
    collections::HashSet,
    io::{self, BufReader, BufWriter, Cursor, Read, Write},
};

use serde::{Deserialize, Serialize};

use crate::{
    archive::{ArchiveReader, ArchiveWriter},
    event::{Event, EventSource, ReplayInfo, Time},
    protocol::{Deserializer, Serializer},
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
}
