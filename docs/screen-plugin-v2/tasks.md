# Issue #74 GLES 3D 描画境界(キューブ画面)タスク

- 設計: `docs/DESIGN-screen-plugin-v2.md`(§11.1 が #74 の確認項目)。PRD は Issue #74 の本文。
- ブランチ: `feat/glscene-cube-74`

## 受け入れ条件(Issue #74 と設計書 §11.1 から)

| AC | 内容 |
|---|---|
| AC1 | `miharashi-glscene` クレート(依存は glow だけ)。Linux 以外でも `cargo check` が通る |
| AC2 | `Compositor` に層(色 + 深度の FBO)・`reset_gl_state`・`draw_scene`。`restore_gl_state` を移す。`SceneFrame` / `present_scene` / `release_scenes`。`Frame` は変えない。オフスクリーンでも同じ経路 |
| AC3 | `Composite::Gl` と `runtime.rs` の変換。最上段の重ね絵(`Top`)と ScreenManager の描画 |
| AC4 | `Screen::needs_gl` / `gl_scene` / `scene_animating`、`App::gl_scenes` / `report_scene_failure`。GL の可否は `kiosk()` から ScreenManager へ |
| AC5 | 故意の panic・`Err`・GL エラーでキオスクが止まらない。アラート表示中に失敗してもアラートが見え続ける。作りかけでも `release` が動く |
| AC6 | `renderer = cpu` と GL なしの `auto` で案内が出て巡回から外れる。動かない写真を毎フレーム CPU で描き直さない。`png` に fixture |
| AC7 | キューブ画面(深度テストあり、VRM に依存しない)。全面と 1/3 幅の `Region` |
| AC8 | 状態を汚す試験画面の直後に写真・Radar・Agent を描いて PNG が一致。2D→3D、3D→3D で状態が漏れない |
| AC9 | 実機(DRM/GBM/EGL)でキューブが描かれ、巡回・アラート・メニューが 3D の上に出る。残像がない |
| AC10 | 切り替え数百回で VRAM+GTT と `GlScene` 内の作成 − 削除の数が増えない |
| AC11 | 消灯中に描かない。復帰で動きがまとめて再生されない |
| AC12 | 720p / 1080p の fps・平均/p95 描画時間・CPU・iGPU。層の FBO を挟む追加コスト |
| AC13 | API の契約・性能・制限事項を docs に残す(設計書の更新と PoC 記録) |

12 時間試験は #71 B とまとめる(設計書 §11.1)。

## タスク

| ID | 内容 | AC | 依存 | 種類 | 状態 | 開始 SHA | コミット | メモ |
|---|---|---|---|---|---|---|---|---|
| T1 | `miharashi-glscene` クレートと、GPU 側(`Compositor` の層・状態の初期化・`present_scene`・失敗の検出・オフスクリーン)。試験用の場面でテスト | AC1, AC2, AC5(GPU 側) | - | full | 完了 | 96f8b20 | da98232..9d437fc | レビュー Sol 2 回。必須 2・推奨 1 を採用。2 回目の推奨(sampler の解除はユニット 0〜15 まで、と GlScene の doc に書く)は T2 で入れる。安全面のレビューは最後の全体レビューでまとめて行う |
| T2 | 画面・アプリ側(`Composite::Gl`・`Top`・Screen / App の口・`runtime.rs`・GL の可否と失敗の扱い・CPU 経路の案内と巡回・消灯) | AC3, AC4, AC5, AC6, AC11 | T1 | full | 完了 | 8df8074 | b634210..da7b4b2 | レビュー Sol 3 回。必須 4・推奨 1 を採用、見送りなし。GPU テストはこのマシンで skip なしで通ることを確認 |
| T3 | キューブ画面と状態を汚す試験画面、作成/削除の数え方、PNG の一致テスト | AC7, AC8, AC10(数え方) | T2 | quick | 実装中 | aede60f | | |
| T4 | 実機での確認と計測、PoC 記録と設計書の更新 | AC9, AC10, AC11, AC12, AC13 | T3 | quick | 未着手 | | | |
