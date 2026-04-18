use std::{
    collections::{BTreeMap, HashMap},
    io::{self, BufReader, BufWriter, Read, Write},
    str::FromStr,
};

use serde::{Deserialize, Serialize};

use crate::{
    archive::{ArchiveReader, ArchiveWriter},
    protocol::{Deserializer, Serializer},
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ActionKind {
    NextTick,
    GamePacket,
    ConfigurationPacket,
    CreateLocalPlayer,
    MoveEntities,
    LevelChunkCached,
    AccuratePlayerPosition,
    /// サードパーティ mod が追加する action 等、既知列挙に無いもの。
    /// 元の名前を保持して書き戻し時に復元する。
    Unknown(String),
}

impl ActionKind {
    pub fn as_str(&self) -> &str {
        match self {
            ActionKind::NextTick => "flashback:action/next_tick",
            ActionKind::GamePacket => "flashback:action/game_packet",
            ActionKind::ConfigurationPacket => "flashback:action/configuration_packet",
            ActionKind::CreateLocalPlayer => "flashback:action/create_local_player",
            ActionKind::MoveEntities => "flashback:action/move_entities",
            ActionKind::LevelChunkCached => "flashback:action/level_chunk_cached",
            ActionKind::AccuratePlayerPosition => "flashback:action/accurate_player_position",
            ActionKind::Unknown(s) => s.as_str(),
        }
    }
    /// 既知 action は enum variant に、それ以外は `Unknown` に分類する。
    pub fn parse(name: &str) -> Self {
        Self::from_str(name).unwrap_or_else(|()| ActionKind::Unknown(name.to_string()))
    }
}

impl FromStr for ActionKind {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    kind: ActionKind,
    data: Box<[u8]>,
}

impl Action {
    pub fn new(kind: ActionKind, data: Box<[u8]>) -> Self {
        Self { kind, data }
    }
    pub fn kind(&self) -> &ActionKind {
        &self.kind
    }
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaData {
    pub uuid: uuid::Uuid,
    pub name: String,
    pub version_string: String,
    pub world_name: Option<String>,
    pub data_version: u32,
    pub protocol_version: u32,
    pub total_ticks: u64,
    pub markers: Option<serde_json::Value>,
    pub chunks: BTreeMap<String, ChunkMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkMeta {
    pub duration: u64,
    #[serde(rename = "forcePlaySnapshot", default)]
    pub force_play_snapshot: bool,
}

/// flashback chunk ファイル先頭 i32 (big-endian)。
pub const MAGIC_NUMBER: i32 = -679417724;

#[derive(Debug)]
pub struct ChunkReader<R> {
    actions: Box<[ActionKind]>,
    snapshot: Box<[u8]>,
    reader: R,
}

impl<R: Read> ChunkReader<R> {
    pub fn new(mut reader: R) -> anyhow::Result<Self> {
        let magic = reader.read_int()?;
        if magic != MAGIC_NUMBER {
            anyhow::bail!("invalid flashback chunk magic: 0x{:08x}", magic);
        }
        let action_count = reader.read_varint()? as usize;
        let mut actions = Vec::with_capacity(action_count);
        for _ in 0..action_count {
            let name = reader.read_string()?;
            actions.push(ActionKind::parse(&name));
        }
        let snapshot_size = reader.read_int()?;
        if snapshot_size < 0 {
            anyhow::bail!("negative snapshot_size: {}", snapshot_size);
        }
        let mut snapshot = vec![0u8; snapshot_size as usize];
        reader.read_exact(&mut snapshot)?;
        Ok(Self {
            actions: actions.into_boxed_slice(),
            snapshot: snapshot.into_boxed_slice(),
            reader,
        })
    }
    pub fn actions(&self) -> &[ActionKind] {
        &self.actions
    }
    pub fn snapshot(&self) -> &[u8] {
        &self.snapshot
    }
}

impl<R: Read> Iterator for ChunkReader<R> {
    type Item = Action;
    fn next(&mut self) -> Option<Self::Item> {
        let action_id = self.reader.read_varint().ok()?;
        let length = self.reader.read_int().ok()?;
        if length < 0 {
            return None;
        }
        let mut data = vec![0u8; length as usize];
        self.reader.read_exact(&mut data).ok()?;
        let kind = self.actions.get(action_id as usize)?.clone();
        Some(Action::new(kind, data.into_boxed_slice()))
    }
}

pub struct ChunkWriter<W: Write> {
    writer: W,
    index: HashMap<ActionKind, u32>,
}

impl<W: Write> ChunkWriter<W> {
    pub fn new(mut writer: W, actions: &[ActionKind], snapshot: &[u8]) -> anyhow::Result<Self> {
        writer.write_all(&MAGIC_NUMBER.to_be_bytes())?;
        writer.write_varint(actions.len() as i32)?;
        let mut index = HashMap::with_capacity(actions.len());
        for (i, action) in actions.iter().enumerate() {
            let name = action.as_str();
            writer.write_varint(name.len() as i32)?;
            writer.write_all(name.as_bytes())?;
            index.insert(action.clone(), i as u32);
        }
        writer.write_all(&(snapshot.len() as i32).to_be_bytes())?;
        writer.write_all(snapshot)?;
        Ok(Self { writer, index })
    }
    pub fn push(&mut self, action: &Action) -> anyhow::Result<()> {
        let id = self
            .index
            .get(&action.kind)
            .ok_or_else(|| anyhow::anyhow!("action {:?} not in registry", action.kind))?;
        self.writer.write_varint(*id as i32)?;
        self.writer
            .write_all(&(action.data.len() as i32).to_be_bytes())?;
        self.writer.write_all(&action.data)?;
        Ok(())
    }
    /// バッファをフラッシュして内部 writer を返す。
    pub fn finish(mut self) -> io::Result<W> {
        self.writer.flush()?;
        Ok(self.writer)
    }
}

pub struct FlashbackReader<R: ArchiveReader> {
    reader: R,
}

impl<R: ArchiveReader> FlashbackReader<R> {
    pub fn new(reader: R) -> Self {
        Self { reader }
    }
    pub fn get_metadata(&mut self) -> anyhow::Result<MetaData> {
        let reader = BufReader::new(self.reader.get_reader("metadata.json")?);
        let metadata = serde_json::from_reader(reader)?;
        Ok(metadata)
    }
    pub fn get_chunk_reader<'a>(
        &'a mut self,
        filename: &str,
    ) -> anyhow::Result<ChunkReader<impl Read + 'a>> {
        let reader = BufReader::new(self.reader.get_reader(filename)?);
        ChunkReader::new(reader)
    }
}

pub struct FlashbackWriter<W: ArchiveWriter> {
    writer: W,
}

impl<W: ArchiveWriter> FlashbackWriter<W> {
    pub fn new(writer: W) -> Self {
        Self { writer }
    }
    pub fn write_metadata(&mut self, metadata: &MetaData) -> anyhow::Result<()> {
        let writer = BufWriter::new(self.writer.get_writer("metadata.json")?);
        serde_json::to_writer(writer, metadata)?;
        Ok(())
    }
    pub fn get_chunk_writer<'a>(
        &'a mut self,
        filename: &str,
        actions: &[ActionKind],
        snapshot: &[u8],
    ) -> anyhow::Result<ChunkWriter<impl Write + 'a>> {
        let writer = BufWriter::new(self.writer.get_writer(filename)?);
        ChunkWriter::new(writer, actions, snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn action_kind_roundtrip_known() {
        for name in [
            "flashback:action/next_tick",
            "flashback:action/game_packet",
            "flashback:action/configuration_packet",
            "flashback:action/create_local_player",
            "flashback:action/move_entities",
            "flashback:action/level_chunk_cached",
            "flashback:action/accurate_player_position",
        ] {
            let k = ActionKind::parse(name);
            assert!(!matches!(k, ActionKind::Unknown(_)));
            assert_eq!(k.as_str(), name);
        }
    }

    #[test]
    fn action_kind_roundtrip_unknown() {
        let name = "arcade-replay:action/foo";
        let k = ActionKind::parse(name);
        assert!(matches!(k, ActionKind::Unknown(_)));
        assert_eq!(k.as_str(), name);
    }

    #[test]
    fn chunk_roundtrip() {
        let actions = vec![
            ActionKind::NextTick,
            ActionKind::GamePacket,
            ActionKind::Unknown("arcade-replay:action/foo".to_string()),
        ];
        let snapshot: Vec<u8> = (0u8..32).collect();
        let packets = vec![
            Action::new(ActionKind::NextTick, Box::new([])),
            Action::new(ActionKind::GamePacket, vec![1, 2, 3, 4].into_boxed_slice()),
            Action::new(
                ActionKind::Unknown("arcade-replay:action/foo".to_string()),
                vec![9, 9, 9].into_boxed_slice(),
            ),
            Action::new(ActionKind::NextTick, Box::new([])),
            Action::new(ActionKind::GamePacket, vec![5].into_boxed_slice()),
        ];

        let mut buf: Vec<u8> = Vec::new();
        {
            let mut w = ChunkWriter::new(&mut buf, &actions, &snapshot).unwrap();
            for p in &packets {
                w.push(p).unwrap();
            }
            w.finish().unwrap();
        }

        let mut r = ChunkReader::new(Cursor::new(&buf)).unwrap();
        assert_eq!(r.actions(), actions.as_slice());
        assert_eq!(r.snapshot(), snapshot.as_slice());
        let read: Vec<Action> = (&mut r).collect();
        assert_eq!(read, packets);
    }

    #[test]
    fn chunk_invalid_magic() {
        let mut buf: Vec<u8> = Vec::new();
        buf.extend_from_slice(&0i32.to_be_bytes());
        let err = ChunkReader::new(Cursor::new(&buf)).unwrap_err();
        assert!(err.to_string().contains("magic"));
    }

    #[test]
    fn chunk_writer_rejects_unregistered_action() {
        let actions = vec![ActionKind::NextTick];
        let mut buf: Vec<u8> = Vec::new();
        let mut w = ChunkWriter::new(&mut buf, &actions, &[]).unwrap();
        let err = w
            .push(&Action::new(ActionKind::GamePacket, Box::new([])))
            .unwrap_err();
        assert!(err.to_string().contains("not in registry"));
    }
}
