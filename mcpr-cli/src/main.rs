use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufReader, BufWriter},
    path::{Path, PathBuf},
};

use clap::Parser;
use mcpr_lib::{
    archive::{
        ArchiveReader, ArchiveWriter,
        directory::DirArchive,
        zip::{ZipArchiveReader, ZipArchiveWriter},
    },
    event::{
        Event, EventSink, EventSource, PlaybackSpeed, ReplayFormat, ReplayInfo, State, Time,
        detect_format, is_connection_init,
    },
    flashback::{FlashbackEventSink, FlashbackReader},
    mcpr::{McprEventSink, ReplayReader},
    protocol::parse_packet_id,
};

macro_rules! chmax {
    ($a:expr, $b:expr) => {
        if $a < $b {
            $a = $b;
            true
        } else {
            false
        }
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum OutputFormat {
    Mcpr,
    Flashback,
}

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    input: Vec<PathBuf>,

    #[arg(short, long)]
    output: Option<PathBuf>,

    /// 出力フォーマット
    #[arg(long, value_enum, default_value_t = OutputFormat::Mcpr)]
    output_format: OutputFormat,

    #[arg(long)]
    exclude_packets: Vec<String>,

    #[arg(long)]
    include_packets: Vec<String>,

    #[arg(short, long, default_value_t = false)]
    packet_details: bool,

    #[arg(long, default_value_t = true)]
    unknow_packet: bool,

    #[arg(short, long)]
    compression_level: Option<i64>,

    /// 入力リプレイ間に挿入する間隔 (ms)
    #[arg(long, default_value_t = 0)]
    interval: u32,

    /// 再生速度倍率 (2.0 = 2倍速, 0.5 = 半速)
    #[arg(long, default_value_t = PlaybackSpeed::NORMAL)]
    speed: PlaybackSpeed,

    /// flashback 入力で snapshot (初期状態の合成イベント) を読み飛ばす
    #[arg(long, default_value_t = false)]
    skip_snapshot: bool,
}

impl Args {
    fn include_packets(&self) -> Vec<u8> {
        Self::parse_packet_ids(&self.include_packets)
    }
    fn exclude_packets(&self) -> Vec<u8> {
        Self::parse_packet_ids(&self.exclude_packets)
    }
    fn parse_packet_ids(args: &[String]) -> Vec<u8> {
        args.iter()
            .map(|x| u8::try_from(parse_packet_id(x).expect("invalid packet id")).unwrap())
            .collect()
    }
}

/// 入力パスをアーカイブとして開き、中身からフォーマットを判別する。
fn detect_and_open(path: &Path) -> anyhow::Result<(ReplayFormat, Box<dyn ArchiveReader>)> {
    let mut archive: Box<dyn ArchiveReader> = if path.is_dir() {
        Box::new(DirArchive::new(path))
    } else {
        let reader = BufReader::new(File::open(path)?);
        Box::new(ZipArchiveReader::new(reader)?)
    };
    let format = detect_format(&mut archive).map_err(|e| anyhow::anyhow!("{}: {:?}", e, path))?;
    Ok((format, archive))
}

fn open_archive_writer(
    path: &Path,
    compression_level: Option<i64>,
) -> anyhow::Result<Box<dyn ArchiveWriter>> {
    if !path.exists()
        && path
            .extension()
            .is_none_or(|ext| ext != "mcpr" && ext != "zip")
    {
        fs::create_dir(path)?;
    }
    Ok(if path.is_dir() {
        Box::new(DirArchive::new(path))
    } else {
        let writer = BufWriter::new(File::create(path)?);
        Box::new(ZipArchiveWriter::new(writer, compression_level))
    })
}

/// 出力フォーマットごとの Sink。スキップ件数の報告のため enum で持つ。
enum AnySink {
    Mcpr(McprEventSink<Box<dyn ArchiveWriter>>),
    Flashback(FlashbackEventSink<Box<dyn ArchiveWriter>>),
}

impl AnySink {
    fn create(output: &Path, args: &Args, info: &ReplayInfo) -> anyhow::Result<Self> {
        let archive = open_archive_writer(output, args.compression_level)?;
        Ok(match args.output_format {
            OutputFormat::Mcpr => AnySink::Mcpr(McprEventSink::new(archive, info.protocol_version)),
            OutputFormat::Flashback => {
                AnySink::Flashback(FlashbackEventSink::new(archive, uuid::Uuid::new_v4())?)
            }
        })
    }
    /// [`EventSink`] としての本体 (report のみ具象型が要る)。
    fn as_sink(&mut self) -> &mut dyn EventSink {
        match self {
            AnySink::Mcpr(sink) => sink,
            AnySink::Flashback(sink) => sink,
        }
    }
    fn report(&self) {
        match self {
            AnySink::Mcpr(sink) => {
                if sink.skipped_custom() > 0 {
                    eprintln!(
                        "note: {} custom events have no .mcpr representation and were dropped",
                        sink.skipped_custom()
                    );
                }
            }
            AnySink::Flashback(sink) => {
                if sink.skipped_packets() > 0 {
                    eprintln!(
                        "note: {} non-play/configuration packets were dropped",
                        sink.skipped_packets()
                    );
                }
                if sink.skipped_customs() > 0 {
                    eprintln!(
                        "note: {} unknown custom events were dropped",
                        sink.skipped_customs()
                    );
                }
            }
        }
    }
}

struct Stats {
    counts: [usize; 256],
    sizes: [usize; 256],
    customs: BTreeMap<String, (usize, usize)>,
}

impl Default for Stats {
    fn default() -> Self {
        Self {
            counts: [0; 256],
            sizes: [0; 256],
            customs: BTreeMap::new(),
        }
    }
}

impl Stats {
    fn record(&mut self, event: &Event) {
        match event {
            Event::Packet { id, data, .. } => {
                if (0..256).contains(id) {
                    self.counts[*id as usize] += 1;
                    self.sizes[*id as usize] += data.len();
                }
            }
            Event::Custom { name, data, .. } => {
                // ホットパスでの name clone を避ける (キーは数種類しかない)
                let entry = match self.customs.get_mut(name.as_str()) {
                    Some(entry) => entry,
                    None => self.customs.entry(name.clone()).or_default(),
                };
                entry.0 += 1;
                entry.1 += data.len();
            }
        }
    }

    fn print(&self) {
        let mut table = vec![[
            "packet".to_string(),
            "count".to_string(),
            "total size".to_string(),
            "avg size".to_string(),
        ]];
        for id in 0..256 {
            let count = self.counts[id];
            let size = self.sizes[id];
            if count == 0 {
                continue;
            }
            table.push([
                format!("  \x1b[38;5;{0}m0x{0:<02x}\x1b[m", id),
                format!("{}", count),
                format!("{}", size),
                format!("{:.2}", size as f32 / count as f32),
            ]);
        }
        let mut table_size = [0usize; 4];
        for row in &table {
            for i in 1..4 {
                chmax!(table_size[i], row[i].len());
            }
        }
        table_size[0] = table[0].len();
        for (i, row) in table.iter().enumerate() {
            print!("{:>3} | {} ", i, row[0]);
            for j in 1..4 {
                print!("| {:>width$} ", row[j], width = table_size[j]);
            }
            println!();
        }
        if !self.customs.is_empty() {
            println!("custom events:");
            let name_width = self.customs.keys().map(|s| s.len()).max().unwrap_or(0);
            for (name, (count, size)) in &self.customs {
                println!(
                    "  {:<width$} count={:>6} size={:>10}",
                    name,
                    count,
                    size,
                    width = name_width
                );
            }
        }
    }
}

/// 1 入力分のイベントを共通パイプラインへ流す。
fn process<S: EventSource>(
    source: &mut S,
    args: &Args,
    is_first_input: bool,
    offset_ms: u64,
    play_filter: &[bool; 256],
    stats: &mut Option<Stats>,
    sink: &mut Option<AnySink>,
) -> anyhow::Result<ReplayInfo> {
    let info = source.info().clone();
    eprintln!(
        "  mc {} / protocol {} / duration {}ms",
        info.mc_version, info.protocol_version, info.duration_ms
    );

    if sink.is_none()
        && let Some(output) = &args.output
    {
        *sink = Some(AnySink::create(output, args, &info)?);
    }

    while let Some(mut event) = source.next_event()? {
        *event.time_mut() = Time::from_millis(
            args.speed
                .scale_millis(event.time().as_millis())
                .saturating_add(offset_ms),
        );

        if let Event::Packet { state, id, .. } = &event {
            // Play パケットの include/exclude フィルタ
            if *state == State::Play {
                let keep = if (0..256).contains(id) {
                    play_filter[*id as usize]
                } else {
                    args.unknow_packet
                };
                if !keep {
                    continue;
                }
            }
            // 2 個目以降の入力では接続初期化の重複を避ける
            if !is_first_input && is_connection_init(*state, *id) {
                continue;
            }
        }

        if let Some(stats) = stats {
            stats.record(&event);
        }
        if let Some(sink) = sink {
            sink.as_sink().push(event)?;
        }
    }
    Ok(info)
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    eprintln!("{:#?}", args);

    anyhow::ensure!(
        !args.input.is_empty(),
        "At least one input file is required"
    );

    let mut play_filter = [args.include_packets.is_empty(); 256];
    for packet in args.include_packets() {
        play_filter[packet as usize] = true;
    }
    for packet in args.exclude_packets() {
        play_filter[packet as usize] = false;
    }

    let mut stats = args.packet_details.then(Stats::default);
    let mut sink: Option<AnySink> = None;
    let mut players = BTreeSet::new();
    let mut merged_info: Option<ReplayInfo> = None;
    let mut offset_ms = 0u64;

    for (index, input) in args.input.iter().enumerate() {
        eprintln!();
        let (format, archive) = detect_and_open(input)?;
        eprintln!("[{}] {:?} ({})", index, input, format.name());

        // McprEventSource は reader を借用するため、match の外で生かす
        let mut mcpr_reader;
        let mut source: Box<dyn EventSource + '_> = match format {
            ReplayFormat::Flashback => {
                Box::new(FlashbackReader::new(archive).event_source(!args.skip_snapshot)?)
            }
            ReplayFormat::ReplayMod => {
                mcpr_reader = ReplayReader::new(archive);
                Box::new(mcpr_reader.event_source()?)
            }
        };
        let info = process(
            &mut source,
            &args,
            index == 0,
            offset_ms,
            &play_filter,
            &mut stats,
            &mut sink,
        )?;

        players.extend(info.players.iter().cloned());
        offset_ms += args.speed.scale_millis(info.duration_ms) + args.interval as u64;
        merged_info.get_or_insert(info);
    }

    if let Some(mut sink) = sink {
        let base = merged_info.expect("at least one input was processed");
        let info = ReplayInfo {
            duration_ms: offset_ms.saturating_sub(args.interval as u64),
            players,
            ..base
        };
        sink.as_sink().finish(&info)?;
        sink.report();
    }

    println!("Finished!");

    if let Some(stats) = &stats {
        stats.print();
    }
    Ok(())
}
