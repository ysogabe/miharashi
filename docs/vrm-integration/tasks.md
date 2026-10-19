# Issue #71 B VRM を Miharashi に組み込む タスク

- 要求: Issue #71 の「B. Miharashi への組み込み」と完了条件。設計: `docs/DESIGN-screen-plugin-v2.md` §11.2(手順)、§2・§3・§9・§10。前提の #74 は PR #82 で完了(`docs/poc/POC-007.md`)。
- ブランチ: `feat/vrm-integration-71b`

## 決めたこと(既定)
- VRM は設定 `[vrm]` で有効にする(既定は無効)。モデルの場所は設定で指定し、リポジトリに入れない(ニケちゃんは同梱禁止)。
- 装飾として載せる画面は写真と Agent(設定で選べる)。位置は PoC-006 と同じ左 1/3。
- 身振り・口パク・表情は PoC-006 の演出(`photo_stage` / `agent_stage`)を使う。T3 Agent の状態との細かな連動はこの範囲では最小限(カードの方を向く程度)。

## 受け入れ条件
| AC | 内容 |
|---|---|
| AC1 | B の前の宿題: sparse accessor の `byteStride` の検査、SpringBone の node のないコライダーの添字ずれ、関節数 × コライダー数の上限。壊れた glb で落ちない |
| AC2 | `VrmScene: GlScene`(`miharashi-vrm` が `miharashi-glscene` に依存、app からは `default-features = false`)。層に描くだけで背景を塗らない。姿勢・SpringBone・morph の CPU 計算は `draw` の中。`Region` の名前の衝突を解消。`vrm-view` は今の経路のまま動く |
| AC3 | モデルの読み込みは別スレッド、GL への転送は最初の `draw`。モデルが無い・壊れていても 3D なしで動く |
| AC4 | `WithScene<S: Screen>`: 内側の画面のメソッドを委譲し、`Composite::Gl { base: 内側, .. }` で包む。`needs_gl() = false`、`id`・操作・巡回は内側のまま。GL なし・失敗中は内側の画面だけ(ScreenManager の既存の処理) |
| AC5 | 表情・口パク・身振りの入力が `tick` から届き、`scene_animating()` が VRM の動きを返す。消灯・非表示で止まる |
| AC6 | `[vrm]` の設定と検証、既定は無効で本番の動きを変えない。`png --renderer gles` の fixture(合成モデル) |
| AC7 | 実機: 写真 + VRM と Agent + VRM の巡回、アラート・メニューが体の手前、`renderer = cpu` で元の画面、壊れたモデルで落ちない。iGPU・CPU・VRAM+GTT の保持量、初期ロード時間、Canvas 下地のときの CPU と描画時間を測り、タイムアウト解放と `base_changed` の要否を決める |
| AC8 | 12 時間試験(#74 の分もまとめて) |
| AC9 | 実装・起動手順・設定・測定結果・判断を docs に残す(POC 記録と設計書の更新) |

## タスク
| ID | 内容 | AC | 依存 | 種類 | 状態 | 開始 SHA | コミット | メモ |
|---|---|---|---|---|---|---|---|---|
| T1 | B の前の宿題(sparse の stride、コライダーの添字、関節 × コライダーの上限) | AC1 | - | quick | 完了 | 2c48639 | cbc3259..31d5e21 | Opus のレビューを 5 回。宿題 3 件に加え、壊れた・悪意のある glb への上限を追加(bufferView・画像の範囲検査と自前のデコード、buffer の uri、個数・頂点・morph・画像の予算、ファイル 1 GiB、JSON チャンク 2 MiB、読み込み全体の panic 防御)。JSON の最悪入力で RSS 約 270 MB、ニケちゃんの画素は変わらず。ユーザーの了承で修正の上限を超えて 5 回目まで直した |
| T2 | `VrmScene: GlScene` と別スレッドの読み込み | AC2, AC3 | T1 | full | 完了 | 31d5e21 | b20e439..86869f2 | Sol のレビュー 2 回(1 回目で必須 2 件: 転送中・描画中の panic で GL オブジェクトが漏れる → 作りかけの控えを場面が持ち、Renderer を take しない)。`Region` は vrm 側を `StageRegion` に改名。ニケちゃん: 解析 60ms(別スレッド)、初回 draw 23ms、以後 1〜2ms、GL オブジェクト 79 個。T3 への申し送り: `input.t`/`dt` は呼び出し側が毎フレーム設定、`from_path` は構築時にスレッドを起動(cpu のときは作らない)、release 後は姿勢・SpringBone が初期化 |
| T3 | `WithScene`・`[vrm]` 設定・演出の入力・png fixture | AC4, AC5, AC6 | T2 | full | 完了 | 86869f2 | d860235..62a8e7c | Sol のレビュー 2 回(1 回目で必須 2 件: GL 失敗後も VRM の 33ms 間隔が残る → 内側が動いていれば内側の間隔、png の壁時計が固定されていない → 固定してバイト一致を試験)。包むのは `[vrm]` 有効・`screens` に含む・GL ありのときだけで、読み込みは初回表示で開始。時刻・身振り・口パクは tick、SpringBone の dt と頭の向きは render_composite。左 1/3 で手の先が切れる(REACH_FRAC は 0.4 以上でないと効かず顔が小さくなるので 0.0 のまま)→ T4 で region を選ぶ |
| T4 | 実機の確認と計測、判断(タイムアウト解放・base_changed)、記録 | AC7, AC9 | T3 | quick | 完了 | 8296e41 | 8296e41 + 本コミット | 実機で計測し [POC-008](../poc/POC-008.md) に記録。覆われている間 CPU 35%→4.5%(8296e41)。タイムアウト解放と base_changed は不要。目視・消灯復帰は未確認 |
| T5 | 12 時間試験 | AC8 | T4 | quick | 実行中 | 1e7a797 | | 本番の設定で VRM を有効にして行うので、始める前にユーザーに確認する |
