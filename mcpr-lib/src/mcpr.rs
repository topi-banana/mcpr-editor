use std::{
    collections::HashSet,
    io::{self, Cursor, Read, Seek, Write},
};

use serde::{Deserialize, Serialize};
use zip::{
    read::ZipArchive,
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
    singleplayer: bool,
    serverName: String,
    customServerName: String,
    duration: u64,
    date: u64,
    mcversion: String,
    fileFormat: String,
    fileFormatVersion: u32,
    protocol: u32,
    generator: String,
    selfId: i32,
    players: HashSet<uuid::Uuid>,
}
impl MetaData {
    pub fn read_from<R: Read>(reader: &mut R) -> Result<Self, serde_json::Error> {
        serde_json::from_reader(reader)
    }
    pub fn write_to<W: Write>(&self, writer: &mut W) -> Result<(), serde_json::Error> {
        serde_json::to_writer(writer, self)
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

#[derive(Debug, Clone)]
pub struct ReplayStream {
    packet_restriction: [bool; 256],
    unknow_packet: bool,
    compression_level: Option<i64>,
    interval: u32,
}
impl Default for ReplayStream {
    fn default() -> Self {
        Self::new(true, true)
    }
}
impl ReplayStream {
    pub fn new(default: bool, unknow_packet: bool) -> Self {
        Self {
            packet_restriction: [default; 256],
            unknow_packet,
            compression_level: None,
            interval: 0,
        }
    }
    pub fn exclude<T: Iterator<Item = u8>>(&mut self, iter: T) {
        for p in iter {
            self.packet_restriction[p as usize] = false;
        }
    }
    pub fn include<T: Iterator<Item = u8>>(&mut self, iter: T) {
        for p in iter {
            self.packet_restriction[p as usize] = true;
        }
    }
    pub fn compression_level(&mut self, compression_level: i64) {
        self.compression_level = if compression_level < 0 {
            None
        } else {
            Some(compression_level)
        };
    }
    pub fn interval(&mut self, interval: u32) {
        self.interval = interval;
    }

    pub fn stream<'a, R, W, F>(
        &self,
        readers: &mut [R],
        writer: &'a mut Option<W>,
        f: F,
    ) -> io::Result<()>
    where
        R: Read + Seek,
        W: Write + Seek + 'a,
        F: Fn(Packet, &mut Option<ZipWriter<&mut W>>) -> bool,
    {
        if readers.is_empty() {
            return Ok(());
        }
        let mut zip_writer = if let Some(e) = writer {
            let mut zipw = ZipWriter::new(e);
            zipw.start_file(
                "recording.tmcpr",
                SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated)
                    .compression_level(self.compression_level),
            )?;
            Some(zipw)
        } else {
            None
        };
        let mut offset = 0;
        let mut last = 0;
        for reader in readers {
            let mut zip = ZipArchive::new(reader)?;
            let mut tmcpr_file = zip.by_name("recording.tmcpr")?;
            while let Some(mut packet) = Packet::read_from(&mut tmcpr_file)? {
                packet.time += offset;
                last = packet.time;
                let p = if packet.id < 256 {
                    self.packet_restriction[packet.id as usize]
                } else {
                    self.unknow_packet
                };
                if p && f(packet, &mut zip_writer) {
                    return Ok(());
                }
            }
            offset = last + self.interval;
        }
        if let Some(w) = zip_writer {
            w.finish()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod test {
    #[test]
    fn test() {}
}
