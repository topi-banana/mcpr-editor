use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    io::{self, BufReader, BufWriter, Cursor, Read, Write},
    str::FromStr,
};

use serde::{Deserialize, Serialize};

use crate::{
    archive::{ArchiveReader, ArchiveWriter},
    event::{Event, EventSink, EventSource, ReplayInfo, State, Time},
    protocol::{Deserializer, Serializer},
};

/// level_chunk_caches の 1 ファイルあたり最大エントリ数。
/// (Flashback mod の ReplayChunkCache.CHUNK_CACHE_SIZE に対応)
pub const CHUNK_CACHE_SIZE: u32 = 10000;

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
    /// 既知 action の一覧 (Flashback mod の ActionRegistry 登録順)。
    /// chunk ヘッダの action テーブルはこの全種を常に登録する。
    pub const KNOWN: [ActionKind; 7] = [
        ActionKind::NextTick,
        ActionKind::GamePacket,
        ActionKind::ConfigurationPacket,
        ActionKind::CreateLocalPlayer,
        ActionKind::MoveEntities,
        ActionKind::LevelChunkCached,
        ActionKind::AccuratePlayerPosition,
    ];

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
    pub fn into_data(self) -> Box<[u8]> {
        self.data
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

impl MetaData {
    /// 再生順 (チャンク名に含まれる数値の昇順) の chunk 名一覧。
    ///
    /// `chunks` は BTreeMap のため辞書順 ("c10" < "c2") になっており、
    /// そのまま辿ると再生順を壊す。数値を持たない名前は末尾に辞書順で並ぶ。
    pub fn chunks_in_order(&self) -> Vec<String> {
        let mut names: Vec<&String> = self.chunks.keys().collect();
        fn numeric_key(name: &str) -> u64 {
            let digits: String = name
                .chars()
                .skip_while(|c| !c.is_ascii_digit())
                .take_while(|c| c.is_ascii_digit())
                .collect();
            digits.parse().unwrap_or(u64::MAX)
        }
        names.sort_by(|a, b| {
            numeric_key(a)
                .cmp(&numeric_key(b))
                .then_with(|| a.cmp(b))
        });
        names.into_iter().cloned().collect()
    }
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
    /// 次の action を読む。終端で `Ok(None)`、途中破損はエラー。
    pub fn next_action(&mut self) -> anyhow::Result<Option<Action>> {
        read_action_from(&mut self.reader, &self.actions)
    }
}

/// action 列 (`VarInt action_id` + `i32 size` + data) から 1 件読む。
/// chunk 本体と snapshot は同じ表現なので両方で使う。
fn read_action_from<R: Read>(
    reader: &mut R,
    actions: &[ActionKind],
) -> anyhow::Result<Option<Action>> {
    let action_id = match reader.read_varint() {
        Ok(id) => id,
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let length = reader.read_int()?;
    if length < 0 {
        anyhow::bail!("negative action length: {}", length);
    }
    let mut data = vec![0u8; length as usize];
    reader.read_exact(&mut data)?;
    let kind = actions
        .get(action_id as usize)
        .ok_or_else(|| anyhow::anyhow!("action id {} out of registry range", action_id))?
        .clone();
    Ok(Some(Action::new(kind, data.into_boxed_slice())))
}

impl<R: Read> Iterator for ChunkReader<R> {
    type Item = Action;
    fn next(&mut self) -> Option<Self::Item> {
        self.next_action().ok().flatten()
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
    /// アーカイブ内のファイルを丸ごと読む。
    /// (借用を保持しないため、読みながら別ファイルを開ける)
    fn read_file_fully(&mut self, filename: &str) -> anyhow::Result<Vec<u8>> {
        let mut reader = self.reader.get_reader(filename)?;
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        Ok(bytes)
    }
    /// 論理イベント列リーダーを開く (self を消費する)。
    ///
    /// `include_snapshot` が真のとき、最初の chunk の snapshot
    /// (configuration packets / login packet 等の初期状態) を
    /// tick 0 のイベントとして先頭に流す。後続 chunk の snapshot は
    /// シーク用の冗長データなので常にスキップする。
    pub fn event_source(
        mut self,
        include_snapshot: bool,
    ) -> anyhow::Result<FlashbackEventSource<R>> {
        let metadata = self.get_metadata()?;
        let info = ReplayInfo::from(&metadata);
        let pending_chunks = metadata.chunks_in_order().into();
        Ok(FlashbackEventSource {
            reader: self,
            info,
            pending_chunks,
            current: None,
            tick: 0,
            chunk_caches: HashMap::new(),
            include_snapshot,
            opened_any_chunk: false,
        })
    }
}

struct CurrentChunk {
    reader: ChunkReader<Cursor<Vec<u8>>>,
    /// 流すべき snapshot の残り。読み終わったら None。
    snapshot: Option<Cursor<Vec<u8>>>,
}

/// Flashback リプレイを論理イベント列として読み出すアダプタ。
///
/// 物理表現をすべて論理層に正規化する:
/// - `NextTick` はイベントにせず、後続イベントの time に織り込む
/// - `LevelChunkCached` は `level_chunk_caches/N` を解決して
///   チャンクパケットをインライン展開する
/// - `GamePacket` / `ConfigurationPacket` は state 付き Packet に
/// - その他の action は Custom として生バイトのまま透過する
pub struct FlashbackEventSource<R: ArchiveReader> {
    reader: FlashbackReader<R>,
    info: ReplayInfo,
    pending_chunks: VecDeque<String>,
    current: Option<CurrentChunk>,
    tick: u64,
    /// cache_index → エントリ (VarInt packet id + body) の列
    chunk_caches: HashMap<u32, Vec<Box<[u8]>>>,
    include_snapshot: bool,
    opened_any_chunk: bool,
}

impl<R: ArchiveReader> FlashbackEventSource<R> {
    /// chunk / snapshot を横断して次の action を返す。
    fn next_action(&mut self) -> anyhow::Result<Option<Action>> {
        loop {
            if self.current.is_none() {
                let Some(name) = self.pending_chunks.pop_front() else {
                    return Ok(None);
                };
                let bytes = self.reader.read_file_fully(&name)?;
                let reader = ChunkReader::new(Cursor::new(bytes))?;
                let snapshot = (self.include_snapshot && !self.opened_any_chunk)
                    .then(|| Cursor::new(reader.snapshot().to_vec()));
                self.opened_any_chunk = true;
                self.current = Some(CurrentChunk { reader, snapshot });
            }
            let current = self.current.as_mut().unwrap();
            if let Some(snapshot) = &mut current.snapshot {
                match read_action_from(snapshot, current.reader.actions())? {
                    Some(action) => return Ok(Some(action)),
                    None => {
                        current.snapshot = None;
                        continue;
                    }
                }
            }
            match current.reader.next_action()? {
                Some(action) => return Ok(Some(action)),
                None => {
                    self.current = None;
                    continue;
                }
            }
        }
    }

    /// `level_chunk_caches` からグローバル index のチャンクパケット
    /// (VarInt packet id + body) を引く。
    fn cached_chunk_payload(&mut self, index: u32) -> anyhow::Result<Box<[u8]>> {
        let cache_index = index / CHUNK_CACHE_SIZE;
        let offset = (index % CHUNK_CACHE_SIZE) as usize;
        if !self.chunk_caches.contains_key(&cache_index) {
            let entries = self.load_chunk_cache(cache_index)?;
            self.chunk_caches.insert(cache_index, entries);
        }
        self.chunk_caches[&cache_index]
            .get(offset)
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "cached chunk index {} not found (cache file {} has fewer entries)",
                    index,
                    cache_index
                )
            })
    }

    /// `level_chunk_caches/<N>` を読み、`i32 BE size` + データの連結を
    /// エントリ列に分割する。N=0 で存在しない場合は旧形式の
    /// 単一ファイル `level_chunk_cache` にフォールバックする。
    fn load_chunk_cache(&mut self, cache_index: u32) -> anyhow::Result<Vec<Box<[u8]>>> {
        let bytes = match self
            .reader
            .read_file_fully(&format!("level_chunk_caches/{}", cache_index))
        {
            Ok(bytes) => bytes,
            Err(e) if cache_index == 0 => self
                .reader
                .read_file_fully("level_chunk_cache")
                .map_err(|_| e)?,
            Err(e) => return Err(e),
        };
        let mut entries = Vec::new();
        let mut cursor = Cursor::new(bytes.as_slice());
        loop {
            let size = match cursor.read_int() {
                Ok(size) => size,
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e.into()),
            };
            if size < 0 {
                anyhow::bail!("negative entry size in level_chunk_cache: {}", size);
            }
            let mut data = vec![0u8; size as usize];
            cursor.read_exact(&mut data)?;
            entries.push(data.into_boxed_slice());
        }
        Ok(entries)
    }

    /// action を論理イベントへ変換する。時間進行のみの action は None。
    fn action_to_event(&mut self, action: Action) -> anyhow::Result<Option<Event>> {
        let time = Time::from_ticks(self.tick);
        match action.kind() {
            ActionKind::NextTick => {
                self.tick += 1;
                Ok(None)
            }
            ActionKind::GamePacket => {
                let (id, data) = split_packet_payload(action.data())?;
                Ok(Some(Event::Packet {
                    time,
                    state: State::Play,
                    id,
                    data,
                }))
            }
            ActionKind::ConfigurationPacket => {
                let (id, data) = split_packet_payload(action.data())?;
                Ok(Some(Event::Packet {
                    time,
                    state: State::Configuration,
                    id,
                    data,
                }))
            }
            ActionKind::LevelChunkCached => {
                let index = Cursor::new(action.data()).read_varint()?;
                if index < 0 {
                    anyhow::bail!("negative level_chunk_cached index: {}", index);
                }
                let payload = self.cached_chunk_payload(index as u32)?;
                let (id, data) = split_packet_payload(&payload)?;
                Ok(Some(Event::Packet {
                    time,
                    state: State::Play,
                    id,
                    data,
                }))
            }
            _ => {
                let name = action.kind().as_str().to_string();
                Ok(Some(Event::Custom {
                    time,
                    name,
                    data: action.into_data(),
                }))
            }
        }
    }
}

