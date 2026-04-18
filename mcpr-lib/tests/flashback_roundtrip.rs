//! 実 flashback サンプルを使った end-to-end round-trip 検証。
//! サンプルファイル非在時は skip する（CI の移植性のため）。

#![cfg(feature = "fs")]

use std::{
    fs::{self, File},
    io::{BufReader, BufWriter},
    path::PathBuf,
};

use mcpr_lib::{
    archive::{
        ArchiveReader, ArchiveWriter,
        directory::DirArchive,
        zip::{ZipArchiveReader, ZipArchiveWriter},
    },
    flashback::{FlashbackReader, FlashbackWriter},
};

fn sample_dir() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .join("tmp/flashback");
    if p.join("metadata.json").is_file() {
        Some(p)
    } else {
        None
    }
}

#[test]
fn dir_to_dir_roundtrip() {
    let Some(src) = sample_dir() else {
        eprintln!("skip: tmp/flashback sample not present");
        return;
    };
    let dst = std::env::temp_dir().join("mcpr_editor_flashback_roundtrip_dir");
    let _ = fs::remove_dir_all(&dst);
    fs::create_dir_all(&dst).unwrap();

    let mut reader = FlashbackReader::new(DirArchive::new(&src));
    let meta = reader.get_metadata().unwrap();

    let mut writer = FlashbackWriter::new(DirArchive::new(&dst));
    writer.write_metadata(&meta).unwrap();
    for name in meta.chunks.keys() {
        let src_chunk = reader.get_chunk_reader(name).unwrap();
        let actions = src_chunk.actions().to_vec();
        let snapshot = src_chunk.snapshot().to_vec();
        let mut dst_chunk = writer.get_chunk_writer(name, &actions, &snapshot).unwrap();
        for action in src_chunk {
            dst_chunk.push(&action).unwrap();
        }
        dst_chunk.finish().unwrap();
    }

    // read-back で一致確認
    let mut reader2 = FlashbackReader::new(DirArchive::new(&dst));
    let meta2 = reader2.get_metadata().unwrap();
    assert_eq!(meta.uuid, meta2.uuid);
    assert_eq!(meta.total_ticks, meta2.total_ticks);
    assert_eq!(meta.chunks.len(), meta2.chunks.len());

    for name in meta.chunks.keys() {
        let original = FlashbackReader::new(DirArchive::new(&src))
            .get_chunk_reader(name)
            .unwrap()
            .collect::<Vec<_>>();
        let copied = reader2.get_chunk_reader(name).unwrap().collect::<Vec<_>>();
        assert_eq!(original, copied);
    }
}

#[test]
fn dir_to_zip_roundtrip() {
    let Some(src) = sample_dir() else {
        eprintln!("skip: tmp/flashback sample not present");
        return;
    };
    let dst = std::env::temp_dir().join("mcpr_editor_flashback_roundtrip.zip");
    let _ = fs::remove_file(&dst);

    let mut reader = FlashbackReader::new(DirArchive::new(&src));
    let meta = reader.get_metadata().unwrap();

    {
        let zip_out = ZipArchiveWriter::new(BufWriter::new(File::create(&dst).unwrap()), None);
        let mut writer = FlashbackWriter::new(zip_out);
        writer.write_metadata(&meta).unwrap();
        for name in meta.chunks.keys() {
            let src_chunk = reader.get_chunk_reader(name).unwrap();
            let actions = src_chunk.actions().to_vec();
            let snapshot = src_chunk.snapshot().to_vec();
            let mut dst_chunk = writer.get_chunk_writer(name, &actions, &snapshot).unwrap();
            for action in src_chunk {
                dst_chunk.push(&action).unwrap();
            }
            dst_chunk.finish().unwrap();
        }
    }

    let zip_in = ZipArchiveReader::new(BufReader::new(File::open(&dst).unwrap())).unwrap();
    let mut reader2 = FlashbackReader::new(zip_in);
    let meta2 = reader2.get_metadata().unwrap();
    assert_eq!(meta.uuid, meta2.uuid);
    assert_eq!(meta.chunks.len(), meta2.chunks.len());

    for name in meta.chunks.keys() {
        let original = FlashbackReader::new(DirArchive::new(&src))
            .get_chunk_reader(name)
            .unwrap()
            .collect::<Vec<_>>();
        let copied = reader2.get_chunk_reader(name).unwrap().collect::<Vec<_>>();
        assert_eq!(original, copied);
    }
    let _ = fs::remove_file(&dst);
}

// 未使用クレート警告回避
#[allow(dead_code)]
fn _assert_archive_traits<A: ArchiveReader + ArchiveWriter>() {}
