# Miharashi

Miharashi（ミハラシ）は、Rustで実装するLinux専用キオスク・ダッシュボードです。X11・Wayland・ウィンドウマネージャーを使わず、DRM/KMSへ直接描画します。仕様・設計・LPに加え、PoCの実装(`crates/`)と検証記録(`docs/poc/`)を収録しています。

リポジトリ名・実行ファイル名は `miharashi`、systemdユニット名は `miharashi.service` に統一しています。CPU/GPUの型番、搭載メモリ容量、デバイス番号を特定の利用環境に固定しません。

## 現在の状況

| 区分 | 状況 |
|---|---|
| PoC-001 Direct Display | 完了。DRM/KMS直接描画、日本語・写真・グラフ、PC状況とフォトフレームの2画面。[検証記録](docs/poc/POC-001.md) |
| PoC-002 Kiosk Runtime | 実装・実機検証あり(seat/VT、systemd、設定、GeoFrame再接続)。24時間試験の結果と未確認事項は[検証記録](docs/poc/POC-002.md) |
| PoC-003 以降 | 未着手(画面プラグイン、エージェント監視、MVP)。[ロードマップ](https://github.com/ysogabe/miharashi/issues/3) |

LPの画面と数値はコンセプトプレビューで、実装の画面ではありません(実装の画面は検証記録の画像を参照してください)。AIエージェント監視・プラグイン・3画面構成は、LPの構想どおりにはまだ実装していません。

## ビルドと実行

```sh
cargo build --release          # Linux。libseat のために pkg-config と libseat-dev が必要(Debian: apt install pkg-config libseat-dev)
cargo test

./target/release/miharashi png --live --out work/out    # ヘッドレス: PNGを出力
./target/release/miharashi kms-info                     # DRMカード・コネクターの一覧と、設定で選ばれる表示先
./target/release/miharashi run --config config/example.toml   # 実機表示(直接オープンは video・input グループが必要)
```

常駐させる場合は `sudo systemd/install.sh <実行ユーザー>` で systemd サービスを導入します(root では動かしません)。設定は [`config/example.toml`](config/example.toml)、運用手順と復旧方法は [PoC-002](docs/poc/POC-002.md) を参照してください。

## 作成物

| 作成物 | 内容 |
|---|---|
| [PRD](docs/PRD.md) | 利用目的、合意した要件、MVP範囲、受け入れ条件、未確定事項 |
| [技術設計書](docs/DESIGN.md) | Rustワークスペース案、描画・データ・入力・DRM/seat管理、設定、検証順序 |
| [画面管理インターフェース](docs/DESIGN-screen-interface.md) | 画面を追加するための最小契約(Screen / Provider / World / App)、共通操作の優先順位、遷移、PoC-003 への引き継ぎ(Issue #9) |
| [LP構成書](docs/LP.md) | LPの役割、3画面の構成、想定FAQ、表示上の約束 |
| [LP](site/index.html) | 3画面を切り替えられるローカルのHTMLページ |

## LPの表示

`site/index.html` をブラウザーで開けます。外部フォント・画像・CDNへの接続は不要です。

ローカルサーバーを使う場合は、プロジェクトフォルダで次を実行します。

```sh
python3 -m http.server 18765 --bind 127.0.0.1
```

ブラウザーで `http://127.0.0.1:18765/site/` を開きます。ポートが使用中なら別の空きポートに変えてください。

プレビュー内の4/6で画面切替、0でメニュー、5で詳細表示を試せます。FAQは見出しをクリックして開きます。このブラウザー内の操作例は、Linux evdevの動作確認を意味しません。

## LPの確認結果

3画面の切替、0メニュー、4/6での項目選択、5での確定・詳細表示、矢印キーとNumpad4、FAQ開閉をブラウザーで確認しました。1280px/375pxの表示幅で横方向のはみ出しを確認し、JavaScript構文・ローカル参照リンク・12項目のFAQも確認しました。コンソールの警告・エラーは確認時点でありませんでした。

[画面構成のプレビュー画像](site/preview.jpg)

## 今後の順序

[ロードマップ(#3)](https://github.com/ysogabe/miharashi/issues/3)に従います。PoC-003(画面プラグイン)、PoC-004(エージェント監視)、MVPの順です。

PRDは要件の基準、技術設計書は実装案です。実機で未検証の項目を実装済みとして扱わず、結果が出た段階で文書を更新します。
