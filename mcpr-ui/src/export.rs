//! ブラウザ専用の書き出し補助。
//!
//! 連結・書き出しエンジン ([`export_merged`] 等) は UI 非依存の
//! [`mcpr_app::export`] に移設済み。ここには生成済みバイト列を
//! ブラウザからダウンロードさせる web 固有処理だけを残す。

use wasm_bindgen::JsCast;

/// Blob ダウンロードをトリガする (ブラウザ専用、テスト対象外)。
pub fn trigger_download(bytes: &[u8], filename: &str) {
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