/// パケットペイロード (`VarInt packet id` + body) を分解する。
fn split_packet_payload(payload: &[u8]) -> anyhow::Result<(i32, Box<[u8]>)> {
    let mut cursor = Cursor::new(payload);
    let id = cursor.read_varint()?;
    let body_start = cursor.position() as usize;
    Ok((id, payload[body_start..].into()))
}

impl<R: ArchiveReader> EventSource for FlashbackEventSource<R> {
    fn info(&self) -> &ReplayInfo {
        &self.info
    }
    fn next_event(&mut self) -> anyhow::Result<Option<Event>> {
        loop {
            let Some(action) = self.next_action()? else {
                return Ok(None);
            };
            if let Some(event) = self.action_to_event(action)? {
                return Ok(Some(event));
            }
        }
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

/// 論理イベント列を Flashback リプレイとして書き出す Sink。
///
/// 時刻の進行は tick 差分から `NextTick` action を合成して表現する
/// (ms → tick は切り捨て)。イベントのマッピング:
/// - `Packet` (Play) → `GamePacket`、(Configuration) →
///   `ConfigurationPacket`。チャンクパケットも dedup せず
///   `GamePacket` としてインラインに書く
/// - `Packet` (Login / Handshaking / Status) → 対応する action が
///   無いためスキップ ([`Self::skipped_packets`])
/// - `Custom` → 既知の flashback action 名ならそのまま書き戻す
///   (flashback → flashback で move_entities 等がロスレスに残る)。
///   未知名は action テーブルがヘッダ先書きのため登録できず
///   スキップ ([`Self::skipped_customs`])
///
/// 出力は空 snapshot の `c0.flashback` 1 本 + `metadata.json`。
/// mcpr 由来では data_version が判明しないため 0 を書く
/// (Flashback mod 側での再生可否は data_version に依存しうる)。
pub struct FlashbackEventSink<W: ArchiveWriter> {
    archive: W,
    chunk: Option<ChunkWriter<Vec<u8>>>,
    tick: u64,
    uuid: uuid::Uuid,
    skipped_packets: usize,
    skipped_customs: usize,
    finished: bool,
}

impl<W: ArchiveWriter> FlashbackEventSink<W> {
    pub fn new(archive: W) -> anyhow::Result<Self> {
        Self::with_uuid(archive, uuid::Uuid::new_v4())
    }
    /// metadata.json に書くリプレイ uuid を指定する。
    pub fn with_uuid(archive: W, uuid: uuid::Uuid) -> anyhow::Result<Self> {
        let chunk = ChunkWriter::new(Vec::new(), &ActionKind::KNOWN, &[])?;
        Ok(Self {
            archive,
            chunk: Some(chunk),
            tick: 0,
            uuid,
            skipped_packets: 0,
            skipped_customs: 0,
            finished: false,
        })
    }
    /// 対応 action が無くスキップした非 Play/Configuration パケット数。
    pub fn skipped_packets(&self) -> usize {
        self.skipped_packets
    }
    /// 未知名のためスキップした Custom イベント数。
    pub fn skipped_customs(&self) -> usize {
        self.skipped_customs
    }
    pub fn into_archive(self) -> W {
        self.archive
    }

    /// `target` tick まで `NextTick` を合成する。過去の時刻は現 tick に丸める。
    fn advance_tick(&mut self, target: u64) -> anyhow::Result<()> {
        let chunk = self.chunk.as_mut().expect("not finished");
        while self.tick < target {
            chunk.push(&Action::new(ActionKind::NextTick, Box::new([])))?;
            self.tick += 1;
        }
        Ok(())
    }
}

impl<W: ArchiveWriter> EventSink for FlashbackEventSink<W> {
    fn push(&mut self, event: Event) -> anyhow::Result<()> {
        match event {
            Event::Packet {
                time,
                state,
                id,
                data,
            } => {
                let kind = match state {
                    State::Play => ActionKind::GamePacket,
                    State::Configuration => ActionKind::ConfigurationPacket,
                    _ => {
                        self.skipped_packets += 1;
                        return Ok(());
                    }
                };
                self.advance_tick(time.as_ticks())?;
                let mut payload = Vec::with_capacity(data.len() + 5);
                payload.write_varint(id)?;
                payload.extend_from_slice(&data);
                self.chunk
                    .as_mut()
                    .expect("not finished")
                    .push(&Action::new(kind, payload.into()))?;
            }
            Event::Custom { time, name, data } => {
                let kind = ActionKind::parse(&name);
                if matches!(kind, ActionKind::Unknown(_)) {
                    self.skipped_customs += 1;
                    return Ok(());
                }
                self.advance_tick(time.as_ticks())?;
                self.chunk
                    .as_mut()
                    .expect("not finished")
                    .push(&Action::new(kind, data))?;
            }
        }
        Ok(())
    }
    fn finish(&mut self, info: &ReplayInfo) -> anyhow::Result<()> {
        if self.finished {
            anyhow::bail!("FlashbackEventSink::finish called twice");
        }
        self.finished = true;

        // 末尾まで時間を進める (duration が tick 数の根拠になる)
        let total_ticks = self
            .tick
            .max(Time::from_millis(info.duration_ms).as_ticks());
        self.advance_tick(total_ticks)?;

        let bytes = self.chunk.take().expect("not finished").finish()?;
        {
            let mut writer = self.archive.get_writer("c0.flashback")?;
            writer.write_all(&bytes)?;
            writer.flush()?;
        }

        let metadata = MetaData {
            uuid: self.uuid,
            name: "Unnamed".to_string(),
            version_string: info.mc_version.clone(),
            world_name: Some("World".to_string()),
            data_version: info.data_version.unwrap_or(0),
            protocol_version: info.protocol_version,
            total_ticks,
            markers: Some(serde_json::json!({})),
            chunks: BTreeMap::from([(
                "c0.flashback".to_string(),
                ChunkMeta {
                    duration: total_ticks,
                    force_play_snapshot: false,
                },
            )]),
        };
        let writer = BufWriter::new(self.archive.get_writer("metadata.json")?);
        serde_json::to_writer(writer, &metadata)?;
        Ok(())
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

    #[test]
    fn chunks_in_order_sorts_numerically() {
        let meta_json = serde_json::json!({
            "uuid": "e6ceb512-c347-474b-af6b-a96ba3ac946b",
            "name": "n",
            "version_string": "1.21.11",
            "world_name": null,
            "data_version": 4671,
            "protocol_version": 774,
            "total_ticks": 0,
            "markers": null,
            "chunks": {
                "c10.flashback": {"duration": 0},
                "c2.flashback": {"duration": 0},
                "c0.flashback": {"duration": 0},
            }
        });
        let meta: MetaData = serde_json::from_value(meta_json).unwrap();
        assert_eq!(
            meta.chunks_in_order(),
            vec!["c0.flashback", "c2.flashback", "c10.flashback"]
        );
    }

    /// テスト用のメモリ上アーカイブ。
    #[derive(Default)]
    struct MemArchive(HashMap<String, Vec<u8>>);
    impl ArchiveReader for MemArchive {
        fn get_reader<'this>(
            &'this mut self,
            filename: &str,
        ) -> anyhow::Result<Box<dyn Read + 'this>> {
            let data = self
                .0
                .get(filename)
                .ok_or_else(|| anyhow::anyhow!("no such file: {}", filename))?;
            Ok(Box::new(Cursor::new(data.clone())))
        }
    }
    impl ArchiveWriter for MemArchive {
        fn get_writer<'this>(
            &'this mut self,
            filename: &str,
        ) -> anyhow::Result<Box<dyn Write + 'this>> {
            Ok(Box::new(self.0.entry(filename.to_string()).or_default()))
        }
    }

    /// パケットペイロード (VarInt id + body) を組み立てる。
    fn payload(id: i32, body: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.write_varint(id).unwrap();
        buf.extend_from_slice(body);
        buf
    }

    /// `i32 BE size` + データの連結で level_chunk_cache ファイルを作る。
    fn cache_file(entries: &[Vec<u8>]) -> Vec<u8> {
        let mut buf = Vec::new();
        for entry in entries {
            buf.extend_from_slice(&(entry.len() as i32).to_be_bytes());
            buf.extend_from_slice(entry);
        }
        buf
    }

    fn test_metadata_json() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "uuid": "e6ceb512-c347-474b-af6b-a96ba3ac946b",
            "name": "test",
            "version_string": "1.21.11",
            "world_name": "World",
            "data_version": 4671,
            "protocol_version": 774,
            "total_ticks": 3,
            "markers": {},
            "chunks": { "c0.flashback": {"duration": 3, "forcePlaySnapshot": false} }
        }))
        .unwrap()
    }

    fn build_test_archive(snapshot: &[u8]) -> MemArchive {
        let registry = vec![
            ActionKind::NextTick,
            ActionKind::GamePacket,
            ActionKind::ConfigurationPacket,
            ActionKind::LevelChunkCached,
            ActionKind::MoveEntities,
        ];
        let mut chunk = Vec::new();
        {
            let mut w = ChunkWriter::new(&mut chunk, &registry, snapshot).unwrap();
            // tick 0: configuration + game packet
            w.push(&Action::new(
                ActionKind::ConfigurationPacket,
                payload(0x07, &[7]).into(),
            ))
            .unwrap();
            w.push(&Action::new(
                ActionKind::GamePacket,
                payload(0x2b, &[1, 2]).into(),
            ))
            .unwrap();
            // tick 1: cache 参照 (index 1) + 独自 action
            w.push(&Action::new(ActionKind::NextTick, Box::new([]))).unwrap();
            w.push(&Action::new(
                ActionKind::LevelChunkCached,
                {
                    let mut buf = Vec::new();
                    buf.write_varint(1).unwrap();
                    buf.into()
                },
            ))
            .unwrap();
            w.push(&Action::new(
                ActionKind::MoveEntities,
                vec![9, 9].into(),
            ))
            .unwrap();
            // tick 2: game packet
            w.push(&Action::new(ActionKind::NextTick, Box::new([]))).unwrap();
            w.push(&Action::new(
                ActionKind::GamePacket,
                payload(0x60, &[6]).into(),
            ))
            .unwrap();
            w.finish().unwrap();
        }
        let cache = cache_file(&[payload(0x2c, &[0xAA]), payload(0x2c, &[0xBB, 0xCC])]);

        let mut files = HashMap::new();
        files.insert("metadata.json".to_string(), test_metadata_json());
        files.insert("c0.flashback".to_string(), chunk);
        files.insert("level_chunk_caches/0".to_string(), cache);
        MemArchive(files)
    }

    fn collect_events<R: ArchiveReader>(
        mut source: FlashbackEventSource<R>,
    ) -> Vec<Event> {
        let mut events = Vec::new();
        while let Some(event) = source.next_event().unwrap() {
            events.push(event);
        }
        events
    }

    #[test]
    fn event_source_normalizes_ticks_and_resolves_cache() {
        let archive = build_test_archive(&[]);
        let source = FlashbackReader::new(archive).event_source(false).unwrap();
        assert_eq!(source.info().protocol_version, 774);
        assert_eq!(source.info().duration_ms, 150);
        let events = collect_events(source);

        assert_eq!(
            events,
            vec![
                Event::Packet {
                    time: Time::from_ticks(0),
                    state: State::Configuration,
                    id: 0x07,
                    data: vec![7].into(),
                },
                Event::Packet {
                    time: Time::from_ticks(0),
                    state: State::Play,
                    id: 0x2b,
                    data: vec![1, 2].into(),
                },
                // LevelChunkCached index=1 → cache の 2 番目のエントリに展開
                Event::Packet {
                    time: Time::from_ticks(1),
                    state: State::Play,
                    id: 0x2c,
                    data: vec![0xBB, 0xCC].into(),
                },
                Event::Custom {
                    time: Time::from_ticks(1),
                    name: "flashback:action/move_entities".to_string(),
                    data: vec![9, 9].into(),
                },
                Event::Packet {
                    time: Time::from_ticks(2),
                    state: State::Play,
                    id: 0x60,
                    data: vec![6].into(),
                },
            ]
        );
    }

    #[test]
    fn event_source_streams_snapshot_first() {
        // snapshot: ConfigurationPacket 1 件 (chunk と同じ action 列表現)
        let registry = vec![
            ActionKind::NextTick,
            ActionKind::GamePacket,
            ActionKind::ConfigurationPacket,
            ActionKind::LevelChunkCached,
            ActionKind::MoveEntities,
        ];
        let snapshot = {
            // ヘッダなしの action 列だけを作るため、一時 chunk から切り出す
            let mut buf = Vec::new();
            let mut w = ChunkWriter::new(&mut buf, &registry, &[]).unwrap();
            w.push(&Action::new(
                ActionKind::ConfigurationPacket,
                payload(0x05, &[5, 5]).into(),
            ))
            .unwrap();
            w.finish().unwrap();
            // ヘッダ部 (magic + registry + snapshot size 0) を読み飛ばして本体だけ取る
            let mut cursor = Cursor::new(buf.as_slice());
            ChunkReader::new(&mut cursor).unwrap();
            let pos = buf.len() - {
                let mut rest = Vec::new();
                cursor.read_to_end(&mut rest).unwrap();
                rest.len()
            };
            buf[pos..].to_vec()
        };

        let archive = build_test_archive(&snapshot);
        let with_snapshot =
            collect_events(FlashbackReader::new(archive).event_source(true).unwrap());
        assert_eq!(
            with_snapshot[0],
            Event::Packet {
                time: Time::from_ticks(0),
                state: State::Configuration,
                id: 0x05,
                data: vec![5, 5].into(),
            }
        );
        // snapshot の 1 件分だけ増える
        let archive = build_test_archive(&snapshot);
        let without_snapshot =
            collect_events(FlashbackReader::new(archive).event_source(false).unwrap());
        assert_eq!(with_snapshot.len(), without_snapshot.len() + 1);
        assert_eq!(&with_snapshot[1..], &without_snapshot[..]);
    }

    #[test]
    fn event_source_falls_back_to_legacy_cache_file() {
        let mut archive = build_test_archive(&[]);
        let cache = archive.0.remove("level_chunk_caches/0").unwrap();
        archive.0.insert("level_chunk_cache".to_string(), cache);
        let events = collect_events(FlashbackReader::new(archive).event_source(false).unwrap());
        assert!(events.iter().any(|e| matches!(
            e,
            Event::Packet { id: 0x2c, data, .. } if data.as_ref() == [0xBB, 0xCC]
        )));
    }

    #[test]
    fn event_sink_roundtrips_through_event_source() {
        let events = vec![
            // Login パケットは flashback に対応物が無くスキップされる
            Event::Packet {
                time: Time::from_ticks(0),
                state: State::Login,
                id: 0x02,
                data: vec![0].into(),
            },
            Event::Packet {
                time: Time::from_ticks(0),
                state: State::Configuration,
                id: 0x07,
                data: vec![7].into(),
            },
            Event::Packet {
                time: Time::from_ticks(0),
                state: State::Play,
                id: 0x2b,
                data: vec![1, 2].into(),
            },
            Event::Custom {
                time: Time::from_ticks(2),
                name: "flashback:action/move_entities".to_string(),
                data: vec![9, 9].into(),
            },
            // 未知の custom はスキップされる
            Event::Custom {
                time: Time::from_ticks(2),
                name: "thirdparty:action/foo".to_string(),
                data: vec![1].into(),
            },
            Event::Packet {
                time: Time::from_ticks(5),
                state: State::Play,
                id: 0x60,
                data: vec![6].into(),
            },
        ];

        let mut sink =
            FlashbackEventSink::with_uuid(MemArchive::default(), uuid::Uuid::nil()).unwrap();
        for event in events.clone() {
            sink.push(event).unwrap();
        }
        let info = ReplayInfo {
            mc_version: "1.21.11".to_string(),
            protocol_version: 774,
            duration_ms: 250,
            data_version: Some(4671),
            players: Default::default(),
        };
        sink.finish(&info).unwrap();
        assert_eq!(sink.skipped_packets(), 1);
        assert_eq!(sink.skipped_customs(), 1);
        let archive = sink.into_archive();

        // metadata の検証
        let metadata: MetaData = serde_json::from_slice(&archive.0["metadata.json"]).unwrap();
        assert_eq!(metadata.protocol_version, 774);
        assert_eq!(metadata.data_version, 4671);
        assert_eq!(metadata.total_ticks, 5);
        assert_eq!(metadata.chunks["c0.flashback"].duration, 5);

        // 読み戻し: Login とunknown custom が落ち、残りが tick 時刻で一致
        let read = collect_events(FlashbackReader::new(archive).event_source(false).unwrap());
        let expected: Vec<Event> = events
            .into_iter()
            .filter(|e| match e {
                Event::Packet { state, .. } => {
                    matches!(state, State::Play | State::Configuration)
                }
                Event::Custom { name, .. } => name.starts_with("flashback:"),
            })
            .collect();
        assert_eq!(read, expected);
    }

    #[test]
    fn event_sink_synthesizes_next_ticks() {
        let mut sink =
            FlashbackEventSink::with_uuid(MemArchive::default(), uuid::Uuid::nil()).unwrap();
        // 130ms → tick 2 (切り捨て)
        sink.push(Event::Packet {
            time: Time::from_millis(130),
            state: State::Play,
            id: 0x10,
            data: Box::new([]),
        })
        .unwrap();
        sink.finish(&ReplayInfo::default()).unwrap();
        let mut archive = sink.into_archive();

        let chunk_bytes = archive.0["c0.flashback"].clone();
        let reader = ChunkReader::new(Cursor::new(chunk_bytes)).unwrap();
        let kinds: Vec<ActionKind> = reader.map(|a| a.kind().clone()).collect();
        assert_eq!(
            kinds,
            vec![
                ActionKind::NextTick,
                ActionKind::NextTick,
                ActionKind::GamePacket,
            ]
        );
        // ArchiveReader としても読めることを確認 (Read+Write 両 impl)
        let _ = archive.get_reader("c0.flashback").unwrap();
    }
}
