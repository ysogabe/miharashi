# T3 Agent 複数サーバー 設計

状態: 検討案。[PRD](prd.md) に対応。実装承認前。

## 今の作り
- `crates/miharashi-app/src/config.rs:966` の ConfigAgents はURL/token_fileを各1個持つ。
- `crates/miharashi-app/src/agents.rs:116` の AgentSnapshot は単一環境。`:142` のProviderはWorld.agentsを丸ごと置換し、`:150` でworkerを1本起動する。
- `crates/miharashi-app/src/app.rs:177` で監視を起動し、画面は1つ登録する。
- `crates/miharashi-app/src/app/screens/agents.rs:230` 以降はthread_id/project_idをキーに履歴・通知・親子・描画を管理する。staleや再同期も画面全体で扱う。
- `crates/miharashi-app/src/panel/admin.rs:393` 付近は既存agents.url編集を扱う。
- 既存概要設計 `docs/DESIGN-t3-agent-monitor.md` の複数環境を後続とする範囲を今回拡張する。

## 方針
独立worker + 集約Provider + 単一AgentScreenを採用する。サーバー切替や別画面は全接続を一目で見る目的に合わない。1本の直列workerは1台のtimeoutが他へ伝播するため採用しない。

## 構成とデータの流れ
設定を正規化 → 接続ごとのworker/Shared → 単一の集約Provider → World.agents（複数ServerSnapshot）→ AgentScreen（1枚）。
既存Providerを複数登録して同一Worldフィールドを上書きする構造にはしない。

ServerSnapshotは設定上の安定server_id、表示label、従来のAgentSnapshotを持つ。取得sequence/cursor/generation/last_success/errorは接続ごとに保持する。全体のstaleや合成sequenceは作らない。

## インターフェースと設定
既存 `[agents] url/token_file` をserver_id=`primary` の1台目としてそのまま維持し、`[[agents.extra_servers]]` に `id` / `label` / `url` / `token_file` を追加する。共通poll_secs/timeout_secs/stale_secs/recent_hoursも維持する。追加配列は省略/空を許容し、接続数は合計4台まで。idは1〜64文字のASCII英数字・ハイフン・アンダースコアで一意、primaryは予約。labelは省略時id、指定時はtrim後1〜32文字・制御文字なし・接続間で一意。1台目の表示名は primary を用いる。

例（実IP・秘密を含まない）:
```toml
[agents]
enabled = true
url = "http://192.0.2.10:3773"
token_file = "/etc/miharashi/t3-agent.token"

[[agents.extra_servers]]
id = "container"
label = "コンテナ"
url = "http://192.0.2.11:3773"
token_file = "/var/lib/miharashi/t3-container.token"
```
管理Webは既存ページ内で接続別の取得状態・token状態を表示する。初回は追加先の登録/編集をTOMLに限定する。既存agents.url編集とoverlay、allowed_hostsの制約を維持し、追加先は編集APIに公開しない。`config-get agents.url` / `agents.token_file` は従来どおりprimaryを返すため、既存token更新スクリプトの互換性を保てる。

## 識別・状態・通知
ThreadKey=(server_id,thread_id)、ProjectKey=(server_id,project_id)を型として導入する。seen/id_map/last_activity/goal_notified/pending_new/queue/focus/カードキー/親子参照をすべて点検する。IDを区切り文字で単純連結しない。
親子探索は同じserver_id内だけ。通知にはserver_idを保持し、順序はローカル受信時刻とserver_idで安定化する。再同期・stale移行による通知破棄は該当サーバーだけに限定する。descriptorのenvironment_idが変更された場合はその接続の履歴とカードをリセットし、別環境の古い表示が混ざるのを防ぐ。同じenvironment_idが複数設定から返る場合は重複接続として警告する（自動統合しない）。

## 1ページのレイアウト
既存カードボードの流れを保ち、プロジェクト見出しを「接続名 / プロジェクト名」、カードとフォーカス通知にも短い接続ラベルを付ける。画面上部に接続別の状態・件数を並べ、未接続のサーバーも残す。設定labelを優先し、descriptor由来の名前は補助情報とする。
画面全体で既存の重要状態優先順位を維持する。同順位の接続間公平化は初回には追加せず、既存の安定した順序に従う。容量超過は接続別の未表示件数を表示。古い接続のカードは古い旨を明記し通知対象から外す。幅不足ではラベルを省略表示し、重要な状態記号と件数を残す。自動巡回・ページ切替は導入しない。

