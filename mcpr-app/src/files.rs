//! ファイルと interval の順序付きリスト (連結の入力列)。

use std::sync::Arc;

use mcpr_lib::event::PlaybackSpeed;

use crate::row::Loaded;

/// ファイルエントリの読み込み状態。
#[derive(Clone)]
pub enum EntryState {
    Loading,
    /// パース後は不変。Arc は再描画判定 (フロント) のポインタ比較も兼ねる。
    /// `bytes` は元ファイルの生バイト列で、書き出し時の再パースに使う
    /// (表示行 [`crate::row::EventRow`] はパケット body を持たないため)。
    Loaded {
        loaded: Arc<Loaded>,
        bytes: Arc<Vec<u8>>,
    },
    Error(String),
}

/// 連結リストの 1 エントリの中身。ファイルと interval を同列に並べる。
#[derive(Clone)]
pub enum EntryKind {
    File {
        filename: String,
        state: EntryState,
        /// 再生速度倍率の入力原文。空/不正値は書き出し時にエラーにする。
        speed_input: String,
    },
    /// 連結時に直前までのタイムラインへ加算する空白。入力欄の原文を保持し、
    /// parse 失敗は 0 扱い (旧 interval 入力欄と同じ方針)。
    Interval { input: String },
}

/// 連結リストの 1 エントリ。`id` は発番順の安定キー
/// (フロントの key、読み込み中 reader の管理、DnD の並べ替えに使う)。
#[derive(Clone)]
pub struct Entry {
    pub id: u64,
    pub kind: EntryKind,
}

/// ファイルと interval の順序付きリスト。
/// 複数の読み込み完了が並行して届くため、クロージャに古い状態を
/// キャプチャしない [`FilesAction`] の適用 ([`FilesState::reduce`]) で更新する。
#[derive(Default, Clone)]
pub struct FilesState {
    pub entries: Vec<Entry>,
}

pub enum FilesAction {
    /// 読み込み開始 (Loading のファイルエントリを末尾へ追加)。
    Add {
        id: u64,
        filename: String,
    },
    /// 読み込み・パース完了。完了前に削除されていたら no-op。
    Finish {
        id: u64,
        result: Result<(Arc<Loaded>, Arc<Vec<u8>>), String>,
    },
    /// interval エントリを末尾へ追加。
    AddInterval {
        id: u64,
        input: String,
    },
    /// 既存 interval の値を差し替える (ダイアログ編集)。
    SetInterval {
        id: u64,
        input: String,
    },
    /// 既存ファイルの速度倍率を差し替える。
    SetSpeed {
        id: u64,
        input: String,
    },
    Remove {
        id: u64,
    },
    Reorder {
        dragged_id: u64,
        target_id: u64,
    },
}

impl FilesState {
    /// アクションを適用した新しい状態を返す (純関数)。
    pub fn reduce(&self, action: FilesAction) -> FilesState {
        // Entry の clone は Arc + String のみで軽量。
        let mut entries = self.entries.clone();
        match action {
            FilesAction::Add { id, filename } => entries.push(Entry {
                id,
                kind: EntryKind::File {
                    filename,
                    state: EntryState::Loading,
                    speed_input: "1".to_string(),
                },
            }),
            FilesAction::Finish { id, result } => {
                if let Some(EntryKind::File { state, .. }) =
                    entries.iter_mut().find(|e| e.id == id).map(|e| &mut e.kind)
                {
                    *state = match result {
                        Ok((loaded, bytes)) => EntryState::Loaded { loaded, bytes },
                        Err(msg) => EntryState::Error(msg),
                    };
                }
            }
            FilesAction::AddInterval { id, input } => entries.push(Entry {
                id,
                kind: EntryKind::Interval { input },
            }),
            FilesAction::SetInterval { id, input } => {
                if let Some(EntryKind::Interval { input: slot }) =
                    entries.iter_mut().find(|e| e.id == id).map(|e| &mut e.kind)
                {
                    *slot = input;
                }
            }
            FilesAction::SetSpeed { id, input } => {
                if let Some(EntryKind::File { speed_input, .. }) =
                    entries.iter_mut().find(|e| e.id == id).map(|e| &mut e.kind)
                {
                    *speed_input = input;
                }
            }
            FilesAction::Remove { id } => entries.retain(|e| e.id != id),
            FilesAction::Reorder {
                dragged_id,
                target_id,
            } => {
                if dragged_id != target_id
                    && let Some(from) = entries.iter().position(|e| e.id == dragged_id)
                    && let Some(to) = entries.iter().position(|e| e.id == target_id)
                {
                    let entry = entries.remove(from);
                    entries.insert(to.min(entries.len()), entry);
                }
            }
        }
        FilesState { entries }
    }

