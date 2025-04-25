use clap::Parser;

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    input_files: Vec<String>,
    
    #[arg(short, long)]
    output_file: String,

    #[arg(short, long)]
    exclude_packets: Vec<String>,

    #[arg(short, long, default_value_t = false)]
    packet_details: bool,
}

use std::collections::BTreeMap;
use std::env;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Cursor, Read, Write};

use mcpr_lib::packet_decoder::Deserializer;

fn write_packet<W: Write>(writer: &mut W, time: u32, data: &[u8]) -> io::Result<()> {
    let length = data.len() as u32;
    writer.write_all(&time.to_be_bytes())?;
    writer.write_all(&length.to_be_bytes())?;
    writer.write_all(data)?;
    Ok(())
}

fn read_packet<R: Read>(reader: &mut R) -> io::Result<Option<(u32, Vec<u8>)>> {
    let mut header = [0u8; 8];
    match reader.read_exact(&mut header) {
        Ok(()) => {
            let time = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
            let length = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
            let mut data = vec![0u8; length as usize];
            reader.read_exact(&mut data)?;
            Ok(Some((time, data)))
        }
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
        Err(e) => Err(e),
    }
}

fn convert_packet<R: Read>(reader: &mut R) -> io::Result<Option<(u32, Vec<u8>)>> {
    if let Some((time, data)) = read_packet(reader)? {
        Ok(Some((time, data)))
    } else {
        Ok(None)
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();

    let mut input_file = BufReader::new(File::open(&args[1]).unwrap());
    // let output_file = File::create(&args[2]).unwrap();
    // let mut writer = BufWriter::new(output_file);
    
    let mut total = 0u64;

    let mut set = BTreeMap::new();
    
    while let Some((time, data)) = convert_packet(&mut input_file).unwrap() {
        total += 1;

        let mut reader = Cursor::new(&data);

        let packet_id = reader.read_varint().unwrap();

        /*
        if packet_id == 0x3A {
            {
                let global_index = reader.read_varint().unwrap();
                let sender = reader.read_uuid().unwrap();
                let index = reader.read_varint().unwrap();
                let msg_sign = if reader.read_bool().unwrap() {
                    let mut sign = vec![0; 256];
                    let _ = reader.read_exact(&mut sign).unwrap();
                    Some(sign)
                } else {
                    None
                };
            }
            /*
            {
                let message = reader.read_string();
                println!("{:?}", message);
            }
            */
            println!("{time}");
            while let Ok(c) = reader.read_unsigned_byte() {
                print!("{}", char::from_u32(c as u32).unwrap());
            }
            println!();
        }
        // println!("{time} {:?}", packet_id);
        // let _ = write_packet(&mut writer, time, &data).unwrap();
        // println!("{}", data.len() as i32 - length);
        */
        *set.entry(packet_id).or_insert(0) += 1;
    }
    println!("total: {}", total);
    println!("{}", set.len());
    for (k, v) in set {
        println!("{:0x} : {}", k, v);
    }
}


