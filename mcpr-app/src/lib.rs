//! UI フレームワーク非依存のアプリロジック。
//!
//! mcpr-ui (Yew) と mcpr-gpui (gpui) が共有する。表示用データモデル
//! ([`row`])、イベントテーブルのフィルタ/ソート ([`filter`])、パケット採否
//! ([`selection`])、ファイル/interval リストの状態遷移 ([`files`])、連結
//! 書き出し ([`export`]) を持つ。状態遷移は Yew の `Reducible` や gpui の
//! `Entity` に依存しない純関数 (`reduce`) として提供し、各フロントが薄い器で
//! 包む。

pub mod export;
pub mod files;
pub mod filter;
pub mod row;
pub mod selection;

#[cfg(test)]
mod merge;

pub use export::{
    ExportFormat, ExportPhase, ExportPlan, ExportProgress, MergeInput, OwnedMergeItem,
    PacketFilter, as_merge_inputs, build_export_plan, export_filename, export_merged,
    new_replay_uuid,
};
pub use files::{Entry, EntryKind, EntryState, FilesAction, FilesState, speed_label};
pub use filter::{EventFilter, SortDir, SortKey, compute_indices};
pub use row::{Category, EventRow, Loaded, RowKind, parse_replay, state_name};
pub use selection::{PacketSelection, SelectionAction, SelectionState};

#[cfg(not(target_arch = "wasm32"))]
pub use export::export_merged_blocking;
