//! 連結結果のリプレイ書き出し。mcpr-cli の連結パイプライン
//! (パケットフィルタ無指定時の process() / main()) を in-memory zip に
//! 対して再現し、ブラウザの Blob ダウンロードへ渡すバイト列を作る。
//!
//! wasm はメインスレッド実行のため、[`export_merged`] は async で
//! 一定イベント数ごとにブラウザへ制御を返し ([`yield_to_browser`])、
//! 進捗バーの再描画機会を作る。yield は native では即時完了するので、
//! `cargo test` では同期関数同様に CLI と同じ挙動 (時刻オフセット・
//! 接続初期化の除外・メタデータ合成) を検証できる。

use std::{collections::BTreeSet, io::Cursor};

use mcpr_lib::{
    archive::zip::{ZipArchiveReader, ZipArchiveWriter},
    event::{
        Event, EventSink, EventSource, ReplayFormat, ReplayInfo, Time, detect_format,
        is_connection_init,
    },
    flashback::{FlashbackEventSink, FlashbackReader},
    mcpr::{McprEventSink, ReplayReader},
};

use crate::merge::MergeRule;

/// 書き出し先の物理フォーマット (mcpr-cli の --output-format に対応)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExportFormat {
    #[default]
    Mcpr,
    Flashback,
}

impl ExportFormat {
    /// フォーマット選択 UI の表示順。
    pub const ORDER: [ExportFormat; 2] = [ExportFormat::Mcpr, ExportFormat::Flashback];

    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Mcpr => "mcpr",
            ExportFormat::Flashback => "zip",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ExportFormat::Mcpr => "ReplayMod (.mcpr)",
            ExportFormat::Flashback => "Flashback (.zip)",
        }
    }
}

/// [`export_merged`] の進捗通知。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportProgress {
    /// イベント処理中。`processed` は入力から読んだイベント数の累計
    /// (フィルタで落ちる行も含む)。総数は呼び出し側がパース済みの
    /// 表示行数から把握する前提。
    Events { processed: u64 },
    /// 終端処理 (zip 圧縮・メタデータ書き込み) 中。一括で走るため
    /// この間の進捗は刻めない。
    Finishing,
}

/// 進捗報告と yield を行うイベント数間隔。小さいほど進捗が滑らかだが、
/// ブラウザの setTimeout クランプ (~4ms) が積み重なって遅くなる。
const YIELD_EVERY_EVENTS: u64 = 32 * 1024;

/// ブラウザのイベントループへ制御を返し、進捗表示の再描画機会を作る。
/// native (テスト) では即時完了する。
async fn yield_to_browser() {
    #[cfg(target_arch = "wasm32")]
    gloo_timers::future::TimeoutFuture::new(0).await;
}

fn should_yield_progress(processed: u64) -> bool {
    processed == 1 || processed.is_multiple_of(YIELD_EVERY_EVENTS)
}

/// in-memory zip へ書く [`ZipArchiveWriter`]。
type MemZipWriter = ZipArchiveWriter<Cursor<Vec<u8>>>;

/// 出力フォーマットごとの Sink (mcpr-cli の AnySink 相当)。
enum ExportSink {
    Mcpr(McprEventSink<MemZipWriter>),
    Flashback(FlashbackEventSink<MemZipWriter>),
}

impl ExportSink {
    /// `info` は先頭入力のメタ情報 (CLI と同じく protocol_version の出所)。
    fn create(
        format: ExportFormat,
        info: &ReplayInfo,
        replay_uuid: uuid::Uuid,
    ) -> anyhow::Result<Self> {
        // 圧縮レベルは CLI のデフォルト (--compression-level 無指定) に合わせる。
        let archive = ZipArchiveWriter::new(Cursor::new(Vec::new()), None);
        Ok(match format {
            ExportFormat::Mcpr => {
                ExportSink::Mcpr(McprEventSink::new(archive, info.protocol_version))
            }
            ExportFormat::Flashback => {
                ExportSink::Flashback(FlashbackEventSink::new(archive, replay_uuid)?)
            }
        })
    }

    fn as_sink(&mut self) -> &mut dyn EventSink {
        match self {
            ExportSink::Mcpr(sink) => sink,
            ExportSink::Flashback(sink) => sink,
        }
    }

