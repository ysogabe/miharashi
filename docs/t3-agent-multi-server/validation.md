# 検証状況

2026-11-06。実機未確認のため本番対応完了ではない。

## コード検証
- 96b6e98: `cargo test -p miharashi-app -p miharashi-render` — app 452成功/失敗0/ignore3、render 35成功。
- `cargo check --workspace --all-targets`、`cargo fmt --check`、`bash systemd/test-miharashi-t3-token.sh` 成功。
- 最終AC確認のAC2指摘（障害直後の保持カード表示）を修正。401/切断後5秒時点で障害側だけ「前回取得時」、復帰後解除を回帰確認。通知停止条件はstaleのまま。

## 全体レビュー
SolのPRD突き合わせ: AC1/3/4/5/8達成。AC2の表示指摘は上記修正で対応。AC6は未達、AC7の最終GPU合成可読性は未確認。

Opus `security-review`: High 0、Medium 0。候補なしのため誤検知確認の追加段階なし。安全面指摘の修正ではない最終表示変更後の再実行は省略。

```text
PREFLIGHT OK repo=/home/hermes/.t3/worktrees/miharashi/t3-026f8469 head=96b6e98417fd02d35df7a82a11ffc8a6a0f01ef0 base=origin/main files=34
```

## 実機確認の未完了
通常権限とsudoの両方でLXDソケットが存在せず、指定コンテナの情報を取得できない。コンテナが動くホストまたは管理接続方法をユーザーに確認中。

残る確認: 接続先固定、待受/ファイアウォール、T3バージョン、読取scope、UID/GIDと配送経路・受取unit、送り出し元の後始末、既存timerと追加先ローテーション、停止復帰、最終GPU合成時のVRM可読性。秘密値は記録しない。これらを確認するまでAC6および実機項目を完了とせず、マージも行わない。