    /// ファイルエントリ数 (interval は数えない)。
    pub fn file_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| matches!(e.kind, EntryKind::File { .. }))
            .count()
    }

    /// 詳細表示する Loaded ファイルを解決する (`selected` があればそれ、
    /// 無ければ先頭の Loaded)。返り値は (id, データ, 速度入力原文) の借用で、
    /// interval エントリは対象外。フロントの「表示中ファイル」解決に使う。
    pub fn active_loaded(&self, selected: Option<u64>) -> Option<(u64, &Arc<Loaded>, &str)> {
        let find = |want: Option<u64>| {
            self.entries.iter().find_map(|entry| match &entry.kind {
                EntryKind::File {
                    state: EntryState::Loaded { loaded, .. },
                    speed_input,
                    ..
                } if want.is_none_or(|id| entry.id == id) => {
                    Some((entry.id, loaded, speed_input.as_str()))
                }
                _ => None,
            })
        };
        selected
            .and_then(|id| find(Some(id)))
            .or_else(|| find(None))
    }

    /// 書き出し可能か (ファイルが 1 件以上あり、全ファイルが読み込み完了)。
    /// interval は阻害しない。
    pub fn all_loaded(&self) -> bool {
        self.file_count() > 0
            && self.entries.iter().all(|e| match &e.kind {
                EntryKind::File { state, .. } => matches!(state, EntryState::Loaded { .. }),
                EntryKind::Interval { .. } => true,
            })
    }
}

/// 速度入力の原文を表示ラベルへ整形する。
pub fn speed_label(input: &str) -> String {
    match input.trim().parse::<PlaybackSpeed>() {
        Ok(speed) => format!("{speed}x"),
        Err(_) => "invalid".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loading_file(id: u64) -> Entry {
        Entry {
            id,
            kind: EntryKind::File {
                filename: format!("{id}.mcpr"),
                state: EntryState::Loading,
                speed_input: "1".to_string(),
            },
        }
    }

    fn interval_entry(id: u64, input: &str) -> Entry {
        Entry {
            id,
            kind: EntryKind::Interval {
                input: input.to_string(),
            },
        }
    }

    fn entry_ids(state: &FilesState) -> Vec<u64> {
        state.entries.iter().map(|entry| entry.id).collect()
    }

    #[test]
    fn file_reorder_moves_dragged_entry_to_drop_target_boundary() {
        let state = FilesState {
            entries: vec![loading_file(1), loading_file(2), loading_file(3)],
        };

        let state = state.reduce(FilesAction::Reorder {
            dragged_id: 1,
            target_id: 3,
        });
        assert_eq!(entry_ids(&state), vec![2, 3, 1]);

        let state = state.reduce(FilesAction::Reorder {
            dragged_id: 1,
            target_id: 2,
        });
        assert_eq!(entry_ids(&state), vec![1, 2, 3]);
    }

    #[test]
    fn interval_reorders_among_files_like_a_file() {
        // ファイル・ファイル・interval を並べ、interval を id ベースで先頭へ。
        let state = FilesState {
            entries: vec![loading_file(1), loading_file(2), interval_entry(3, "500")],
        };

        let state = state.reduce(FilesAction::Reorder {
            dragged_id: 3,
            target_id: 1,
        });
        assert_eq!(entry_ids(&state), vec![3, 1, 2]);
    }

    #[test]
    fn set_interval_updates_only_matching_interval() {
        let state = FilesState {
            entries: vec![loading_file(1), interval_entry(2, "500")],
        };

        let state = state.reduce(FilesAction::SetInterval {
            id: 2,
            input: "1200".to_string(),
        });

        let inputs: Vec<Option<&str>> = state
            .entries
            .iter()
            .map(|e| match &e.kind {
                EntryKind::Interval { input } => Some(input.as_str()),
                EntryKind::File { .. } => None,
            })
            .collect();
        assert_eq!(inputs, vec![None, Some("1200")]);
    }

    #[test]
    fn set_speed_updates_only_matching_file() {
        let state = FilesState {
            entries: vec![loading_file(1), interval_entry(2, "500")],
        };

        let state = state.reduce(FilesAction::SetSpeed {
            id: 1,
            input: "2".to_string(),
        });

        let speeds: Vec<Option<&str>> = state
            .entries
            .iter()
            .map(|e| match &e.kind {
                EntryKind::File { speed_input, .. } => Some(speed_input.as_str()),
                EntryKind::Interval { .. } => None,
            })
            .collect();
        assert_eq!(speeds, vec![Some("2"), None]);
    }
}
