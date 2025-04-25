use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufReader, BufWriter},
    sync::{Arc, Mutex},
};

use mcpr_lib::mcpr::ReplayStream;

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
    input: Vec<String>,

    #[arg(short, long)]
    output: Option<String>,

    #[arg(short, long)]
    exclude_packets: Vec<String>,

    #[arg(short, long)]
    include_packets: Vec<String>,

    #[arg(short, long, default_value_t = false)]
    packet_details: bool,

    #[arg(short, long, default_value_t = true)]
    unknow_packet: bool,

    #[arg(short, long, default_value_t = 9)]
    compression_level: i64,

    #[arg(short, long, default_value_t = 0)]
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

fn main() {
    let args = Args::parse();

    println!("input: {:?}", args.input);
    println!("output: {:?}", args.output);

    println!("compression level: {:?}", args.compression_level);

    let mut stream_config = ReplayStream::new(args.include_packets.is_empty(), args.unknow_packet);

    stream_config.include(args.include_packets().iter().copied());
    stream_config.exclude(args.exclude_packets().iter().copied());

    stream_config.interval(args.interval);

    stream_config.compression_level(args.compression_level);

    let mut readers: Vec<_> = args
        .input
        .iter()
        .map(|x| BufReader::new(File::open(x).unwrap()))
        .collect();

    let mut writer = args
        .output
        .map(|output| BufWriter::new(File::create(output).unwrap()));

    let details = if args.packet_details {
        Some((
            Arc::new(Mutex::new(BTreeMap::new())),
            Arc::new(Mutex::new(BTreeMap::new())),
        ))
    } else {
        None
    };
    stream_config
        .stream(&mut readers, &mut writer, |packet, writer| {
            if let Some(w) = writer {
                packet.write_to(w).unwrap();
            }
            if let Some((cnts, size)) = &details {
                *cnts.lock().unwrap().entry(packet.id()).or_insert(0u32) += 1;
                *size.lock().unwrap().entry(packet.id()).or_insert(0usize) += packet.data().len();
            }
            false
        })
        .unwrap();

    println!("Finished!");

    if let Some((cnts, size)) = &details {
        let mut table = vec![[
            "packet".to_string(),
            "count".to_string(),
            "total size".to_string(),
            "avg size".to_string(),
        ]];

        let mut cnts: Vec<_> = cnts.lock().unwrap().clone().into_iter().collect();
        cnts.sort_by_key(|&(a, b)| (b, a));
        cnts.reverse();

        let size = size.lock().unwrap();
        for (id, cnt) in cnts {
            let size = *size.get(&id).unwrap();
            table.push([
                format!("0x{:0x}", id),
                format!("{}", cnt),
                format!("{}", size),
                format!("{:.2}", size as f32 / cnt as f32),
            ]);
        }
        let mut table_size = [0usize; 4];
        for low in &table {
            for i in 0..4 {
                chmax!(table_size[i], low[i].len());
            }
        }
        for low in &table {
            print!("| {:<width$} ", low[0], width = table_size[0]);
            for i in 1..4 {
                print!("| {:>width$} ", low[i], width = table_size[i]);
            }
            println!("|");
        }
    }
}
