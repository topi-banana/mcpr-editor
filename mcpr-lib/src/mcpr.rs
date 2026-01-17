use std::{
    collections::HashSet,
    io::{self, BufReader, BufWriter, Cursor, Read, Write},
};

use serde::{Deserialize, Serialize};

use crate::{
    archive::{ArchiveReader, ArchiveWriter},
    protocol::{Deserializer, Serializer},
};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Handshaking,
    Status,
    Login,
    Configuration,
    Play,
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
                if old_state == State::Login && packet.id() == 0x02 {
                    self.state = State::Configuration;
                }
                if old_state == State::Configuration && packet.id() == 0x03 {
                    self.state = State::Play;
                }
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
