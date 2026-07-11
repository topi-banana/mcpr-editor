//! イベントテーブルの表示フィルタとソート。

use std::cmp::Ordering;

use mcpr_lib::protocol::parse_packet_id;

use crate::row::{Category, EventRow, RowKind};

/// イベントテーブルの表示フィルタ。
/// (PartialEq は indices を導出する再計算の依存キーとして使う)
#[derive(Clone, PartialEq, Default)]
pub struct EventFilter {
    /// 非表示カテゴリのビット集合 ([`Category::bit`]、0 = 全表示)。
    hidden: u8,
    /// イベント検索クエリ (入力欄の原文)。
    pub query: String,
    /// query の 16 進 packet id 解釈 (マッチ用キャッシュ)。
    query_id: Option<i32>,
    /// query の小文字化 (Custom 名マッチ用キャッシュ)。
    query_lower: String,
}

impl EventFilter {
    pub fn with_query(&self, query: String) -> Self {
        Self {
            hidden: self.hidden,
            query_id: parse_packet_id(&query),
            query_lower: query.trim().to_lowercase(),
            query,
        }
    }

    pub fn with_toggled(&self, category: Category) -> Self {
        Self {
            hidden: self.hidden ^ category.bit(),
            ..self.clone()
        }
    }

    pub fn is_hidden(&self, category: Category) -> bool {
        self.hidden & category.bit() != 0
    }

    pub fn is_empty(&self) -> bool {
        self.hidden == 0 && self.query_lower.is_empty()
    }

    /// クエリは「event 列の表示」へのマッチ:
    /// 16 進として解釈できれば packet id の一致、Custom は常に名前の部分一致。
    pub fn matches(&self, row: &EventRow) -> bool {
        if self.is_hidden(Category::of(&row.kind)) {
            return false;
        }
        if self.query_lower.is_empty() {
            return true;
        }
        match &row.kind {
            RowKind::Packet { id, .. } => self.query_id == Some(*id),
            RowKind::Custom { name } => name.contains(&self.query_lower),
        }
    }
}

/// ソート対象のカラム。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Index,
    Time,
    Event,
    State,
    Size,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

/// event 列の全順序: packet (id 順) → custom (名前順)。
fn event_ord(a: &RowKind, b: &RowKind) -> Ordering {
    match (a, b) {
        (RowKind::Packet { id: a, .. }, RowKind::Packet { id: b, .. }) => a.cmp(b),
        (RowKind::Custom { name: a }, RowKind::Custom { name: b }) => a.cmp(b),
        (RowKind::Packet { .. }, RowKind::Custom { .. }) => Ordering::Less,
        (RowKind::Custom { .. }, RowKind::Packet { .. }) => Ordering::Greater,
    }
}

/// `indices` (元 index 昇順) をカラム値で並べ替える。
/// 安定ソートのため同値は記録順を保つ (Desc は全体の反転)。
/// (公開 API は [`compute_indices`] のみで、ソートはその内部実装。)
pub(crate) fn sort_indices(events: &[EventRow], indices: &mut [usize], key: SortKey, dir: SortDir) {
    match key {
        SortKey::Index => {}
        SortKey::Time => indices.sort_by_key(|&i| events[i].time_ms),
        SortKey::Event => indices.sort_by(|&a, &b| event_ord(&events[a].kind, &events[b].kind)),
        SortKey::State => indices.sort_by_key(|&i| Category::of(&events[i].kind).bit()),
        SortKey::Size => indices.sort_by_key(|&i| events[i].size),
    }
    if dir == SortDir::Desc {
        indices.reverse();
    }
}

