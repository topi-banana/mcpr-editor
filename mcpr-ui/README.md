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

## Cloudflare Pages へのデプロイ

`main` への push で `.github/workflows/deploy.yml` が動き、Direct Upload で Cloudflare Pages に成果物を転送する。

### 初回セットアップ

1. **Pages プロジェクトを作成**
   - Cloudflare ダッシュボード → Workers & Pages → Create → Pages → **Direct Upload** を選択
   - プロジェクト名を `mcpr-editor` で作成（workflow の `CF_PAGES_PROJECT` と合わせる）
   - Production branch は `main` のまま

2. **API token を作成**
   - My Profile → API Tokens → Create Token → Custom token
   - 権限に `Account > Cloudflare Pages: Edit` を付与（対象アカウントに絞る）
   - 発行されたトークンをコピー

3. **GitHub Secrets を登録**
   リポジトリ Settings → Secrets and variables → Actions で以下を追加:
   - `CLOUDFLARE_API_TOKEN`: 上で作ったトークン
   - `CLOUDFLARE_ACCOUNT_ID`: ダッシュボード右サイドバー or URL 中の Account ID

4. `main` に push するか Actions タブから **Deploy mcpr-ui to Cloudflare Pages** を手動実行する。
   成功すると `https://mcpr-editor.pages.dev/` で閲覧可能。

### プロジェクト名を変えたい場合

`.github/workflows/deploy.yml` の `env.CF_PAGES_PROJECT` を変更し、CF ダッシュボード側のプロジェクト名と一致させる。
