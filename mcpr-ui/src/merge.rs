//! 複数リプレイの連結。順序付き入力 (リプレイと interval) を先頭から走査し、
//! リプレイは現在のオフセットで表示行の時刻を積んでから duration ぶん、
//! interval はその ms ぶんオフセットを進める。行のフィルタはしない。
//!
//! ([`crate::export::export_merged`] の時刻オフセット計算をテストで独立に
//! 検証するための参照実装。)

#[cfg(test)]
use std::{collections::BTreeSet, rc::Rc};

#[cfg(test)]
use mcpr_lib::event::ReplayInfo;

#[cfg(test)]
use crate::app::{EventRow, Loaded, RowKind, categories_of};

/// 連結リストの 1 要素 (テスト用)。[`crate::export::MergeInput`] のテスト版。
#[cfg(test)]
enum MergeItem {
    Replay(Rc<Loaded>),
    Interval(u64),
}

/// 読み込み済みリプレイ列を並び順で連結する。リプレイが無ければ None、
/// リプレイ 1 本だけ (interval なし) なら入力 Rc をそのまま返す (従来の単一
/// ファイル表示と完全に一致し、ポインタが安定するので下流の memo も再計算
/// されない)。
#[cfg(test)]
fn merge_loaded(items: &[MergeItem]) -> Option<Rc<Loaded>> {
    match items {
        [MergeItem::Replay(single)] => Some(single.clone()),
        _ if items.iter().any(|i| matches!(i, MergeItem::Replay(_))) => {
            Some(Rc::new(merge_many(items)))
        }
        _ => None,
    }
}

