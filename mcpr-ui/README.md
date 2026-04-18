# mcpr-ui

Yew + Tailwind CSS + daisyUI でリプレイファイルを視覚的に閲覧・編集するフロントエンド。

## 初回セットアップ

WebAssembly ターゲットと [`trunk`](https://trunkrs.dev/) が必要:

```sh
rustup target add wasm32-unknown-unknown
cargo install --locked trunk
```

Tailwind CSS standalone バイナリと daisyUI plugin を取得（Node/npm 不要）:

```sh
cd mcpr-ui
curl -sL daisyui.com/fast | bash
```

これにより `mcpr-ui/` に `tailwindcss`, `daisyui.mjs`, `daisyui-theme.mjs` が配置される。
いずれも `.gitignore` 済み。

## 開発サーバ

```sh
cd mcpr-ui
trunk serve --open
```

`Trunk.toml` の build hook が `tailwindcss -i input.css -o dist/output.css` を自動実行する。

## ビルド

```sh
cd mcpr-ui
trunk build --release
```

`dist/` 以下に静的アセットが出力される。
