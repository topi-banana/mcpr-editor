//! 複数リプレイの連結。各入力の表示行に累積オフセット (duration + interval)
//! を加算して 1 本に連結する。行のフィルタはしない。

#[cfg(test)]
use std::{collections::BTreeSet, rc::Rc};

#[cfg(test)]
use mcpr_lib::event::ReplayInfo;

#[cfg(test)]
use crate::app::{EventRow, Loaded, RowKind, categories_of};

/// 読み込み済みリプレイを並び順で連結する。
/// 空入力は None、単一入力は入力 Rc をそのまま返す (従来の単一ファイル表示と
/// 完全に一致し、ポインタが安定するので下流の memo も再計算されない)。
#[cfg(test)]
fn merge_loaded(inputs: &[Rc<Loaded>], interval_ms: u64) -> Option<Rc<Loaded>> {
    match inputs {
        [] => None,
        [single] => Some(single.clone()),
        _ => Some(Rc::new(merge_many(inputs, interval_ms))),
    }
}

#[cfg(test)]
fn merge_many(inputs: &[Rc<Loaded>], interval_ms: u64) -> Loaded {
    let mut rows = Vec::with_capacity(inputs.iter().map(|l| l.events.len()).sum());
    let mut players = BTreeSet::new();
    let mut offset_ms = 0u64;
    for loaded in inputs.iter() {
        for row in loaded.events.iter() {
            rows.push(EventRow {
                time_ms: row.time_ms + offset_ms,
                ..row.clone()
            });
        }
        players.extend(loaded.info.players.iter().copied());
        offset_ms += loaded.info.duration_ms + interval_ms;
    }
    let categories = categories_of(&rows);
    let format = if inputs.iter().all(|l| l.format == inputs[0].format) {
        inputs[0].format
    } else {
        "mixed"
    };
    Loaded {
        filename: inputs
            .iter()
            .map(|l| l.filename.as_str())
            .collect::<Vec<_>>()
            .join(" + "),
        format,
        info: ReplayInfo {
            duration_ms: offset_ms.saturating_sub(interval_ms),
            players,
            // mc_version / protocol_version / data_version は先頭から継承
            ..inputs[0].info.clone()
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

    #[test]
    fn empty_input_returns_none() {
        assert!(merge_loaded(&[], 0).is_none());
    }

    #[test]
    fn single_file_is_identity() {
        let a = loaded("a.mcpr", 100, vec![packet(0, 0x02, State::Login)]);
        let merged = merge_loaded(std::slice::from_ref(&a), 9999).unwrap();
        assert!(Rc::ptr_eq(&merged, &a));
    }

    #[test]
    fn later_file_times_offset_by_duration_plus_interval() {
        let a = loaded("a.mcpr", 500, vec![packet(0, 0x2c, State::Play)]);
        let b = loaded(
            "b.mcpr",
            300,
            vec![packet(0, 0x2c, State::Play), packet(250, 0x2c, State::Play)],
        );
        let c = loaded("c.mcpr", 100, vec![packet(10, 0x2c, State::Play)]);
        let merged = merge_loaded(&[a, b, c], 1000).unwrap();
        // b は 500+1000、c は (500+1000)+(300+1000) のオフセット
        assert_eq!(times(&merged), vec![0, 1500, 1750, 2810]);
    }

    #[test]
    fn all_rows_kept_with_offset() {
        let init = || {
            vec![
                packet(0, 0x00, State::Login),
                packet(1, LOGIN_PLAY_PACKET_ID, State::Play),
            ]
        };
        let merged = merge_loaded(
            &[loaded("a.mcpr", 100, init()), loaded("b.mcpr", 100, init())],
            0,
        )
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
        let merged = merge_loaded(&[a, b], 1000).unwrap();
        // duration = Σduration + (N-1) * interval
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
        let merged = merge_loaded(&[a, b], 0).unwrap();
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
        let same = merge_loaded(&[a.clone(), a.clone()], 0).unwrap();
        assert_eq!(same.format, "ReplayMod");
        let mixed = merge_loaded(&[a, b], 0).unwrap();
        assert_eq!(mixed.format, "mixed");
    }
}
