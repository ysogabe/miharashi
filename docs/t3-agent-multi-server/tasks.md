# T3 Agent 複数サーバー タスク

- PRD: [prd.md](prd.md) / 設計: [design.md](design.md)
- ブランチ: t3/t3-agent-multi-server-study
- 子タスク台帳: /tmp/t3-026f8469-children.tsv（Git管理領域は通常sandboxから書込不可）

| ID | 内容 | AC | 依存 | 種類 | 状態 | 開始 SHA | コミット | メモ |
|---|---|---|---|---|---|---|---|---|
| T1 | extra_servers設定・検証・旧設定/overlay/config-get/token更新の互換性 | AC4,AC5 | - | full | 完了 | 8626a78 | 9f4957c..c2409cd | 405件通過、tokenテスト成功。Sol再レビュー指摘なし。取得/UIはT2以降 |
| T2 | 独立worker・集約Provider・複合キー・単一画面・接続別通知・demo対応 | AC1,AC2,AC3,AC7 | T1 | full | 実装中 | c2409cd | | 画面追加なし |
| T3 | 管理Web/feedsの接続別状態・token読込保護・配送/更新の手順と検証 | AC5,AC8 | T2 | full | 未着手 | | | 既存ページを拡張 |
| T4 | コンテナ接続先/権限/公開範囲/更新経路の確認・実機確認と全体検証 | AC6 | T3 | full | 未着手 | | | LXDソケット権限は要確認。未確認を完了扱いしない |

タスクは同じブランチへコミットし、最後に設計書と実装を1本のPRへまとめる。