## データと移行
永続データの移行は不要。既存設定を残したまま追加配列を書き足す。状態・通知はメモリ。失敗時は追加配列を外して再起動する。旧設定・既存overlay・config-get・token timerの動作が追加前後で変わらないことを検証する。

## 安全面とコンテナの接続
各workerは自分のURL/token_fileのみ使用。既存check_url、token権限検査、再読み込み、リダイレクト拒否を維持する。HTTPの名前解決制限を緩和せず、到達可能なIPまたはHTTPSを使う。トークン・認証ヘッダー・API本文をログに出さない。

最初の追加対象は `lxc-t3code` 内のt3-server。ホスト上で `lxc list lxc-t3code --format json` を試したが、LXDソケットpermission deniedで情報取得できなかった。IP、ポート、待受、バージョンは未確認。調査段階では設定変更・token発行は行わない。

実装前に権限のある管理経路でコンテナの待受と経路を確認し、descriptorおよび認証済みshellの互換性を確認する。token更新はコンテナ側の専用処理で発行し、ホスト側の受け取り役で検証・配置する方式を第一候補とするが、配送経路・所有者・更新失敗監視は未決定。既存timerをそのまま2台目に使うとホスト側のtokenを発行する危険がある。キオスク実行ユーザーへ広いlxc権限を与える案は採用しない。配送設計が決まるまでは手動配置で接続試験できるが、本番の自動更新完了とは扱わない。秘密の入力が必要な段階では専用の秘密入力経路を使う。

## テスト計画
- AC1,7: 同一画面に2台のfixture、長いlabel、多件数、小解像度、片側0件、両側0件を投入して描画と件数を確認。
- AC2: 独立mockサーバーで片側timeout/401/再同期を起こし、他方の更新継続と通知保持を確認。
- AC3: 同一IDと異なる親子関係を2台へ与え、表示・通知・削除・キャッシュが独立することを確認。
- AC4: 旧設定、extra_servers追加、空配列、予約/重複/不正id、空/長すぎる/重複label、上限超過、既存overlayとconfig-getの互換性をconfigテストで検証。
- AC5: 異なるtokenを要求する2台のmock、権限不正、URL制限、リダイレクトの既存保護を検証。
- AC6: 実機接続と片側停止復帰。秘密値や本文は記録せず結果と時刻を記録。
検証入口は `cargo test -p miharashi-app`。tokenスクリプト変更時のみ `bash systemd/test-miharashi-t3-token.sh` も実行する。

## リスクと対策
接続増加によるworker/通知負荷は初期2台で測定する。ラベル衝突は設定時に拒否し、IDとの混同を防ぐ。通知偏りは接続ごとの順番制御で抑える。コンテナ再作成によるIP変更は固定経路またはHTTPS名を運用段階で決める。実機確認が未完のまま本番対応済みとしない。

## 実装の順番
1. 設定の正規化・互換性・管理Webの接続別状態表示。
2. 独立workerと集約Provider、複合キー、接続別状態。
3. 単一画面の統合表示と通知・容量制御。
4. 回帰検証、コンテナ接続と認証更新経路の確定、実機確認。
設計と実装は同じブランチの1本のPRへまとめる。今回の検討だけでPRは作らない。

## レビュー反映（設計レビュー1回目）
既存設定をprimaryとして保持する追加形式を採用し、既存token更新の破壊を避ける。コンテナtokenの更新・失敗検知も今回の本番受け入れ条件に含める。Issueは依頼に指定がないため新設せず、目的1つのPRとする。

影響範囲に `panel/feeds.rs:111` のfrom_agents、`panel/admin.rs:451` のtoken_reports、`app/screens/agents/layout.rs:85` のanchor、`main.rs:640` のagents-demo、agent_demo.rs、`video/demo.rs:96` を追加する。feedは接続ごとの取得元IDを持たせ、管理Web/状態画面の集約時にも上書きしない。demoと動画fixtureも単一接続を複数snapshotの1要素として構築する。

画面同期状態はServerUiState（cursor/generation/stale/last_snapshot/observed_sync）として接続ごとに保持する。ChangeKey=(server_id,AgentChange.id)とし、カーソル比較は同じ接続内のみ。advanceは健全な接続の通知だけを進める。stable_idの元はenvironment_idではなく構造化したThreadKeyとする。ProviderでIDへ接頭辞を付ける代案もあるが、型でサーバー境界を明示し、通知や親子参照の取り違えを防ぐ方針を維持する。

