//! パケット採否 (書き出しに含めるかどうか)。

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use crate::export::PacketFilter;

/// 1 ファイル分のパケット採否。デフォルト全採用を O(1) で表すため、全体の
/// 既定 (`base`) と、それに反する元イベント index の集合 (`flipped`) で持つ。
/// 採否は `base ^ flipped.contains(index)`。これにより全選択/全解除は
/// `flipped` を空にするだけで済み、巨大なイベント列でも軽い。
#[derive(Clone)]
pub struct PacketSelection {
    base: bool,
    flipped: HashSet<usize>,
}

impl Default for PacketSelection {
    fn default() -> Self {
        // 既定は全採用。
        Self {
            base: true,
            flipped: HashSet::new(),
        }
    }
}

impl PacketSelection {
    pub fn is_on(&self, index: usize) -> bool {
        self.base ^ self.flipped.contains(&index)
    }

    /// 1 件の採否を反転する。
    pub fn toggle(&mut self, index: usize) {
        if !self.flipped.insert(index) {
            self.flipped.remove(&index);
        }
    }

    /// 全件を一括採否する。
    pub fn set_all(&mut self, on: bool) {
        self.base = on;
        self.flipped.clear();
    }

    /// 指定した複数件をまとめて `on` に揃える (ドラッグ範囲選択の一括 On/Off)。
    /// 採否は `base ^ flipped.contains` なので、`on == base` の側は flipped から
    /// 外し、反対側は flipped に入れるだけで済む。
    pub fn set_many(&mut self, indices: &HashSet<usize>, on: bool) {
        if on == self.base {
            for i in indices {
                self.flipped.remove(i);
            }
        } else {
            for &i in indices {
                self.flipped.insert(i);
            }
        }
    }

    /// 採用件数 (`total` は対象ファイルの全イベント数)。
    pub fn on_count(&self, total: usize) -> usize {
        if self.base {
            total.saturating_sub(self.flipped.len())
        } else {
            self.flipped.len()
        }
    }
}

impl PacketFilter for PacketSelection {
    fn keep(&self, index: usize) -> bool {
        self.is_on(index)
    }
}

/// パケット選択をファイル id 単位で持つ。エントリの無いファイルは全採用扱い。
/// 値は Arc にして再描画判定 (フロント) をポインタ比較で行い、かつ書き出しの
/// バックグラウンド処理へ move できるようにする。
#[derive(Default, Clone)]
pub struct SelectionState {
    pub by_file: HashMap<u64, Arc<PacketSelection>>,
}

pub enum SelectionAction {
    /// 1 パケットの採否を反転。
    Toggle { file_id: u64, index: usize },
    /// 対象ファイルの全パケットを一括採否。
    SetAll { file_id: u64, on: bool },
    /// ドラッグ範囲選択した複数パケットをまとめて採否。
    SetMany {
        file_id: u64,
        indices: Arc<HashSet<usize>>,
        on: bool,
    },
    /// ファイル削除時に選択も捨てる。
    Remove { file_id: u64 },
}

impl SelectionState {
    /// アクションを適用した新しい状態を返す (純関数)。Yew の `Reducible` や
    /// gpui の `Entity` 更新から共通に呼ぶ。
    pub fn reduce(&self, action: SelectionAction) -> SelectionState {
        // by_file は Arc のマップで clone は軽量。更新するファイルの中身だけ複製する。
        let mut by_file = self.by_file.clone();
        match action {
            SelectionAction::Toggle { file_id, index } => {
                let mut sel = by_file
                    .get(&file_id)
                    .map_or_else(PacketSelection::default, |s| (**s).clone());
                sel.toggle(index);
                by_file.insert(file_id, Arc::new(sel));
            }
            SelectionAction::SetAll { file_id, on } => {
                let mut sel = by_file
                    .get(&file_id)
                    .map_or_else(PacketSelection::default, |s| (**s).clone());
                sel.set_all(on);
                by_file.insert(file_id, Arc::new(sel));
            }
            SelectionAction::SetMany {
                file_id,
                indices,
                on,
            } => {
                let mut sel = by_file
                    .get(&file_id)
                    .map_or_else(PacketSelection::default, |s| (**s).clone());
                sel.set_many(&indices, on);
                by_file.insert(file_id, Arc::new(sel));
            }
            SelectionAction::Remove { file_id } => {
                by_file.remove(&file_id);
            }
        }
        SelectionState { by_file }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_many_flips_only_listed_indices_to_target() {
        // 既定 (全採用) から一部を Off にし、別の集合を On に戻す。
        let mut sel = PacketSelection::default();
        sel.set_many(&HashSet::from([1, 3, 5]), false);
        assert!(sel.is_on(0));
        assert!(!sel.is_on(1));
        assert!(!sel.is_on(3));
        assert!(!sel.is_on(5));
        assert_eq!(sel.on_count(6), 3);

        // 3 と 5 を On に戻すと 1 だけ Off のまま。
        sel.set_many(&HashSet::from([3, 5]), true);
        assert!(sel.is_on(3));
        assert!(sel.is_on(5));
        assert!(!sel.is_on(1));
        assert_eq!(sel.on_count(6), 5);
    }

    #[test]
    fn set_many_on_after_set_all_off_selects_listed() {
        // 全 Off の土台 (base=false) から指定だけ On にする。
        let mut sel = PacketSelection::default();
        sel.set_all(false);
        sel.set_many(&HashSet::from([2, 4]), true);
        assert!(sel.is_on(2));
        assert!(sel.is_on(4));
        assert!(!sel.is_on(0));
        assert_eq!(sel.on_count(5), 2);
    }
}
