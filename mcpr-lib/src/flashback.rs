use std::{
    collections::BTreeMap,
    io::{BufReader, BufWriter},
    str::FromStr,
};

use serde::{Deserialize, Serialize};

use crate::{
    archive::{ArchiveReader, ArchiveWriter},
    protocol::Deserializer,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ActionKind {
    NextTick,
    GamePacket,
    ConfigurationPacket,
    CreateLocalPlayer,
    MoveEntities,
    LevelChunkCached,
    AccuratePlayerPosition,
    Unknown,
}

impl std::str::FromStr for ActionKind {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "flashback:action/next_tick" => ActionKind::NextTick,
            "flashback:action/game_packet" => ActionKind::GamePacket,
            "flashback:action/configuration_packet" => ActionKind::ConfigurationPacket,
            "flashback:action/create_local_player" => ActionKind::CreateLocalPlayer,
            "flashback:action/move_entities" => ActionKind::MoveEntities,
            "flashback:action/level_chunk_cached" => ActionKind::LevelChunkCached,
            "flashback:action/accurate_player_position" => ActionKind::AccuratePlayerPosition,
            _ => return Err(()),
        })
    }
}

#[derive(Debug, Clone, PartialEq, PartialOrd)]
pub struct Action {
    id: ActionKind,
    data: Box<[u8]>,
}

impl Action {
    pub fn new(id: ActionKind, data: Box<[u8]>) -> Self {
        Self { id, data }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaData {
    uuid: uuid::Uuid,
    name: String,
    version_string: String,
    world_name: Option<String>,
    data_version: u32,
    protocol_version: u32,
    total_ticks: u64,
    markers: Option<serde_json::Value>,
    chunks: BTreeMap<String, ChunkMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkMeta {
    duration: u64,
    // #[serde(rename = "forcePlaySnapshot")]
    // force_play_snapshot: bool,
}

pub struct ReadableChunkPacketStream<R> {
    actions: Box<[ActionKind]>,
    reader: R,
}

impl<R> ReadableChunkPacketStream<R> {
    pub fn new(actions: Box<[ActionKind]>, reader: R) -> Self {
        Self { actions, reader }
    }
}

impl<R: std::io::Read> Iterator for ReadableChunkPacketStream<R> {
    type Item = Action;
    fn next(&mut self) -> Option<Self::Item> {
        let action_id = self.reader.read_varint().ok()?;
        let length = self.reader.read_int().ok()?;
        let mut result = vec![0; length as usize];
        self.reader.read_exact(&mut result).ok()?;
        Some(Action::new(
            self.actions[action_id as usize],
            result.into_boxed_slice(),
        ))
    }
}

pub struct FlashbackReader<R: ArchiveReader> {
    reader: R,
}

const MAGIC_NUMBER: i32 = -679417724;

impl<R: ArchiveReader> FlashbackReader<R> {
    pub fn new(reader: R) -> Self {
        Self { reader }
    }
    pub fn get_metadata(&mut self) -> anyhow::Result<MetaData> {
        let reader = BufReader::new(self.reader.get_reader("metadata.json")?);
        let metadata = serde_json::from_reader(reader)?;
        Ok(metadata)
    }
    pub fn get_packet_reader(&mut self) -> anyhow::Result<()> {
        let metadata = self.get_metadata()?;
        let mut ticks = 0;
        for (filename, chunkmeta) in &metadata.chunks {
            let mut reader = self.reader.get_reader(filename)?;
            let magic = reader.read_int()?;
            if magic != MAGIC_NUMBER {
                panic!("Invalid magic number: {:0x}", magic);
            }
            let action_count = reader.read_varint()? as usize;
            let mut actions = Vec::with_capacity(action_count);
            for _ in 0..action_count {
                let action_name = reader.read_string()?;
                actions.push(ActionKind::from_str(&action_name).unwrap_or(ActionKind::Unknown));
            }
            let snapshot_size = reader.read_int()?;
            for _ in 0..snapshot_size {
                let _ = reader.read_byte()?;
            }
            let packet_stream = ReadableChunkPacketStream::new(actions.into_boxed_slice(), reader);
            let mut cur_ticks = ticks;
            for packet in packet_stream {
                match packet.id {
                    ActionKind::NextTick => {
                        cur_ticks += 1;
                        continue;
                    }
                    ActionKind::GamePacket => {
                        eprintln!("> {cur_ticks} {:?}", packet)
                    }
                    _ => {
                        eprintln!("# {cur_ticks} {:?}", packet)
                    }
                }
            }
            ticks += chunkmeta.duration;
        }
        Ok(())
    }
}

pub struct FlashbackWriter<W: ArchiveWriter> {
    writer: W,
}

impl<W: ArchiveWriter> FlashbackWriter<W> {
    pub fn new(writer: W) -> Self {
        Self { writer }
    }
    pub fn write_metadata(&mut self, metadata: MetaData) -> anyhow::Result<()> {
        let writer = BufWriter::new(self.writer.get_writer("metadata.json")?);
        serde_json::to_writer(writer, &metadata)?;
        Ok(())
    }
}