レイアウト検証では容量N=4に対してAの承認待ち3件・実行中10件、Bの入力待ち1件を投入し、重要4件が表示され未表示がA=10/B=0となることを確認する。容量N未満の重要カードも既存優先順位に従う。VRM併用時の実容量Nはレイアウトから計算し、表示数+未表示数が各接続の候補数に一致することを検証する。

コンテナtoken更新の具体候補として、既存miharashi-t3-tokenをコンテナ内でT3_URL/T3_TOKEN_FILE/T3_CLI明示により動かし、専用共有ディレクトリへ書く方式を検討する。UID/GID、読取権限、原子的置換とrename後の共有可視性、失敗監視を実機確認するまでは方式確定としない。

## 安全面レビュー反映（1回目）
レビューの旧servers形式に対するtoken/overlay移行の指摘は、extra_servers追加方式への変更で解消する。既存config-get、overlayと既存timerの意味は変更しない。ただしtokenスクリプトの回帰テストは変更の有無にかかわらず必須とし、既存接続の更新成功も確認する。

コンテナ共有領域をworkerのtoken_fileに直接指定してはならない。ホスト側受け取り役だけが書ける専用ディレクトリへ配置する。受け取りはリンクを追跡しないopen（O_NOFOLLOW）と開いたFDのfstatで通常ファイルを検証し、1行・8192バイト以下を確認する。ホスト所有の一時ファイルを0600で作り、同一ディレクトリ内で原子的に置換する。受け取り元の親ディレクトリと信頼境界も固定する。workerのread_tokenも同様にリンクを追跡せず、FDから権限・所有者を確認して読むよう強化し、リンク差し替えのテストを追加する。非特権コンテナのUID/GID変換を考慮し、共有先の0600ファイルをそのままホストで読む案は採用しない。安全な配送経路が未確定ならAC6は未達とする。

更新時はT3 v0.1.0以降の `--scope orchestration:read` で発行し、対象サーバーでscopeとshell GET成功を確認する。新tokenの配送・worker再読込・取得成功をホスト側から確認してから旧tokenを失効する。既存の60秒固定待機を配送経路へそのまま適用しない。受取/更新失敗は接続別に管理Webで表示する。tokenの値を確認通知に含めない。

本番前に固定アドレスまたは証明書検証付きHTTPS名を確保し、待受/ファイアウォールは既存LAN接続を維持する（ユーザー指定）。監視ホスト限定への変更は行わない。DHCP再割当で別接続先へ秘密を送る構成は不可。証明書検証を無効化する設定は作らない。これらの環境整備は別途管理作業だが、確認完了を接続の前提とする。

設定検証では正規化origin（scheme/host/effective port）の重複、token_fileの重複、panel/admin/home_assistantのtoken_fileとの共有も拒否する。追加先のtoken_fileは絶対パス必須で、書いたまま実行に渡す（字面の正規化で`..`やシンボリックリンクの意味を変えない）。比較は別に行い、既存側の相対パスはcwd基準、リンクをたどってから`..`を解き、未作成部分は字面で畳み、実体を確認できる場合はinode（ハードリンク）でも同一ファイルを検出する。labelの一意性はlabel同士だけで見る。受取先ディレクトリの保護とworkerのリンク拒否で後からの取り違えも防ぐ。表示文字列は設定labelもdescriptor由来labelも既存タイトルの無害化処理を共用し、制御文字・双方向制御文字を除去、上限を適用する。descriptor名を識別キーにはしない。

environment_id変更の検出は現行workerのdescriptor再取得時（再接続時）に行う。常時の検出を保証しない。文書・ログに実IPや秘密を記録しない方針を維持し、画面の診断情報は必要な接続識別情報のみ表示し、エラー文字列に認証情報が混入しないよう整形する。

安全面指摘は必須4件・推奨6件・参考3件を採用（旧形式への指摘2件は追加方式への変更で解消）。設計レビューは必須3件・推奨5件を反映。簡素化案は既存設定維持・公平化後回し・VRM検証を採用し、ID文字列書換えは型による識別を優先して見送った。共有領域への直接token配置案は安全面指摘を踏まえホスト受取方式へ修正した。

## 実機調査後の方針更新
既存LXCリモートから対象コンテナへ到達し、固定LANアドレス・T3 v0.1.7・protocol 2を確認。ユーザーは既存LAN接続の維持を指定したため、ネットワークの公開範囲を狭める変更は行わない。短時間のorchestration:readセッションで受取/GET/ローテーション/旧token失効/環境ID不一致拒否を実サーバー確認し、試験セッションは失効済み。実IP・実環境IDは公開文書には含めない。