/// フィルタとソートを適用した表示 index 列を求める。
/// フィルタが空でソートも無いとき (= 全行を記録順のまま) は `None` を返し、
/// 呼び出し側が元配列をそのまま使えるようにする (数百万行のコピー回避)。
pub fn compute_indices(
    events: &[EventRow],
    filter: &EventFilter,
    sort: Option<(SortKey, SortDir)>,
) -> Option<Vec<usize>> {
    if filter.is_empty() && sort.is_none() {
        return None;
    }
    let mut matched: Vec<usize> = if filter.is_empty() {
        (0..events.len()).collect()
    } else {
        let mut v = Vec::with_capacity(events.len());
        v.extend(
            events
                .iter()
                .enumerate()
                .filter(|(_, row)| filter.matches(row))
                .map(|(i, _)| i),
        );
        v.shrink_to_fit();
        v
    };
    if let Some((key, dir)) = sort {
        sort_indices(events, &mut matched, key, dir);
    }
    Some(matched)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcpr_lib::event::State;

    fn packet(id: i32, state: State) -> EventRow {
        EventRow {
            time_ms: 0,
            kind: RowKind::Packet { id, state },
            size: 0,
        }
    }

    fn custom(name: &str) -> EventRow {
        EventRow {
            time_ms: 0,
            kind: RowKind::Custom {
                name: name.to_string(),
            },
            size: 0,
        }
    }

    #[test]
    fn default_filter_shows_everything() {
        let f = EventFilter::default();
        assert!(f.is_empty());
        assert!(f.matches(&packet(0x2c, State::Play)));
        assert!(f.matches(&custom("flashback:action/move_entities")));
    }

    #[test]
    fn hidden_category_drops_rows() {
        let f = EventFilter::default().with_toggled(Category::State(State::Play));
        assert!(!f.matches(&packet(0x2c, State::Play)));
        assert!(f.matches(&packet(0x07, State::Configuration)));
        assert!(f.matches(&custom("flashback:action/move_entities")));
        // 再トグルで元に戻る
        let f = f.with_toggled(Category::State(State::Play));
        assert!(f.is_empty());
    }

    #[test]
    fn hex_query_matches_packet_id() {
        for q in ["0x2c", "2c", " 0x2C "] {
            let f = EventFilter::default().with_query(q.to_string());
            assert!(f.matches(&packet(0x2c, State::Play)), "query {q:?}");
            assert!(!f.matches(&packet(0x2b, State::Play)), "query {q:?}");
        }
    }

    #[test]
    fn text_query_matches_custom_name_case_insensitive() {
        for q in ["move", "MOVE"] {
            let f = EventFilter::default().with_query(q.to_string());
            assert!(f.matches(&custom("flashback:action/move_entities")));
            assert!(!f.matches(&custom("flashback:action/next_tick")));
            // 16 進として解釈できないクエリはパケットに一致しない
            assert!(!f.matches(&packet(0x2c, State::Play)));
        }
    }

    #[test]
    fn query_and_category_combine() {
        let f = EventFilter::default()
            .with_toggled(Category::State(State::Play))
            .with_query("0x07".to_string());
        // クエリは一致するがカテゴリが非表示
        assert!(!f.matches(&packet(0x07, State::Play)));
        assert!(f.matches(&packet(0x07, State::Configuration)));
    }

    fn sorted(events: &[EventRow], key: SortKey, dir: SortDir) -> Vec<usize> {
        let mut indices: Vec<usize> = (0..events.len()).collect();
        sort_indices(events, &mut indices, key, dir);
        indices
    }

    #[test]
    fn sort_by_time_is_stable() {
        let mut events = vec![
            packet(0x01, State::Play),
            packet(0x02, State::Play),
            packet(0x03, State::Play),
        ];
        events[0].time_ms = 100;
        // index 1, 2 は同時刻 → 記録順を保つ
        events[1].time_ms = 50;
        events[2].time_ms = 50;
        assert_eq!(sorted(&events, SortKey::Time, SortDir::Asc), vec![1, 2, 0]);
        assert_eq!(sorted(&events, SortKey::Time, SortDir::Desc), vec![0, 2, 1]);
    }

    #[test]
    fn sort_by_event_orders_packets_before_customs() {
        let events = vec![
            custom("flashback:action/move_entities"),
            packet(0x2c, State::Play),
            custom("flashback:action/accurate_player_position"),
            packet(0x07, State::Configuration),
        ];
        // packet (id 順) → custom (名前順)
        assert_eq!(
            sorted(&events, SortKey::Event, SortDir::Asc),
            vec![3, 1, 2, 0]
        );
    }

    #[test]
    fn sort_by_state_follows_phase_order() {
        let events = vec![
            custom("flashback:action/move_entities"),
            packet(0x2c, State::Play),
            packet(0x02, State::Login),
            packet(0x07, State::Configuration),
        ];
        // Login → Config → Play → Custom
        assert_eq!(
            sorted(&events, SortKey::State, SortDir::Asc),
            vec![2, 3, 1, 0]
        );
    }

    #[test]
    fn sort_by_index_desc_reverses() {
        let events = vec![packet(0x01, State::Play), packet(0x02, State::Play)];
        assert_eq!(sorted(&events, SortKey::Index, SortDir::Asc), vec![0, 1]);
        assert_eq!(sorted(&events, SortKey::Index, SortDir::Desc), vec![1, 0]);
    }
}
