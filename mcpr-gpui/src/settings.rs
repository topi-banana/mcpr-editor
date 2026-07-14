//! 設定の永続化 (テーマ等)。gpui 非依存の純ロジックで、`dirs` が返す
//! ユーザ設定ディレクトリ配下の JSON に読み書きする。
//! web 版 mcpr-ui の localStorage テーマ保存に対応する native 実装。

use std::{fs, path::PathBuf};

use serde::{Deserialize, Serialize};

/// 配色テーマ。web 版 [`Theme`](../../mcpr-ui/src/app.rs) と同義。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    Light,
    Dark,
}

impl Theme {
    pub fn toggled(self) -> Self {
        match self {
            Theme::Light => Theme::Dark,
            Theme::Dark => Theme::Light,
        }
    }
}

/// 永続化する設定。今はテーマのみ。将来のキー追加に備えて `default` を付ける。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Settings {
    #[serde(default)]
    pub theme: Theme,
}

/// 設定ファイルのパス (`<config_dir>/mcpr-editor/settings.json`)。
/// `config_dir` が取れない環境ではカレント直下へフォールバックする。
fn settings_path() -> PathBuf {
    let mut dir = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    dir.push("mcpr-editor");
    dir.push("settings.json");
    dir
}

impl Settings {
    /// 設定を読み込む。ファイルが無い/壊れている場合は既定値。
    pub fn load() -> Settings {
        let path = settings_path();
        let Ok(text) = fs::read_to_string(&path) else {
            return Settings::default();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    /// 設定を保存する。ディレクトリが無ければ作る。失敗は握りつぶす
    /// (設定の保存失敗でアプリを止めない)。
    pub fn save(&self) {
        let path = settings_path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = fs::write(&path, text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_toggles() {
        assert_eq!(Theme::Light.toggled(), Theme::Dark);
        assert_eq!(Theme::Dark.toggled(), Theme::Light);
    }

    #[test]
    fn settings_roundtrip_json() {
        let s = Settings { theme: Theme::Dark };
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, r#"{"theme":"dark"}"#);
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn missing_key_defaults() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.theme, Theme::Light);
    }
}