    /// 終端処理して zip バイト列を取り出す。
    fn finish_into_bytes(mut self, info: &ReplayInfo) -> anyhow::Result<Vec<u8>> {
        self.as_sink().finish(info)?;
        let archive = match self {
            ExportSink::Mcpr(sink) => sink.into_archive(),
            ExportSink::Flashback(sink) => sink.into_archive(),
        };
        Ok(archive.finish()?.into_inner())
    }
}

/// 順序付き入力 (リプレイ zip のバイト列) を連結して 1 本のリプレイ zip を
/// 生成する。連結規則は mcpr-cli と同一:
/// 時刻オフセット (各入力の duration + interval) を加算し、
/// [`MergeRule::CliCompatible`] では 2 個目以降の接続初期化パケット
/// ([`is_connection_init`]) を除外する。
///
/// `replay_uuid` は Flashback 出力の metadata に書くリプレイ uuid
/// (乱数生成は呼び出し側の責務。テストでは `Uuid::nil()` で決定的に)。
/// `on_progress` には最初のイベント、[`YIELD_EVERY_EVENTS`] ごと、
/// 各入力の処理完了時、および終端処理の開始時に進捗が届く。
///
/// 注意: [`MergeRule::OffsetOnly`] で複数入力を .mcpr へ書くと、2 個目の
/// Login パケットで state が後退するためエラーになる (CLI に対応する
/// 動作モードが存在しない)。
pub async fn export_merged(
    inputs: &[&[u8]],
    interval_ms: u64,
    rule: MergeRule,
    format: ExportFormat,
    replay_uuid: uuid::Uuid,
    mut on_progress: impl FnMut(ExportProgress),
) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(!inputs.is_empty(), "no input replays");

    let mut sink: Option<ExportSink> = None;
    let mut players = BTreeSet::new();
    let mut base_info: Option<ReplayInfo> = None;
    let mut offset_ms = 0u64;
    let mut processed = 0u64;

    // クリック直後の UI 状態を一度描画させてから重い再パースへ入る。
    yield_to_browser().await;

    for (index, bytes) in inputs.iter().enumerate() {
        let mut zip = ZipArchiveReader::new(Cursor::new(*bytes))?;
        // McprEventSource は reader を借用するため、match の外で生かす
        let mut mcpr_reader;
        let mut source: Box<dyn EventSource + '_> = match detect_format(&mut zip)? {
            ReplayFormat::ReplayMod => {
                mcpr_reader = ReplayReader::new(zip);
                Box::new(mcpr_reader.event_source()?)
            }
            // CLI のデフォルト (--skip-snapshot なし) と同じく snapshot 込み
            ReplayFormat::Flashback => Box::new(FlashbackReader::new(zip).event_source(true)?),
        };
        let info = source.info().clone();

        let sink = match &mut sink {
            Some(sink) => sink,
            None => sink.insert(ExportSink::create(format, &info, replay_uuid)?),
        };

        while let Some(mut event) = source.next_event()? {
            processed += 1;
            if should_yield_progress(processed) {
                on_progress(ExportProgress::Events { processed });
                yield_to_browser().await;
            }
            *event.time_mut() = Time::from_millis(event.time().as_millis() + offset_ms);
            // 2 個目以降の入力では接続初期化の重複を避ける (CLI の process と同じ規則)
            if rule == MergeRule::CliCompatible
                && index > 0
                && let Event::Packet { state, id, .. } = &event
                && is_connection_init(*state, *id)
            {
                continue;
            }
            sink.as_sink().push(event)?;
        }
        // 入力の境目でも報告する (小さいファイルでも進捗が動くように)。
        on_progress(ExportProgress::Events { processed });
        yield_to_browser().await;

        players.extend(info.players.iter().cloned());
        offset_ms += info.duration_ms + interval_ms;
        base_info.get_or_insert(info);
    }

    // 終端処理 (圧縮) は一括で走るため、開始前に表示を切り替えさせる。
    on_progress(ExportProgress::Finishing);
    yield_to_browser().await;

    let info = ReplayInfo {
        duration_ms: offset_ms.saturating_sub(interval_ms),
        players,
        // mc_version / protocol_version / data_version は先頭から継承
        ..base_info.expect("inputs is non-empty")
    };
    sink.expect("inputs is non-empty").finish_into_bytes(&info)
}

