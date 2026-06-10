//! イベント層を介したクロスフォーマット変換の end-to-end 検証。
//! メモリ上の zip アーカイブを使い、fs 非依存で完結する。

use std::io::Cursor;

use mcpr_lib::{
    archive::zip::{ZipArchiveReader, ZipArchiveWriter},
    event::{Event, EventSink, EventSource, ReplayInfo, State, Time},
    flashback::{FlashbackEventSink, FlashbackReader},
    mcpr::{McprEventSink, ReplayReader},
};

fn play_packet(time_ms: u64, id: i32, data: &[u8]) -> Event {
    Event::Packet {
        time: Time::from_millis(time_ms),
        state: State::Play,
        id,
        data: data.into(),
    }
}

fn drain<S: EventSource>(source: &mut S) -> Vec<Event> {
    source.events().collect::<anyhow::Result<_>>().unwrap()
}

fn test_info() -> ReplayInfo {
    ReplayInfo {
        mc_version: "1.21.11".to_string(),
        protocol_version: 774,
        duration_ms: 1000,
        data_version: Some(4671),
        players: Default::default(),
    }
}

/// mcpr → flashback → mcpr。Play パケット列が tick 量子化を除いて保存される。
#[test]
fn mcpr_to_flashback_to_mcpr() {
    let source_events = vec![
        Event::Packet {
            time: Time::from_millis(0),
            state: State::Configuration,
            id: 0x07,
            data: vec![7].into(),
        },
        play_packet(0, 0x2b, &[1, 2]),
        play_packet(100, 0x2c, &[3]),
        play_packet(1000, 0x60, &[6, 6]),
    ];

    // → flashback (メモリ zip)
    let mut zip_buf = Cursor::new(Vec::new());
    {
        let archive = ZipArchiveWriter::new(&mut zip_buf, None);
        let mut sink = FlashbackEventSink::new(archive, uuid::Uuid::nil()).unwrap();
        for event in source_events.clone() {
            sink.push(event).unwrap();
        }
        sink.finish(&test_info()).unwrap();
    }

    // flashback として読み戻し → mcpr へ
    let archive = ZipArchiveReader::new(Cursor::new(zip_buf.into_inner())).unwrap();
    let mut source = FlashbackReader::new(archive).event_source(false).unwrap();
    assert_eq!(source.info().protocol_version, 774);
    assert_eq!(source.info().data_version, Some(4671));

    let mut mcpr_zip = Cursor::new(Vec::new());
    {
        let archive = ZipArchiveWriter::new(&mut mcpr_zip, None);
        let mut sink = McprEventSink::new(archive, source.info().protocol_version);
        let info = source.info().clone();
        while let Some(event) = source.next_event().unwrap() {
            sink.push(event).unwrap();
        }
        sink.finish(&info).unwrap();
    }

    // mcpr として読み戻し
    let archive = ZipArchiveReader::new(Cursor::new(mcpr_zip.into_inner())).unwrap();
    let mut reader = ReplayReader::new(archive);
    let mut source = reader.event_source().unwrap();
    assert_eq!(source.info().protocol_version, 774);
    assert_eq!(source.info().mc_version, "1.21.11");
    let events = drain(&mut source);

    // 合成された Login Success / Finish Configuration を除くと元のパケット列
    let replayed: Vec<&Event> = events
        .iter()
        .filter(|e| {
            !matches!(
                e,
                Event::Packet {
                    state: State::Login,
                    ..
                } | Event::Packet {
                    state: State::Configuration,
                    id: 0x03,
                    ..
                }
            )
        })
        .collect();
    assert_eq!(replayed.len(), source_events.len());
    for (got, want) in replayed.iter().zip(&source_events) {
        assert_eq!(*got, want, "packet must survive the roundtrip");
    }

    // 遷移構造の検証: Login Success → (config) → Finish Configuration → Play
    let sequence: Vec<(State, i32)> = events
        .iter()
        .map(|e| match e {
            Event::Packet { state, id, .. } => (*state, *id),
            _ => panic!("unexpected custom event"),
        })
        .collect();
    assert_eq!(sequence[0], (State::Login, 0x02));
    assert!(sequence.contains(&(State::Configuration, 0x03)));
}

/// flashback → mcpr で snapshot 由来の初期状態も含めて読める。
/// 実サンプル (tmp/flashback) 非在時は skip。
#[test]
fn real_flashback_to_mcpr() {
    let sample = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("tmp/flashback");
    if !sample.join("metadata.json").is_file() {
        eprintln!("skip: tmp/flashback sample not present");
        return;
    }
    use mcpr_lib::archive::directory::DirArchive;

    let mut source = FlashbackReader::new(DirArchive::new(&sample))
        .event_source(true)
        .unwrap();
    let info = source.info().clone();

    let mut mcpr_zip = Cursor::new(Vec::new());
    {
        let archive = ZipArchiveWriter::new(&mut mcpr_zip, None);
        let mut sink = McprEventSink::new(archive, info.protocol_version);
        while let Some(event) = source.next_event().unwrap() {
            sink.push(event).unwrap();
        }
        sink.finish(&info).unwrap();
        assert!(sink.skipped_custom() > 0, "sample has move_entities");
    }

    // 書いた .mcpr が構造的に正しく読み戻せる
    let archive = ZipArchiveReader::new(Cursor::new(mcpr_zip.into_inner())).unwrap();
    let mut reader = ReplayReader::new(archive);
    let metadata = reader.read_metadata().unwrap();
    assert_eq!(metadata.protocol, 774);
    assert_eq!(metadata.fileFormatVersion, 14);

    let mut source = reader.event_source().unwrap();
    let events = drain(&mut source);
    assert!(!events.is_empty());
    // 先頭は合成 Login Success、以降に configuration と play が続く
    assert!(matches!(
        &events[0],
        Event::Packet {
            state: State::Login,
            id: 0x02,
            ..
        }
    ));
    assert!(events.iter().any(|e| matches!(
        e,
        Event::Packet {
            state: State::Configuration,
            ..
        }
    )));
    assert!(events.iter().any(|e| matches!(
        e,
        Event::Packet {
            state: State::Play,
            ..
        }
    )));
}
