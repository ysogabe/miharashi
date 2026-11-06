# T3 Agent 複数サーバー タスク

- PRD: [prd.md](prd.md) / 設計: [design.md](design.md)
- ブランチ: t3/t3-agent-multi-server-study
- 子タスク台帳: /tmp/t3-026f8469-children.tsv（Git管理領域は通常sandboxから書込不可）

| ID | 内容 | AC | 依存 | 種類 | 状態 | 開始 SHA | コミット | メモ |
|---|---|---|---|---|---|---|---|---|
| T1 | extra_servers設定・検証・旧設定/overlay/config-get/token更新の互換性 | AC4,AC5 | - | full | 完了 | 8626a78 | 9f4957c..c2409cd | 405件通過、tokenテスト成功。Sol再レビュー指摘なし。取得/UIはT2以降 |
| T2 | 独立worker・集約Provider・複合キー・単一画面・接続別通知・demo対応 | AC1,AC2,AC3,AC7 | T1 | full | 完了 | c2409cd | 6805ecd..572e1e2 + 親の文字幅修正 | 画面1枚。WithScene配置検証済み、最終GPU合成の実機確認はT4。app425通過/管理Web429の1件は単独40件成功。再レビューW幅の指摘を実測へ修正し回帰テスト成功 |
| T3 | 管理Web/feedsの接続別状態・token読込保護・配送/更新の手順と検証 | AC5,AC8 | T2 | full | 完了 | 572e1e2 | 0d2676e..e724983 + 親のUnicode比較修正 | app452通過。受取18件再実行成功。Sol指摘はUnicode trim/エラー表示含め反映 |
| T4 | コンテナ接続先/権限/公開範囲/更新経路の確認・実機確認と全体検証 | AC6 | T3 | full | 完了 | 649e8cb | 9b7e7ee..3ea92f0 | 本番2台LIVE/GPU実フレーム/primary更新/追加更新を確認。ユーザー承認後に失敗表示と正常復旧を実機確認。Sol必須0、OpusHigh0/Medium0 |

タスクは同じブランチへコミットし、最後に設計書と実装を1本のPRへまとめる。

全体確認は [validation.md](validation.md) を参照。最終AC2の障害直後表示指摘を修正し、複数接続17件成功。全体app452/render35成功、安全面High0/Medium0。接続経路は確認済み。本番未確認のAC6/7を明示したdraft PRを作成し、マージは保留する。

全4タスク完了。意図的な本番更新失敗試験は追加承認を得て実施・復旧確認済み。VRMは本番設定で無効のまま。