/// ダウンロードファイル名。単一入力は先頭ファイル名の拡張子差し替え、
/// 複数入力は連結を示す `_merged` を付ける。
pub fn export_filename(first_filename: &str, multi: bool, format: ExportFormat) -> String {
    let stem = first_filename
        .rsplit_once('.')
        .map_or(first_filename, |(stem, _)| stem);
    let stem = if stem.is_empty() { "replay" } else { stem };
    if multi {
        format!("{stem}_merged.{}", format.extension())
    } else {
        format!("{stem}.{}", format.extension())
    }
}

/// Flashback metadata 用のランダム (v4) uuid。
///
/// uuid crate の rng 系 feature は getrandom 0.4 を別途引き込み wasm
/// backend の二重設定が必要になるため、wasm_js backend 構成済みの
/// getrandom 0.3 から直接 16 bytes を取って組み立てる。
pub fn new_replay_uuid() -> uuid::Uuid {
    let mut bytes = [0u8; 16];
    // 乱数源が無い環境は実質想定外。失敗時は nil 由来の uuid で続行する。
    let _ = getrandom::fill(&mut bytes);
    uuid::Builder::from_random_bytes(bytes).into_uuid()
}

/// Blob ダウンロードをトリガする (ブラウザ専用、テスト対象外)。
pub fn trigger_download(bytes: &[u8], filename: &str) {
    use wasm_bindgen::JsCast;

    let blob = gloo_file::Blob::new_with_options(bytes, Some("application/zip"));
    let url = gloo_file::ObjectUrl::from(blob);
    let Some(document) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    let Ok(element) = document.create_element("a") else {
        return;
    };
    let anchor: web_sys::HtmlAnchorElement = element.unchecked_into();
    anchor.set_href(&url);
    anchor.set_download(filename);
    anchor.click();
    // ObjectUrl はここで drop され revoke されるが、click は同期に
    // ダウンロードを開始済みなので問題ない。
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcpr_lib::{
        event::State,
        protocol::{FINISH_CONFIGURATION_PACKET_ID, LOGIN_PLAY_PACKET_ID, LOGIN_SUCCESS_PACKET_ID},
    };

    /// native では [`yield_to_browser`] が即時完了するため、1 回の poll で
    /// 完了する future を同期実行する最小の executor。
    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        let mut future = std::pin::pin!(future);
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        match future.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(out) => out,
            std::task::Poll::Pending => unreachable!("native では yield は即時完了する"),
        }
    }

    /// 進捗を無視して [`export_merged`] を同期実行するテスト用ラッパ。
    fn export(
        inputs: &[&[u8]],
        interval_ms: u64,
        rule: MergeRule,
        format: ExportFormat,
        replay_uuid: uuid::Uuid,
    ) -> anyhow::Result<Vec<u8>> {
        block_on(export_merged(
            inputs,
            interval_ms,
            rule,
            format,
            replay_uuid,
            |_| {},
        ))
    }

    fn packet(time_ms: u64, state: State, id: i32, data: &[u8]) -> Event {
        Event::Packet {
            time: Time::from_millis(time_ms),
            state,
            id,
            data: data.into(),
        }
    }

    /// Login → Configuration → Play の正しい遷移を持つイベント列の雛形。
    /// `play_times` の各時刻に Play パケット (0x2c) を置く。
    fn full_stream(play_times: &[u64]) -> Vec<Event> {
        let mut events = vec![
            packet(0, State::Login, LOGIN_SUCCESS_PACKET_ID, &[0x01]),
            packet(0, State::Configuration, FINISH_CONFIGURATION_PACKET_ID, &[]),
        ];
        for &t in play_times {
            events.push(packet(t, State::Play, 0x2c, &[0xaa, 0xbb]));
        }
        events
    }

    fn info(duration_ms: u64, players: &[u128]) -> ReplayInfo {
        ReplayInfo {
            mc_version: "1.21.11".to_string(),
            protocol_version: 774,
            duration_ms,
            data_version: None,
            players: players.iter().map(|&n| uuid::Uuid::from_u128(n)).collect(),
        }
    }

    /// McprEventSink で in-memory の .mcpr フィクスチャを生成する。
    fn mcpr_fixture(events: Vec<Event>, info: &ReplayInfo) -> Vec<u8> {
        let archive = ZipArchiveWriter::new(Cursor::new(Vec::new()), None);
        let mut sink = McprEventSink::new(archive, info.protocol_version);
        for event in events {
            sink.push(event).unwrap();
        }
        sink.finish(info).unwrap();
        sink.into_archive().finish().unwrap().into_inner()
    }

    fn read_mcpr(bytes: &[u8]) -> (ReplayInfo, Vec<Event>) {
        let zip = ZipArchiveReader::new(Cursor::new(bytes)).unwrap();
        let mut reader = ReplayReader::new(zip);
        let mut source = reader.event_source().unwrap();
        let info = source.info().clone();
        let events = source.events().collect::<anyhow::Result<Vec<_>>>().unwrap();
        (info, events)
    }

    #[test]
    fn single_mcpr_roundtrips() {
        let events = full_stream(&[100, 250]);
        let input = mcpr_fixture(events.clone(), &info(1000, &[1, 2]));

        let out = export(
            &[&input],
            0,
            MergeRule::CliCompatible,
            ExportFormat::Mcpr,
            uuid::Uuid::nil(),
        )
        .unwrap();

        let (out_info, out_events) = read_mcpr(&out);
        assert_eq!(out_events, events);
        assert_eq!(out_info.mc_version, "1.21.11");
        assert_eq!(out_info.protocol_version, 774);
        assert_eq!(out_info.duration_ms, 1000);
        assert_eq!(out_info.players, info(0, &[1, 2]).players);
    }

    #[test]
    fn cli_compatible_merge_offsets_and_dedups() {
        let a = mcpr_fixture(full_stream(&[20]), &info(500, &[1]));
        let mut b_events = full_stream(&[30]);
        // 2 個目の Login(play) 0x2b が除外されることも見る
        b_events.push(packet(40, State::Play, LOGIN_PLAY_PACKET_ID, &[]));
        let b = mcpr_fixture(b_events, &info(300, &[2]));

        let out = export(
            &[&a, &b],
            1000,
            MergeRule::CliCompatible,
            ExportFormat::Mcpr,
            uuid::Uuid::nil(),
        )
        .unwrap();

        let (out_info, out_events) = read_mcpr(&out);
        // 1 個目はそのまま、2 個目は Play の通常パケットのみ (offset = 500 + 1000)
        let mut expected = full_stream(&[20]);
        expected.push(packet(1530, State::Play, 0x2c, &[0xaa, 0xbb]));
        assert_eq!(out_events, expected);
        // duration = d1 + interval + d2、players は union
        assert_eq!(out_info.duration_ms, 500 + 1000 + 300);
        assert_eq!(out_info.players, info(0, &[1, 2]).players);
    }

    #[test]
    fn offset_only_multi_input_mcpr_fails() {
        // OffsetOnly では 2 個目の Login パケットがそのまま流れ、
        // .mcpr の state が後退するためエラーになる (仕様)。
        let a = mcpr_fixture(full_stream(&[20]), &info(500, &[]));
        let b = mcpr_fixture(full_stream(&[30]), &info(300, &[]));

        let err = export(
            &[&a, &b],
            0,
            MergeRule::OffsetOnly,
            ExportFormat::Mcpr,
            uuid::Uuid::nil(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("transition"), "{err}");
    }

    #[test]
    fn flashback_output_writes_metadata_and_ticks() {
        let input = mcpr_fixture(full_stream(&[0, 50, 120]), &info(1000, &[]));

        let out = export(
            &[&input],
            0,
            MergeRule::CliCompatible,
            ExportFormat::Flashback,
            uuid::Uuid::nil(),
        )
        .unwrap();

        let mut zip = ZipArchiveReader::new(Cursor::new(out.as_slice())).unwrap();
        assert_eq!(detect_format(&mut zip).unwrap(), ReplayFormat::Flashback);

        let mut reader = FlashbackReader::new(zip);
        let metadata = reader.get_metadata().unwrap();
        assert_eq!(metadata.uuid, uuid::Uuid::nil());
        assert_eq!(metadata.protocol_version, 774);
        // duration 1000ms = 20 ticks
        assert_eq!(metadata.total_ticks, 20);

        let mut source = reader.event_source(true).unwrap();
        let mut events = Vec::new();
        while let Some(event) = source.next_event().unwrap() {
            events.push(event);
        }
        // Login パケットは Flashback に対応物がなく落ちる。
        // Configuration はそのまま、Play は tick (50ms) 単位へ丸まる。
        let expected = vec![
            packet(0, State::Configuration, FINISH_CONFIGURATION_PACKET_ID, &[]),
            packet(0, State::Play, 0x2c, &[0xaa, 0xbb]),
            packet(50, State::Play, 0x2c, &[0xaa, 0xbb]),
            packet(100, State::Play, 0x2c, &[0xaa, 0xbb]),
        ];
        assert_eq!(events, expected);
    }

    #[test]
    fn progress_is_reported() {
        // YIELD_EVERY_EVENTS を跨ぐイベント数で、開始報告 → チャンク報告
        // → 入力完了報告 → Finishing の順に届くことを見る。
        let play_times: Vec<u64> = (0..YIELD_EVERY_EVENTS).collect();
        let total = play_times.len() as u64 + 2; // + Login/Config の遷移 2 つ
        let input = mcpr_fixture(full_stream(&play_times), &info(1000, &[]));

        let mut progress = Vec::new();
        block_on(export_merged(
            &[&input],
            0,
            MergeRule::CliCompatible,
            ExportFormat::Mcpr,
            uuid::Uuid::nil(),
            |p| progress.push(p),
        ))
        .unwrap();

        let expected = vec![
            ExportProgress::Events { processed: 1 },
            ExportProgress::Events {
                processed: YIELD_EVERY_EVENTS,
            },
            ExportProgress::Events { processed: total },
            ExportProgress::Finishing,
        ];
        assert_eq!(progress, expected);
    }

    #[test]
    fn empty_inputs_fail() {
        assert!(
            export(
                &[],
                0,
                MergeRule::CliCompatible,
                ExportFormat::Mcpr,
                uuid::Uuid::nil()
            )
            .is_err()
        );
    }

    /// CLI バイト一致 E2E 用の一時 fixture 出力 (手動検証用、CI では走らない)。
    #[test]
    #[ignore]
    fn dump_e2e_fixtures() {
        let dir = std::path::Path::new("/tmp/mcpr-e2e");
        std::fs::create_dir_all(dir).unwrap();
        let a = mcpr_fixture(full_stream(&[20]), &info(500, &[1]));
        let b = mcpr_fixture(full_stream(&[30]), &info(300, &[2]));
        std::fs::write(dir.join("a.mcpr"), &a).unwrap();
        std::fs::write(dir.join("b.mcpr"), &b).unwrap();
        let out = export(
            &[&a, &b],
            1000,
            MergeRule::CliCompatible,
            ExportFormat::Mcpr,
            uuid::Uuid::nil(),
        )
        .unwrap();
        std::fs::write(dir.join("ui-out.mcpr"), &out).unwrap();
        let out_fb = export(
            &[&a, &b],
            1000,
            MergeRule::CliCompatible,
            ExportFormat::Flashback,
            uuid::Uuid::nil(),
        )
        .unwrap();
        std::fs::write(dir.join("ui-out-fb.zip"), &out_fb).unwrap();
    }

    #[test]
    fn filenames() {
        use ExportFormat::{Flashback, Mcpr};
        assert_eq!(export_filename("a.mcpr", false, Mcpr), "a.mcpr");
        assert_eq!(export_filename("a.mcpr", false, Flashback), "a.zip");
        assert_eq!(export_filename("a.mcpr", true, Mcpr), "a_merged.mcpr");
        assert_eq!(export_filename("b.zip", true, Flashback), "b_merged.zip");
        assert_eq!(export_filename("noext", false, Mcpr), "noext.mcpr");
        assert_eq!(export_filename(".mcpr", false, Mcpr), "replay.mcpr");
    }
}
