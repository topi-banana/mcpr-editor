use std::{
    collections::HashSet,
    fs::{self, File},
    io::{BufReader, BufWriter},
    path::Path,
};

use anyhow::Ok;
use mcpr_lib::{
    archive::{
        ArchiveReader, ArchiveWriter,
        directory::DirArchive,
        zip::{ZipArchiveReader, ZipArchiveWriter},
    },
    mcpr::{MetaData, ReplayReader, ReplayWriter, State},
};

use clap::Parser;

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

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    input: Vec<std::path::PathBuf>,

    #[arg(short, long)]
    output: Option<std::path::PathBuf>,

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

    #[arg(long, default_value_t = 0)]
    interval: u32,
}
impl Args {
    fn include_packets(&self) -> Vec<u8> {
        self.include_packets
            .iter()
            .map(|x| u8::from_str_radix(x.trim_start_matches("0x"), 16).unwrap())
            .collect()
    }
    fn exclude_packets(&self) -> Vec<u8> {
        self.exclude_packets
            .iter()
            .map(|x| u8::from_str_radix(x.trim_start_matches("0x"), 16).unwrap())
            .collect()
    }
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    eprintln!("{:#?}", args);

    if args.input.is_empty() {
        panic!("At least one input file is required");
    }

    let mut packets = [args.include_packets.is_empty(); 256];
    for packet in args.include_packets() {
        packets[packet as usize] = true
    }
    for packet in args.exclude_packets() {
        packets[packet as usize] = false
    }

    let mut replay_writer = if let Some(output) = &args.output {
        let path = Path::new(output);
        if !path.exists() {
            if let Some(ext) = path.extension() {
                if ext != "mcpr" {
                    fs::create_dir(path)?;
                }
            }
        }
        let archive: Box<dyn ArchiveWriter> = if path.is_dir() {
            Box::new(DirArchive::new(path))
        } else {
            let writer = BufWriter::new(File::create(path)?);
            Box::new(ZipArchiveWriter::new(writer, args.compression_level))
        };
        Some(ReplayWriter::new(archive))
    } else {
        None
    };

    let mut details = if args.packet_details {
        Some(([0; 256], [0; 256]))
    } else {
        None
    };

    let mut players = HashSet::new();
    let mut offset = 0u64;
    {
        let mut writable_replay = replay_writer
            .as_mut()
            .map(|e| e.get_packet_writer().unwrap());

        for i in 0..args.input.len() {
            eprintln!();
            let path = Path::new(&args.input[i]);
            let reader: Box<dyn ArchiveReader> = if path.is_dir() {
                Box::new(DirArchive::new(path))
            } else {
                let reader = BufReader::new(File::open(path).unwrap());
                Box::new(ZipArchiveReader::new(reader).unwrap())
            };
            let mut readable_replay = ReplayReader::new(reader);
            for (state, mut packet) in readable_replay.get_packet_reader().unwrap() {
                *packet.time_mut() += offset as u32;
                let q = if packet.id() < 0 || packet.id() >= 256 {
                    args.unknow_packet
                } else {
                    packets[packet.id() as usize]
                };
                if state == State::Play && !q {
                    continue;
                }
                if i != 0 {
                    if state == State::Play {
                        if packet.id() == 0x2b {
                            continue;
                        }
                    } else {
                        continue;
                    }
                }
                // eprintln!("{0} \x1b[38;5;{1}m0x{1:<02x}\x1b[m {:?}", packet.time(), packet.id(), packet.data());
                // eprintln!("{i} {0} \x1b[38;5;{1}m0x{1:<02x}\x1b[m {2:?} {3}", packet.time(), packet.id(), state, packet.data().len());
                if let Some((cnts, size)) = &mut details {
                    cnts[packet.id() as usize] += 1usize;
                    size[packet.id() as usize] += packet.data().len();
                }
                if let Some(writer) = &mut writable_replay {
                    writer.push(packet).unwrap();
                }
            }
            let metadata = readable_replay.read_metadata().unwrap();
            players.extend(metadata.players);
            offset += metadata.duration + args.interval as u64;
        }
    }
    let metadata = MetaData {
        singleplayer: false,
        customServerName: "NaN".to_string(),
        serverName: "NaN".to_string(),
        duration: offset,
        date: 0,
        mcversion: "1.21.1".to_string(),
        fileFormat: "MCPR".to_string(),
        fileFormatVersion: 14,
        protocol: 767,
        generator: "topi-banana/tmcpr-editor".to_string(),
        selfId: -1,
        players,
    };
    if let Some(writer) = &mut replay_writer {
        writer.write_metadata(metadata).unwrap();
    }

    println!("Finished!");

    if let Some((cnts, size)) = &mut details {
        let mut table = vec![[
            "packet".to_string(),
            "count".to_string(),
            "total size".to_string(),
            "avg size".to_string(),
        ]];
        for id in 0..256 {
            let cnt = cnts[id];
            let size = size[id];
            if cnt == 0 {
                continue;
            }
            table.push([
                format!("  \x1b[38;5;{0}m0x{0:<02x}\x1b[m", id),
                format!("{}", cnt),
                format!("{}", size),
                format!("{:.2}", size as f32 / cnt as f32),
            ]);
        }
        let mut table_size = [0usize; 4];
        for low in &table {
            for i in 1..4 {
                chmax!(table_size[i], low[i].len());
            }
        }
        table_size[0] = table[0].len();
        for (i, low) in table.iter().enumerate() {
            print!("{:>3} | {} ", i, low[0]);
            // print!("{i:>3} | {:<width$} ", low[0], width = table_size[0]);
            for j in 1..4 {
                print!("| {:>width$} ", low[j], width = table_size[j]);
            }
            println!();
        }
    }
    Ok(())
}
