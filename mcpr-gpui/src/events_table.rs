//! イベントテーブルの [`TableDelegate`] 実装。
//!
//! gpui-component の仮想化テーブルへ、共有クレートの表示行 [`EventRow`] と
//! フィルタ/ソート ([`compute_indices`]) を接続する。web 版のページングは廃止し、
//! 仮想スクロールで全行を表示する。パケット採否 (チェックボックス列) は
//! [`AppStore`] のライブ状態を参照し、クリックで `SelectionAction::Toggle` を送る。

use gpui::{App, Context, IntoElement, ParentElement, WeakEntity, Window, div, px};
use gpui_component::{
    checkbox::Checkbox,
    table::{Column, ColumnSort, TableDelegate, TableState},
};
use mcpr_app::{
    EventFilter, EventRow, RowKind, SelectionAction, SortDir, SortKey, compute_indices, state_name,
};

use crate::store::AppStore;

pub struct EventsTableDelegate {
    store: WeakEntity<AppStore>,
    file_id: u64,
    /// 表示行 (パース後は不変なので snapshot でよい)。
    events: std::sync::Arc<Vec<EventRow>>,
    filter: EventFilter,
    sort: Option<(SortKey, SortDir)>,
    /// フィルタ/ソート適用後の元 index 列 (None = 全行を記録順のまま)。
    indices: Option<Vec<usize>>,
    columns: Vec<Column>,
}

/// 列の並び (web 版のテーブルと同じ): 採否 / # / time / event / state / size。
const COL_SELECT: usize = 0;
const COL_INDEX: usize = 1;
const COL_TIME: usize = 2;
const COL_EVENT: usize = 3;
const COL_STATE: usize = 4;
const COL_SIZE: usize = 5;

impl EventsTableDelegate {
    pub fn new(
        store: WeakEntity<AppStore>,
        file_id: u64,
        events: std::sync::Arc<Vec<EventRow>>,
    ) -> Self {
        let columns = vec![
            Column::new("sel", "").width(px(36.)),
            Column::new("idx", "#").width(px(80.)).sortable(),
            Column::new("time", "time (ms)").width(px(110.)).sortable(),
            Column::new("event", "event").sortable(),
            Column::new("state", "state").width(px(110.)).sortable(),
            Column::new("size", "size").width(px(90.)).sortable(),
        ];
        Self {
            store,
            file_id,
            events,
            filter: EventFilter::default(),
            sort: None,
            indices: None,
            columns,
        }
    }

    /// フィルタを差し替えて index 列を再計算する。
    pub fn set_filter(&mut self, filter: EventFilter) {
        self.filter = filter;
        self.recompute();
    }

    pub fn filter(&self) -> &EventFilter {
        &self.filter
    }

    /// 現在フィルタ/ソートで表示中の元 index 集合 (一括 On/Off の対象)。
    /// web 版のドラッグ範囲選択に相当する「見えている行をまとめて採否」に使う。
    /// 呼び出し側は HashSet を欲しがるため、Vec を複製せず直接集合を組む。
    pub fn visible_indices_set(&self) -> std::collections::HashSet<usize> {
        match &self.indices {
            Some(v) => v.iter().copied().collect(),
            None => (0..self.events.len()).collect(),
        }
    }

    /// 表示が全行を覆っているか (絞り込みで件数が減っていない)。true なら一括
    /// 採否を全 index の列挙ではなく O(1) の全採否 (`SetAll`) で済ませられる。
    pub fn covers_all_rows(&self) -> bool {
        self.indices
            .as_ref()
            .is_none_or(|v| v.len() == self.events.len())
    }

    fn recompute(&mut self) {
        self.indices = compute_indices(&self.events, &self.filter, self.sort);
    }

    /// 表示位置 row_ix を元 index へ写す。
    fn orig_index(&self, row_ix: usize) -> usize {
        self.indices.as_ref().map_or(row_ix, |v| v[row_ix])
    }
}

fn sort_key_of(col_ix: usize) -> Option<SortKey> {
    match col_ix {
        COL_INDEX => Some(SortKey::Index),
        COL_TIME => Some(SortKey::Time),
        COL_EVENT => Some(SortKey::Event),
        COL_STATE => Some(SortKey::State),
        COL_SIZE => Some(SortKey::Size),
        _ => None,
    }
}

impl TableDelegate for EventsTableDelegate {
    fn columns_count(&self, _cx: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _cx: &App) -> usize {
        self.indices.as_ref().map_or(self.events.len(), Vec::len)
    }

    fn column(&self, col_ix: usize, _cx: &App) -> Column {
        self.columns[col_ix].clone()
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) {
        self.sort = sort_key_of(col_ix).and_then(|key| match sort {
            ColumnSort::Ascending => Some((key, SortDir::Asc)),
            ColumnSort::Descending => Some((key, SortDir::Desc)),
            ColumnSort::Default => None,
        });
        self.recompute();
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let orig = self.orig_index(row_ix);
        let row = &self.events[orig];
        match col_ix {
            COL_SELECT => {
                // 採否は store のライブ状態を参照 (エントリ無し = 全採用)。
                let checked = self
                    .store
                    .upgrade()
                    .and_then(|s| {
                        s.read(cx)
                            .selection
                            .by_file
                            .get(&self.file_id)
                            .map(|sel| sel.is_on(orig))
                    })
                    .unwrap_or(true);
                let store = self.store.clone();
                let file_id = self.file_id;
                Checkbox::new(("sel", orig))
                    .checked(checked)
                    .on_click(move |_checked, _window, cx: &mut App| {
                        if let Some(store) = store.upgrade() {
                            store.update(cx, |store, cx| {
                                store.dispatch_selection(
                                    SelectionAction::Toggle {
                                        file_id,
                                        index: orig,
                                    },
                                    cx,
                                );
                            });
                        }
                    })
                    .into_any_element()
            }
            COL_INDEX => div().child(orig.to_string()).into_any_element(),
            COL_TIME => div().child(row.time_ms.to_string()).into_any_element(),
            COL_EVENT => match &row.kind {
                RowKind::Packet { id, .. } => div().child(format!("0x{id:02x}")).into_any_element(),
                RowKind::Custom { name } => div().child(name.clone()).into_any_element(),
            },
            COL_STATE => match &row.kind {
                RowKind::Packet { state, .. } => div().child(state_name(*state)).into_any_element(),
                RowKind::Custom { .. } => div().child("—").into_any_element(),
            },
            _ => div().child(row.size.to_string()).into_any_element(),
        }
    }
}
