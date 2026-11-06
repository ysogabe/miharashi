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

## T4 の実機確認(2026-11-06〜07。実IP・実IDは省略。本番の miharashi・timer・設定は未変更)
- コードの検証: `cargo test -p miharashi-app` 457 成功/失敗 0(ignore 3)、`bash systemd/test-miharashi-t3-token.sh` 成功、`cargo fmt --check` 成功、配送役の結合テスト `python3 -I systemd/test_miharashi_t3_remote_token.py` 18 件成功(偽の T3/lxc/CLI に、本物の受取ツールと実 worker)。
- 既存LXCリモート経由・実コンテナ(T3 v0.1.7/protocol 2): 配送役を一時設定(専用ラベル、期限 10 分、本番設定とは別)で 2 回実行。1 回目 終了コード 0(token 配置・実 worker での取得成功・失効 0 件)、2 回目 終了コード 0(古いセッション 1 件を失効)。置いた token_file での GET は 200、古い token は 401、新しい token は 200。受取先ディレクトリに一時ファイルなし。配送役の出力に token なし。試験セッションはすべて失効し、サーバーのセッション数は試験前の 8 件に戻った。
- 実サーバーと現ブランチ worker の**同時 2 接続**(`miharashi agents-check`、画面なし。primary は本番の token_file を読み取りだけで使用、コンテナ側は短命の試験 token、`agents-check` は別プロセスで本番の miharashi には影響なし): primary 38 スレッド・コンテナ 4 スレッドが同時に live。
  - 片側停止: コンテナ前段のローカル proxy を止める → コンテナだけ retrying(最終データは保持)、primary は live のまま。proxy 復帰 → コンテナが live に戻る。
  - 認証失敗: 試験 token をサーバーで失効 → コンテナだけ auth-required(401)、primary は live のまま。配送役で新しい token を配送 → 起動中の worker が token_file を読み直して live に自動復帰(約 20 秒)。
  - 全体を通して primary の状態変化は connecting → live の 1 回だけ。
- 本番の primary の token_file(0600・実行ユーザー所有・リンクなし・通常ファイル)は、新しい読み込み規則(リンク拒否・FD 検査)で worker が読めた。既存の primary timer は実行していない(secret の不要な更新を避けた)。既存の更新成功の証拠は、既存履歴(`miharashi-t3-token.service` の直近の実行は 11-02、終了コード 0・新しい token を配置・古いセッション 1 件を失効)と、設定互換のテスト(T1: `config-get agents.url`/`agents.token_file` の互換、`systemd/test-miharashi-t3-token.sh`)を別に扱う。
- 未確認(この環境から確認できない): 最終 GPU 合成での AC7 の可読性、VRM 実モデル併用、キオスクの実画面での 2 接続表示(アプリの run は画面を占有するため行っていない)。配送役の本番導入(unit/timer の配置、`extra_servers` の追記、miharashi の再起動、週次 timer の実行)は未実施のため、AC6 の「移行後の追加先の token 更新・再読み込み・更新失敗表示」を本番では未確認。
