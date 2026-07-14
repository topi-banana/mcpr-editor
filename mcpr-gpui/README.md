# mcpr-gpui

[gpui](https://www.gpui.rs/)(Zed 製 UI フレームワーク)+ [gpui-component](https://github.com/longbridge/gpui-component) による mcpr editor の native UI。

アプリロジック(連結・書き出し・フィルタ・状態遷移)は `mcpr-app` を通じて `mcpr-ui`(web/Yew)と共有する。この crate は gpui の View 層だけを持つ。

## 実装状況

実装済み(コンパイル済み・実 .mcpr で起動/描画確認済み):

- ファイル読み込み(ファイルダイアログ / コマンドライン引数 `-- a.mcpr b.zip`、背景スレッドでパース)
- ファイルリスト(選択・削除)、interval 追加、複数ファイル連結
- メタデータパネル、再生速度の増減(± 0.5x)
- 仮想スクロールのイベントテーブル(列ソート付き)
- パケット採否チェックボックス(行トグル / 全選択・全解除)
- カテゴリフィルタトグル + テキスト検索(packet id / custom 名)
- フィルタ中の行を一括 On/Off(`On (shown)` / `Off (shown)`、`SelectionAction::SetMany`)
- ファイルの DnD 並べ替え(行をドラッグして順序変更)、OS からのファイルドロップ
- Export(mcpr / Flashback、保存ダイアログ、背景実行 + 進捗、連結・速度・採否を反映)
- ライト/ダークテーマ切替 + 設定ファイル永続化

連結・書き出し・フィルタ・状態遷移のロジックは `mcpr-app` を web 版 `mcpr-ui` と共有しており、
`cargo test -p mcpr-app` で検証済み。

### 未実装(フォローアップ)

- 行のマウスドラッグによる範囲選択(現状はフィルタ + `On/Off (shown)` の一括で代替。
  仮想スクロールでは視界外行の追従が難しいため後回し)
- interval 値のダイアログ編集(現状は既定 1000ms 追加のみ)

## モジュール構成

```
src/
  main.rs         エントリ (application().run、init、Root、Workspace、CLI 引数読み込み)
  store.rs        Entity<AppStore>: 状態 + dispatch + 読み込み/書き出しの async
  workspace.rs    ルート View (ヘッダ / サイドバー / メタデータ / イベントパネル)
  events_table.rs gpui-component TableDelegate 実装 (仮想テーブル)
  settings.rs     テーマ永続化 (gpui 非依存の純ロジック)
```

## ビルド要件(Linux)

gpui は GPU レンダリング(Vulkan)とテキスト整形にシステムライブラリを要求する。
Debian/Ubuntu 系:

```sh
sudo apt install libxkbcommon-dev libwayland-dev xorg-dev \
                 libvulkan1 mesa-vulkan-drivers vulkan-tools
```

- `libxkbcommon-dev`: x11 / wayland どちらのバックエンドでも必須
- `libwayland-dev`: `wayland` feature を使う場合(`x11` のみなら不要)
- `xorg-dev`: `x11` feature 用
- Vulkan: レンダラ(blade)が要求。`vulkaninfo --summary` で ICD を確認

ツールチェーンは stable でよい(現行 gpui-component は edition 2024、nightly 不要)。

## 実行

```sh
cargo run -p mcpr-gpui
```

`mcpr-gpui` は workspace の `default-members` から外してあるため、
`cargo test` / `cargo build`(`--workspace` 無し)では**ビルドされない**。
CI(mcpr-ui の Trunk / CLI)に gpui のシステム依存を持ち込まないための措置。
gpui を扱うときは `-p mcpr-gpui` を明示する。

### WSL2 / WSLg での注意

WSLg は Wayland が古く(`UnsupportedVersion` パニック)、また Dozen(D3D12 パススルー)
Vulkan が拡張を欠くことがある。X11 を強制し、必要ならエミュレート GPU を許可する:

```sh
WAYLAND_DISPLAY= ZED_ALLOW_EMULATED_GPU=1 cargo run -p mcpr-gpui
```

それでも描画できない場合は Windows 側で native ビルドする(コードは共通)。

## 依存の rev 固定と更新

`gpui` / `gpui_platform` は gpui-component と**同じ rev 無し git 宣言**にしてソースを
一本化している(rev を付けると別ソース扱いで gpui が二重化し型不一致になる)。実際の
commit は `Cargo.lock` で固定する。

`gpui-component` を更新するとき:

1. `mcpr-gpui/Cargo.toml` の `gpui-component` / `gpui-component-assets` の `rev` を上げる
2. その rev の gpui-component 同梱 `Cargo.lock` が指す zed commit を確認し、揃える:
   ```sh
   cargo update -p gpui --precise <zed-commit>
   ```

現在の固定:

- gpui-component: `0775df394083c1ed74f36f846b78868d1267398f`
- zed (gpui): `c545fb67d0ce13e335bff76f7c08986000333f2c`(gpui-component 同梱 lock 由来)