#[cfg(test)]
fn merge_many(items: &[MergeItem]) -> Loaded {
    let replays: Vec<&Rc<Loaded>> = items
        .iter()
        .filter_map(|i| match i {
            MergeItem::Replay(l) => Some(l),
            MergeItem::Interval(_) => None,
        })
        .collect();
    let mut rows = Vec::with_capacity(replays.iter().map(|l| l.events.len()).sum());
    let mut players = BTreeSet::new();
    let mut offset_ms = 0u64;
    for item in items {
        match item {
            MergeItem::Interval(ms) => offset_ms += ms,
            MergeItem::Replay(loaded) => {
                for row in loaded.events.iter() {
                    rows.push(EventRow {
                        time_ms: row.time_ms + offset_ms,
                        ..row.clone()
                    });
                }
                players.extend(loaded.info.players.iter().copied());
                offset_ms += loaded.info.duration_ms;
            }
        }
    }
    let categories = categories_of(&rows);
    let format = if replays.iter().all(|l| l.format == replays[0].format) {
        replays[0].format
    } else {
        "mixed"
    };
    Loaded {
        filename: replays
            .iter()
            .map(|l| l.filename.as_str())
            .collect::<Vec<_>>()
            .join(" + "),
        format,
        info: ReplayInfo {
            duration_ms: offset_ms,
            players,
            // mc_version / protocol_version / data_version は先頭から継承
            ..replays[0].info.clone()
        },
        events: Rc::new(rows),
        categories,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Category;
    use mcpr_lib::{event::State, protocol::LOGIN_PLAY_PACKET_ID};

    fn packet(time_ms: u64, id: i32, state: State) -> EventRow {
        EventRow {
            time_ms,
            kind: RowKind::Packet { id, state },
            size: 0,
        }
    }

    fn loaded(filename: &str, duration_ms: u64, rows: Vec<EventRow>) -> Rc<Loaded> {
        loaded_with(filename, "ReplayMod", duration_ms, rows, &[])
    }

    fn loaded_with(
        filename: &str,
        format: &'static str,
        duration_ms: u64,
        rows: Vec<EventRow>,
        players: &[u128],
    ) -> Rc<Loaded> {
        let categories = categories_of(&rows);
        Rc::new(Loaded {
            filename: filename.to_string(),
            format,
            info: ReplayInfo {
                duration_ms,
                players: players.iter().map(|&n| uuid::Uuid::from_u128(n)).collect(),
                ..Default::default()
            },
            events: Rc::new(rows),
            categories,
        })
    }

    fn times(merged: &Loaded) -> Vec<u64> {
        merged.events.iter().map(|r| r.time_ms).collect()
    }

    use MergeItem::{Interval, Replay};

    #[test]
    fn empty_input_returns_none() {
        assert!(merge_loaded(&[]).is_none());
    }

    #[test]
    fn interval_only_input_returns_none() {
        // リプレイが無ければ連結対象も無い。
        assert!(merge_loaded(&[Interval(500)]).is_none());
    }

    #[test]
    fn single_file_is_identity() {
        let a = loaded("a.mcpr", 100, vec![packet(0, 0x02, State::Login)]);
        let merged = merge_loaded(&[Replay(a.clone())]).unwrap();
        assert!(Rc::ptr_eq(&merged, &a));
    }

    #[test]
    fn later_file_times_offset_by_preceding_intervals() {
        let a = loaded("a.mcpr", 500, vec![packet(0, 0x2c, State::Play)]);
        let b = loaded(
            "b.mcpr",
            300,
            vec![packet(0, 0x2c, State::Play), packet(250, 0x2c, State::Play)],
        );
        let c = loaded("c.mcpr", 100, vec![packet(10, 0x2c, State::Play)]);
        let merged = merge_loaded(&[
            Replay(a),
            Interval(1000),
            Replay(b),
            Interval(1000),
            Replay(c),
        ])
        .unwrap();
        // b は 500+1000、c は (500+1000)+(300+1000) のオフセット
        assert_eq!(times(&merged), vec![0, 1500, 1750, 2810]);
    }

    #[test]
    fn leading_interval_shifts_first_file() {
        let a = loaded("a.mcpr", 100, vec![packet(0, 0x2c, State::Play)]);
        let merged = merge_loaded(&[Interval(500), Replay(a)]).unwrap();
        // 先頭 interval ぶん最初のファイルがずれ、duration も伸びる
        assert_eq!(times(&merged), vec![500]);
        assert_eq!(merged.info.duration_ms, 600);
    }

    #[test]
    fn trailing_interval_extends_duration_only() {
        let a = loaded("a.mcpr", 100, vec![packet(0, 0x2c, State::Play)]);
        let merged = merge_loaded(&[Replay(a), Interval(500)]).unwrap();
        // イベントはずれず、duration だけ伸びる
        assert_eq!(times(&merged), vec![0]);
        assert_eq!(merged.info.duration_ms, 600);
    }

    #[test]
    fn all_rows_kept_with_offset() {
        let init = || {
            vec![
                packet(0, 0x00, State::Login),
                packet(1, LOGIN_PLAY_PACKET_ID, State::Play),
            ]
        };
        let merged = merge_loaded(&[
            Replay(loaded("a.mcpr", 100, init())),
            Replay(loaded("b.mcpr", 100, init())),
        ])
        .unwrap();
        // 接続初期化を含め全行が残り、2 個目には duration ぶんのオフセットが付く
        assert_eq!(times(&merged), vec![0, 1, 100, 101]);
    }

    #[test]
    fn merged_info_combines_inputs() {
        let mut a = loaded_with("a.mcpr", "ReplayMod", 500, vec![], &[1, 2]);
        {
            let info = &mut Rc::get_mut(&mut a).unwrap().info;
            info.mc_version = "1.21.11".to_string();
            info.protocol_version = 774;
            info.data_version = Some(4671);
        }
        let b = loaded_with("b.mcpr", "ReplayMod", 300, vec![], &[2, 3]);
        let merged = merge_loaded(&[Replay(a), Interval(1000), Replay(b)]).unwrap();
        // duration = Σduration + 明示した interval の総和
        assert_eq!(merged.info.duration_ms, 500 + 1000 + 300);
        // players は union
        let expect: std::collections::BTreeSet<_> = [1u128, 2, 3]
            .iter()
            .map(|&n| uuid::Uuid::from_u128(n))
            .collect();
        assert_eq!(merged.info.players, expect);
        // バージョン情報は先頭から継承
        assert_eq!(merged.info.mc_version, "1.21.11");
        assert_eq!(merged.info.protocol_version, 774);
        assert_eq!(merged.info.data_version, Some(4671));
        assert_eq!(merged.filename, "a.mcpr + b.mcpr");
    }

    #[test]
    fn merged_categories_reflect_all_rows() {
        let a = loaded("a.mcpr", 100, vec![packet(0, 0x2c, State::Play)]);
        let b = loaded("b.mcpr", 100, vec![packet(0, 0x07, State::Configuration)]);
        let merged = merge_loaded(&[Replay(a), Replay(b)]).unwrap();
        // 全行を残すため、両入力の state が categories に現れる
        assert_eq!(
            merged.categories,
            vec![
                Category::State(State::Configuration),
                Category::State(State::Play),
            ]
        );
    }

    #[test]
    fn mixed_formats_reported() {
        let a = loaded_with("a.mcpr", "ReplayMod", 100, vec![], &[]);
        let b = loaded_with("b.zip", "Flashback", 100, vec![], &[]);
        let same = merge_loaded(&[Replay(a.clone()), Replay(a.clone())]).unwrap();
        assert_eq!(same.format, "ReplayMod");
        let mixed = merge_loaded(&[Replay(a), Replay(b)]).unwrap();
        assert_eq!(mixed.format, "mixed");
    }
}
