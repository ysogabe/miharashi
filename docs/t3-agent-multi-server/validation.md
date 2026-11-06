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

## T4 レビュー修正の検証(2026-11-07。本番の miharashi・timer・設定・token は未変更)
指摘: 受取が負の戻りコードのとき新 token を失効しうる(P1)/発行済みで応答前の SIGTERM の後始末がない/別プロセスの `agents-check` と固定待ちでは稼働中の worker の成功を確認していない/配送結果が管理 Web に出ない/長い先行警告で置換後を置換前と誤判定。すべて採用して修正した(設計は [token-delivery.md](token-delivery.md)「稼働中の worker の観測」「更新結果の管理ページへの取り込み」、[design.md](design.md)「T4 レビュー修正」)。

- コードの検証: `cargo test -p miharashi-app -p miharashi-render` — app 468 成功/失敗 0/ignore 4(既存 3 + 手動の実機再現用 1)、render 35 成功。`cargo fmt --check`、`cargo check --workspace --all-targets`、`bash systemd/test-miharashi-t3-token.sh` 成功。配送役の結合テスト `python3 -I systemd/test_miharashi_t3_remote_token.py` 32 件成功(18 件から拡張)。
- 指摘別:
  1. 受取の戻りコードが負(SIGKILL/SIGTERM)・タイムアウト・想定外(101)・世代なしの受取ツールは、新旧とも失効せず終了コード 3(置換直後にシグナルを受ける回帰テストあり。token_file は新 token、新旧とも有効を確認)。
  2. 発行コマンド実行中の SIGTERM は、応答喪失と同じ後始末(発行前の一覧との差分を失効)。一覧が取れないときは `cleanup_pending`/残存の可能性を状態に残し、次の実行が回復する(テストあり)。
  3. 失効の条件を「稼働中の worker が配置した世代で取得に成功し LIVE」と管理 API で観測することに変更。古い世代での成功では失効しない/新しい世代で成功を観測して初めて失効/poll が遅い worker でも固定待ちで失効しない/観測できなければ新旧とも残す(終了コード 3)。実際の worker → 実際の管理 API の HTTP の一連を Rust のテスト(`a_real_worker_reports_its_token_revision_through_the_real_admin_api`)で確認。管理トークンは HTTP ヘッダーだけに載り、引数・環境変数・出力・状態・ファイルのどこにも出ない。
  4. 配送結果を接続別に管理 Web へ取り込み(`tokens[].rotation`、「更新の結果」列)。token・取得が正常でも更新失敗が出ること(AC8)、状態の無い接続・primary は従来のまま、壊れた/危険な状態ファイルは「読めない」、上限・JSON 検査・無害化をテストで確認。
  5. 置換後の不備は受取の専用終了コード 3 で判定。長い先行警告(3000 文字)のあとでも置換後と判定し、古い形(終了コード 1 + 目印)も標準エラー全体で判定。長い警告のあとの置換前の失敗は新セッションを失効する(回帰テストあり)。
- **実環境の再現(一時のヘッドレス worker + 管理 API。本番の miharashi とは別プロセス・別ポート・一時ディレクトリの設定)**: 既存 LXC リモート経由・実コンテナ(T3 v0.1.7)に対し、専用の試験ラベル・期限 15 分で配送役を 2 回実行。ヘッドレスは `cargo test -p miharashi-app -- --ignored manual_headless_worker_with_admin_api`(本物の worker と本物の管理 API を 127.0.0.1 の一時ポートで立てる)。1 回目 終了コード 0(受取 → 事前確認 → 稼働中 worker の世代を管理 API で観測 → 古いセッション 0 件)。2 回目 終了コード 0(約 20 秒で稼働中 worker が新世代で取得したと観測してから、古いセッション 1 件を失効)。管理 API の取得元は `ok`・`token_revision` を報告、トークン表の「更新の結果」は `ok`(終了コード 0・残り 0)。試験セッションは終了後にすべて失効し、サーバーのセッション数は試験前の 8 件に戻った。本番の primary の token_file は未変更。値(token・環境 ID・IP)は記録していない。
- **本番未導入のため未確認**: 実際のキオスク(`miharashi.service`)の worker と管理ページでの観測(本番は旧版で `token_revision` を報告しない)、`rotation_state_dir` を設定した本番の管理ページ表示、配送役 timer の週次実行、最終 GPU 合成での AC7、AC6 の「移行後の追加先 token 更新・再読み込み・更新失敗表示」の本番確認。上のヘッドレス再現は本番導入済みの確認の代わりにならない。


## 最終レビュー（3ea92f0）
- 通常レビュー(Sol)必須0。推奨の「未回収セッション情報の次回preflight失敗時の保持」は未採用。更新失敗そのものは表示される。未回収セッションは次の成功した実行で専用ラベルの範囲から回収される。診断詳細の持越しは改善余地として残す。
- Opus security-review: High0/Medium0。候補0のため追加の誤検知確認なし。
- 参考改善として発行失敗時のstderrは記録せず終了コードだけにし、管理APIの設定例をloopbackへ変更。
- 本番未反映のためAC6の本番項目・AC7の実画面項目は未達/未確認。PRはdraftとし、完了扱いにしない。

```text
PREFLIGHT OK repo=/home/hermes/.t3/worktrees/miharashi/t3-026f8469 head=3ea92f058f7c0fa6749ea575c056888f60e9001c base=origin/main files=43
```
