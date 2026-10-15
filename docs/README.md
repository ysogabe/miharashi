# Miharashi — ドキュメント

プロダクトの紹介は [README](../README.md) と [LP](https://ysogabe.github.io/miharashi/) にあります。ここには、使い方と技術的な情報をまとめています。

## 使い方

| 文書 | 内容 |
|---|---|
| [使い方](USAGE.md) | ビルド、実行、systemd での常駐、設定、Home Assistant と操作パネルのつなぎ方 |

## 要件と設計

| 文書 | 内容 |
|---|---|
| [PRD](PRD.md) | 利用目的、合意した要件、MVP の範囲、受け入れ条件、未確定事項 |
| [技術設計書](DESIGN.md) | Rust ワークスペース、描画・データ・入力・DRM/seat 管理、設定、検証の順序 |
| [画面管理インターフェース](DESIGN-screen-interface.md) | 画面を追加するための最小の契約(Screen / Provider / World / App)、共通操作、遷移(Issue #9) |
| [画面の全体構成](DESIGN-screens.md) | 状況・写真・室内環境と天気・航空機の画面、データの取得元、無人時の巡回 |
| [T3 Agent 画面の概要設計](DESIGN-t3-agent-monitor.md) | 操作不要のカードボード、新着・変更・縮小の動き、T3 データフロー、認証、実装前の確認事項 |
| [次の画面の設計(PoC)](poc/DESIGN-next-screens.md) | 室内環境・天気・航空機などの調査と設計メモ |
| [監視のコスト](DESIGN-monitoring-cost.md) | 監視スレッドの読み取りごとの CPU 時間、読む間隔の決め方、監視するものを増やすときの手順 |
| [操作パネルの設計](panel/DESIGN.md) | Echo Show 5 を手元の操作盤にする設計。接続方式の比較、API(§5.2)、段階 |
| [LP 構成書](LP.md) | LP の役割、構成、載せる画像の約束、ローカルでの確認方法 |

## 検証記録

| 文書 | 内容 |
|---|---|
| [PoC-001 Direct Display](poc/POC-001.md) | DRM/KMS への直接描画、日本語・写真・グラフ、2 画面 |
| [PoC-002 Kiosk Runtime](poc/POC-002.md) | seat / VT、systemd、設定、写真サーバーへの再接続、12 時間試験。運用と復旧の手順 |
| [PoC-005 GPU Render Baseline](poc/POC-005.md) | 内蔵 GPU(OpenGL ES 3.2)での描画と、CPU 使用率の計測 |
| [T3 Agent カードボード — 実装・検証記録](poc/T3-AGENTS.md) | T3 Agent画面の実装状況、CPU/GPU合成検証、未確認事項 |

## 現在の状況

| 区分 | 状況 |
|---|---|
| PoC-001 Direct Display | 完了 |
| PoC-002 Kiosk Runtime | 完了(12 時間試験に合格) |
| PoC-005 GPU Render Baseline | 完了(写真画面の CPU 使用率 0.8%、重いエフェクトの予算は GPU 50% 以下)。[#16](https://github.com/ysogabe/miharashi/issues/16) |
| アラート・電源・写真・航空機 | 実装済み(地震・災害アラート、在宅判定でのモニター消灯、旅ごとの写真、航空機の画面) |
| 操作パネル | P2 まで完了(API、Echo Show 5 のアプリ、モニターの消灯・点灯、操作盤の自動消灯) |
| PoC-003 Screen Plugins | S1(`ScreenManager` / `Screen` への分割)と S2(4/6 = 画面の巡回、8/2 = 写真の前後、0 = メニュー、`POST /api/v1/screen`)、S3(航空機を独立した画面に)を実装。次は S4(室内環境と天気)。[#5](https://github.com/ysogabe/miharashi/issues/5) |
| 室内環境と天気・航空機の画面・自動巡回 | 未着手。[#30](https://github.com/ysogabe/miharashi/issues/30)、[#29](https://github.com/ysogabe/miharashi/issues/29)、[#31](https://github.com/ysogabe/miharashi/issues/31) |
| T3 Agent カードボード | 旧 Hermes 連携の PoC-004 は中止。別件として、T3 サーバーを取得元とする画面の[承認済み設計](DESIGN-t3-agent-monitor.md)に沿って実装・ローカル検証済み。21:9 PNGと仮データ動画を確認し、T3実サーバーの認証済みshell APIもGET疎通済み。試験トークンは失効済み。([検証記録](poc/T3-AGENTS.md)) |

全体の順序は [ロードマップ(#3)](https://github.com/ysogabe/miharashi/issues/3) に従います。PRD は要件の基準、技術設計書は実装案です。実機で確かめていない項目は実装済みとして扱わず、結果が出た段階で文書を更新します。
