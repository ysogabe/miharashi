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
| [技術設計書](DESIGN.md) | 現行ソースの構成、描画・データ・入力・DRM/seat 管理、設定と運用上の制約 |
| [画面管理インターフェース](DESIGN-screen-interface.md) | 画面を追加するための最小の契約(Screen / Provider / World / App)、共通操作、遷移(Issue #9) |
| [画面の全体構成](DESIGN-screens.md) | 状況・写真・室内環境と天気・航空機・T3 Agent の画面、データの取得元、無人時の巡回 |
| [T3 Agent 画面の概要設計](DESIGN-t3-agent-monitor.md) | 操作不要のカードボード、新着・変更・縮小の動き、T3 データフロー、認証、実装前の確認事項 |
| [T3 Agent 画面のトークンの更新](DESIGN-t3-token.md) | T3 の認証の仕組み(30 日で切れる、延長できない、v0.1.0 から `session issue --scope` で権限を絞れる)と、読み取りだけのトークンを週 1 回作り直す仕組み |
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
| [PoC-006 VRM 描画の単体スパイク](poc/POC-006.md) | VRM 1.0 モデルを内蔵 GPU(GLES 3.2)で描くときの速さの計測(合成モデル) |
| [T3 Agent カードボード — 実装・検証記録](poc/T3-AGENTS.md) | T3 Agent画面の実装状況、CPU/GPU合成検証、未確認事項 |

## 現在の状況

| 区分 | 状況 |
|---|---|
| PoC-001 Direct Display | 完了 |
| PoC-002 Kiosk Runtime | 完了(12 時間試験に合格) |
| PoC-005 GPU Render Baseline | 完了(写真画面の CPU 使用率 0.8%、重いエフェクトの予算は GPU 50% 以下)。[#16](https://github.com/ysogabe/miharashi/issues/16) |
| アラート・電源・写真・航空機 | 実装済み(地震・災害アラート、在宅判定でのモニター消灯、旅ごとの写真、航空機の画面) |
| 操作パネル | P3 まで完了(API、Echo Show 5 のアプリ、モニターの消灯・点灯、操作盤の自動消灯、操作ラベル、メニューからの画面選択、巡回の状態表示) |
| PoC-003 Screen Plugins | S1〜S5 を実装・本番稼働(画面の分割、4/6 = 画面の巡回・8/2・5 = 画面の中・0 = メニュー、航空機の独立、室内環境と天気、自動巡回)。操作パネルの `/api/v1/status` に巡回の状態(#45)。キーボードはつながず操作パネルで操作する運用なので、キーボード固有の確認(終了キー、物理テンキーの 0・NumLock OFF)は対象外とし、完了。[#5](https://github.com/ysogabe/miharashi/issues/5) |
| 画面 | `status`・`photo`・`room`・`aircraft`・`agents` の 5 つ([画面の全体構成](DESIGN-screens.md))。アラートはすべての画面の上に重ねる層 |
| T3 Agent カードボード | PoC-004 は Hermes 連携をやめ、T3 向けに改訂した([#6](https://github.com/ysogabe/miharashi/issues/6))。T3 サーバーを取得元とする `agents` 画面を[承認済み設計](DESIGN-t3-agent-monitor.md)に沿って実装し、本番に配備済み(既定は無効)。トークンは週 1 回、読み取りだけのものに自動で作り直す([トークンの更新](DESIGN-t3-token.md))。([検証記録](poc/T3-AGENTS.md)) |
| MVP(日常運用) | 未完了。今の版での長時間試験、ブート時の自動起動、写真のディスクキャッシュ、テストの CI、モニターのつなぎ替え、MVP のリリースノートが残り。[#7](https://github.com/ysogabe/miharashi/issues/7) |

全体の順序は [ロードマップ(#3)](https://github.com/ysogabe/miharashi/issues/3) に従います。PRD は初期要件の基準、技術設計書は現在の実装の説明です。画面インターフェース文書は §0 が現行契約、後続の節には当初案と段階別の記録も残しています。PoC 文書は各試験時点の記録で、現在の版の再検証結果ではありません。実機で確かめていない項目は実装済みとして扱わず、結果が出た段階で文書を更新します。
