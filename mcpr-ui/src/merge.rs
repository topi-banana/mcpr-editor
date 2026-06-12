//! 複数リプレイの連結。mcpr-cli の連結仕様 (時刻オフセット + 2 個目以降の
//! 接続初期化パケット除外) をパース済みの表示行に対して再現する。

use std::{collections::BTreeSet, rc::Rc};

use mcpr_lib::event::{ReplayInfo, is_connection_init};

use crate::app::{EventRow, Loaded, RowKind, categories_of};

/// 連結時の行フィルタルール。
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum MergeRule {
    /// mcpr-cli 互換: 2 個目以降は Play パケットのみ残し、Login (play) パケット
    /// も除外する ([`is_connection_init`])。Custom 行はフィルタ対象外
    /// (CLI のフィルタは Packet にのみ掛かる)。
    #[default]
    CliCompatible,
    /// 時刻オフセットのみ適用し、全行を保持する。
    OffsetOnly,
}

impl MergeRule {
    pub fn toggled(self) -> Self {
        match self {
            MergeRule::CliCompatible => MergeRule::OffsetOnly,
            MergeRule::OffsetOnly => MergeRule::CliCompatible,
        }
    }
}

/// 読み込み済みリプレイを並び順で連結する。
/// 空入力は None、単一入力は入力 Rc をそのまま返す (従来の単一ファイル表示と
/// 完全に一致し、ポインタが安定するので下流の memo も再計算されない)。
pub fn merge_loaded(
    inputs: &[Rc<Loaded>],
    interval_ms: u64,
    rule: MergeRule,
) -> Option<Rc<Loaded>> {
    match inputs {
        [] => None,
        [single] => Some(single.clone()),
        _ => Some(Rc::new(merge_many(inputs, interval_ms, rule))),
    }
}

fn merge_many(inputs: &[Rc<Loaded>], interval_ms: u64, rule: MergeRule) -> Loaded {
    let mut rows = Vec::with_capacity(inputs.iter().map(|l| l.events.len()).sum());
    let mut players = BTreeSet::new();
    let mut offset_ms = 0u64;
    for (index, loaded) in inputs.iter().enumerate() {
        for row in loaded.events.iter() {
            // 2 個目以降は接続初期化の重複を避ける (mcpr-cli の process と同じ規則)。
            if rule == MergeRule::CliCompatible
                && index > 0
                && let RowKind::Packet { id, state } = &row.kind
                && is_connection_init(*state, *id)
            {
                continue;
            }
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

    fn custom(time_ms: u64, name: &str) -> EventRow {
        EventRow {
            time_ms,
            kind: RowKind::Custom {
                name: name.to_string(),
            },
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
        assert!(merge_loaded(&[], 0, MergeRule::CliCompatible).is_none());
    }

    #[test]
    fn single_file_is_identity() {
        let a = loaded("a.mcpr", 100, vec![packet(0, 0x02, State::Login)]);
        for rule in [MergeRule::CliCompatible, MergeRule::OffsetOnly] {
            let merged = merge_loaded(std::slice::from_ref(&a), 9999, rule).unwrap();
            assert!(Rc::ptr_eq(&merged, &a));
        }
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
        let merged = merge_loaded(&[a, b, c], 1000, MergeRule::CliCompatible).unwrap();
        // b は 500+1000、c は (500+1000)+(300+1000) のオフセット
        assert_eq!(times(&merged), vec![0, 1500, 1750, 2810]);
    }

    #[test]
    fn cli_rule_drops_non_play_packets_in_later_files() {
        let init = || {
            vec![
                packet(0, 0x00, State::Login),
                packet(1, 0x07, State::Configuration),
                packet(2, 0x2c, State::Play),
            ]
        };
        let merged = merge_loaded(
            &[loaded("a.mcpr", 100, init()), loaded("b.mcpr", 100, init())],
            0,
            MergeRule::CliCompatible,
        )
        .unwrap();
        // 1 個目の Login/Config は残り、2 個目は Play のみ
        assert_eq!(times(&merged), vec![0, 1, 2, 102]);
    }

    #[test]
    fn cli_rule_drops_login_play_packet_in_later_files() {
        let rows = || {
            vec![
                packet(0, LOGIN_PLAY_PACKET_ID, State::Play),
                packet(1, 0x2c, State::Play),
            ]
        };
        let merged = merge_loaded(
            &[loaded("a.mcpr", 100, rows()), loaded("b.mcpr", 100, rows())],
            0,
            MergeRule::CliCompatible,
        )
        .unwrap();
        // 1 個目の 0x2b は残り、2 個目の 0x2b だけ落ちる
        assert_eq!(times(&merged), vec![0, 1, 101]);
    }

    #[test]
    fn cli_rule_keeps_custom_events_in_later_files() {
        let a = loaded("a.mcpr", 100, vec![packet(0, 0x2c, State::Play)]);
        let b = loaded(
            "b.zip",
            100,
            vec![custom(0, "flashback:action/move_entities")],
        );
        let merged = merge_loaded(&[a, b], 0, MergeRule::CliCompatible).unwrap();
        // CLI のフィルタは Packet にのみ掛かるため Custom は通過する
        assert_eq!(times(&merged), vec![0, 100]);
        assert!(matches!(merged.events[1].kind, RowKind::Custom { .. }));
    }

    #[test]
    fn offset_only_keeps_all_rows() {
        let init = || {
            vec![
                packet(0, 0x00, State::Login),
                packet(1, LOGIN_PLAY_PACKET_ID, State::Play),
            ]
        };
        let merged = merge_loaded(
            &[loaded("a.mcpr", 100, init()), loaded("b.mcpr", 100, init())],
            0,
            MergeRule::OffsetOnly,
        )
        .unwrap();
        assert_eq!(times(&merged), vec![0, 1, 100, 101]);
    }

    #[test]
    fn merged_info_follows_cli() {
        let mut a = loaded_with("a.mcpr", "ReplayMod", 500, vec![], &[1, 2]);
        {
            let info = &mut Rc::get_mut(&mut a).unwrap().info;
            info.mc_version = "1.21.11".to_string();
            info.protocol_version = 774;
            info.data_version = Some(4671);
        }
        let b = loaded_with("b.mcpr", "ReplayMod", 300, vec![], &[2, 3]);
        let merged = merge_loaded(&[a, b], 1000, MergeRule::CliCompatible).unwrap();
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
    fn merged_categories_reflect_surviving_rows() {
        let a = loaded("a.mcpr", 100, vec![packet(0, 0x2c, State::Play)]);
        let b = loaded("b.mcpr", 100, vec![packet(0, 0x07, State::Configuration)]);
        let cli = merge_loaded(&[a.clone(), b.clone()], 0, MergeRule::CliCompatible).unwrap();
        // 2 個目の Config 行が落ちるため categories にも現れない
        assert_eq!(cli.categories, vec![Category::State(State::Play)]);
        let raw = merge_loaded(&[a, b], 0, MergeRule::OffsetOnly).unwrap();
        assert_eq!(
            raw.categories,
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
        let same = merge_loaded(&[a.clone(), a.clone()], 0, MergeRule::CliCompatible).unwrap();
        assert_eq!(same.format, "ReplayMod");
        let mixed = merge_loaded(&[a, b], 0, MergeRule::CliCompatible).unwrap();
        assert_eq!(mixed.format, "mixed");
    }
}
