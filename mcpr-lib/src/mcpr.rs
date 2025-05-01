use std::{
    collections::HashSet,
    fs::File,
    io::{self, BufReader, BufWriter, Cursor, Read, Seek, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use zip::{
    read::ZipArchive,
    result::ZipError,
    write::{SimpleFileOptions, ZipWriter},
};

use crate::protocol::{Deserializer, Serializer};

#[derive(Debug, Clone, PartialEq, PartialOrd)]
pub struct Packet {
    time: u32,
    id: i32,
    data: Vec<u8>,
}
impl Packet {
    pub fn new(time: u32, id: i32, data: Vec<u8>) -> Self {
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
    pub fn data(&self) -> &Vec<u8> {
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
                Ok(Some(Packet::new(time, packet_id, packet_data)))
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
impl MetaData {
    pub fn read_from<R: Read>(reader: R) -> Result<Self, Error> {
        serde_json::from_reader(reader).map_err(Error::JsonError)
    }
    pub fn write_to<W: Write>(&self, writer: &mut W) -> Result<(), Error> {
        serde_json::to_writer(writer, self).map_err(Error::JsonError)
    }
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

#[derive(Debug)]
pub enum Error {
    ZipError(ZipError),
    IOError(io::Error),
    JsonError(serde_json::Error),
}

pub trait ReplayReader {
    fn read_metadata(&mut self) -> Result<MetaData, Error>;
    fn get_packet_reader<'a>(
        &'a mut self,
    ) -> Result<ReadablePacketStream<Box<dyn Read + 'a>>, Error>;
}
pub trait ReplayWriter {
    fn write_metadata(&mut self, metadata: MetaData) -> Result<(), Error>;
    fn get_packet_writer<'a>(
        &'a mut self,
    ) -> Result<WritablePacketStream<Box<dyn Write + 'a>>, Error>;
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
    fn new(state: State, reader: R) -> Self {
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

pub struct MCPRReader<R: Read + Seek> {
    zip: ZipArchive<R>,
}
impl<R: Read + Seek> MCPRReader<R> {
    pub fn new(reader: R) -> Result<Self, Error> {
        Ok(Self {
            zip: ZipArchive::new(reader).map_err(Error::ZipError)?,
        })
    }
}
impl<R: Read + Seek> ReplayReader for MCPRReader<R> {
    fn read_metadata(&mut self) -> Result<MetaData, Error> {
        let file = self.zip.by_name("metaData.json").map_err(Error::ZipError)?;
        MetaData::read_from(file)
    }
    fn get_packet_reader<'a>(
        &'a mut self,
    ) -> Result<ReadablePacketStream<Box<dyn Read + 'a>>, Error> {
        let reader = self
            .zip
            .by_name("recording.tmcpr")
            .map_err(Error::ZipError)?;
        Ok(ReadablePacketStream::new(State::Login, Box::new(reader)))
    }
}

pub struct MCPRWriter<W: Write + Seek> {
    zip: ZipWriter<W>,
    compression_level: Option<i64>,
}
impl<W: Write + Seek> MCPRWriter<W> {
    pub fn new(writer: W, compression_level: Option<i64>) -> Result<Self, Error> {
        Ok(Self {
            zip: ZipWriter::new(writer),
            compression_level,
        })
    }
}
impl<W: Write + Seek> ReplayWriter for MCPRWriter<W> {
    fn write_metadata(&mut self, metadata: MetaData) -> Result<(), Error> {
        self.zip
            .start_file(
                "metaData.json",
                SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated)
                    .compression_level(Some(264)),
            )
            .map_err(Error::ZipError)?;
        metadata.write_to(&mut self.zip)?;
        Ok(())
    }
    fn get_packet_writer<'a>(
        &'a mut self,
    ) -> Result<WritablePacketStream<Box<dyn Write + 'a>>, Error> {
        self.zip
            .start_file(
                "recording.tmcpr",
                SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated)
                    .compression_level(self.compression_level),
            )
            .map_err(Error::ZipError)?;
        Ok(WritablePacketStream::new(Box::new(&mut self.zip)))
    }
}

pub struct DirReaderWriter {
    path: PathBuf,
}
impl DirReaderWriter {
    pub fn new<S: AsRef<Path>>(path: S) -> Option<Self> {
        if path.as_ref().is_dir() {
            Some(Self {
                path: path.as_ref().to_path_buf(),
            })
        } else {
            None
        }
    }
}
impl ReplayReader for DirReaderWriter {
    fn read_metadata(&mut self) -> Result<MetaData, Error> {
        let metadata_json = self.path.join("metaData.json");
        let reader = BufReader::new(File::open(metadata_json).map_err(Error::IOError)?);
        MetaData::read_from(reader)
    }
    fn get_packet_reader<'a>(
        &'a mut self,
    ) -> Result<ReadablePacketStream<Box<dyn Read + 'a>>, Error> {
        let recording_tmcpr = self.path.join("recording.tmcpr");
        let reader = BufReader::new(File::open(recording_tmcpr).map_err(Error::IOError)?);
        Ok(ReadablePacketStream::new(State::Login, Box::new(reader)))
    }
}
impl ReplayWriter for DirReaderWriter {
    fn write_metadata(&mut self, metadata: MetaData) -> Result<(), Error> {
        let metadata_json = self.path.join("metaData.json");
        let mut writer = BufWriter::new(File::create(metadata_json).map_err(Error::IOError)?);
        metadata.write_to(&mut writer)
    }
    fn get_packet_writer<'a>(
        &'a mut self,
    ) -> Result<WritablePacketStream<Box<dyn Write + 'a>>, Error> {
        let recording_tmcpr = self.path.join("recording.tmcpr");
        let writer = BufWriter::new(File::create(recording_tmcpr).map_err(Error::IOError)?);
        Ok(WritablePacketStream::new(Box::new(writer)))
    }
}
