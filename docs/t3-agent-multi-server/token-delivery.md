# T3 token の配送と更新(追加先の T3 Server)

状態: 受取ツール(T3)・配送役 `miharashi-t3-remote-token`・診断コマンド `miharashi agents-check`(T4)まで実装済み。実機(コンテナ・実サーバー)でローテーションと同時 2 接続を確認した(結果は [validation.md](validation.md))。**本番への導入(unit/timer の配置、設定の追記、再起動)は未実施**で、手順は末尾の「配送役の導入」。この文書の例のアドレス・パスは例示用(`192.0.2.x`、`/var/lib/miharashi/...`)で、実環境の値ではない。
関連: [PRD](prd.md) / [設計](design.md)(「安全面とコンテナの接続」「安全面レビュー反映」)/ [既存 primary の更新](../DESIGN-t3-token.md)

## 何を解決するか

追加先の T3 Server(最初はコンテナ内)の token は 30 日で切れ、延長の API がない。primary は `miharashi-t3-token`(週 1 回の timer)が同じホストで発行・配置・失効まで行うが、追加先ではそのまま使えない。

- 発行は追加先の T3 Server 側(`t3 auth session issue` はその Server のデータディレクトリを使う)。
- token_file を読むのは監視ホストの miharashi(worker)。別の場所で作った token を、ホストの worker が読める場所へ**検査して**渡す必要がある。
- コンテナとの共有ディレクトリを worker の `token_file` に直接指定してはいけない(設計「安全面レビュー反映」)。非特権コンテナでは UID/GID が変換され、共有先の 0600 ファイルはホストの実行ユーザーのものにならず、コンテナ側から差し替えられる窓も残る。

## 全体の流れ

```
追加先(コンテナ)                        監視ホスト
 ① 発行+権限確認+一覧 API の確認           ③ 受取ツール(miharashi t3-token-receive)
    (miharashi-t3-token,                      検査 → 対象 T3 に問い合わせ → 原子的に置く
     T3_TOKEN_REVOKE_MODE=defer)              ↓
 ② 配送(lxc exec + パイプ。下の「配送役」) ───────────→  ホスト専用ディレクトリの token_file(worker が読む)
                                            ↓ 30 秒以内に worker が読み直す
 ⑤ 古い token を失効  ←──────────────  ④ ホストで取得成功を確認(管理ページ・feeds)
    (T3_TOKEN_REVOKE_MODE=only)
```

①〜⑤の順序が重要で、**新しい token でホストからの取得が成功したと確かめてから古い token を失効させる**(失効を先にすると、更新の失敗で画面が止まる)。

## 既存スクリプト `systemd/miharashi-t3-token` の流用

| 使えるか | 内容 |
|---|---|
| 使える | 発行(`t3 auth session issue --scope orchestration:read --label ... --json`)、権限が `orchestration:read` だけか確認、`GET /api/orchestration/shell` の確認、同じディレクトリの一時ファイル+`mv` での置き換え。`T3_URL` / `T3_TOKEN_FILE` / `T3_CLI` / `T3_TOKEN_LABEL` を明示すれば設定ファイルや `miharashi` は不要 |
| 使えない(そのままでは) | 手順 5 の「60 秒待って古いセッションを失効」。60 秒は同じホストの worker が token_file を読み直す時間の想定で、別ホストが受け取る運用では受け取る前に失効しうる |
| 追加した | `T3_TOKEN_REVOKE_MODE`: `wait`(既定。従来どおり)/ `defer`(発行〜置き換えまで行い、新しいセッション ID を表示して終わる。失効しない)/ `only`(`T3_TOKEN_KEEP_ID` 以外の同じラベルのセッションを失効だけ行う。待たない。`T3_TOKEN_LABEL` は発行時と同じ値を付ける) |

primary の timer・unit は変更していない(既定は `wait`)。`bash systemd/test-miharashi-t3-token.sh` が `defer` / `only` も含めて確かめる。

発行側(コンテナ内)での実行例(**T4 の実機では、この方式ではなく配送役がコンテナの t3 CLI を直接呼ぶ**。下の「配送役」。以下は `miharashi-t3-token` を `defer`/`only` で使う場合の参考。トークンは引数・環境変数・ログに出さない):

```bash
# ① コンテナ内、T3 を動かしているユーザーで。置き先はコンテナ内の「送り出し用」ディレクトリ(例示)
T3_URL=http://127.0.0.1:3773 \
T3_TOKEN_FILE=/var/lib/miharashi-outbox/t3-container.token \
T3_TOKEN_LABEL=miharashi-agent-monitor-container \
T3_TOKEN_REVOKE_MODE=defer \
  miharashi-t3-token
# 出力: 「新しいセッション ID は <ID> です」 ← ID は秘密ではない。⑤ で使うので控える
```

- `T3_URL` はコンテナ内から見た T3 の URL。監視ホストが使う URL(設定の `url`)とは別でよい。
- ラベルは primary と別にする(同じラベルだと、primary の更新が追加先のセッションを失効させる)。
- `t3 auth session issue` / `list` / `revoke` の引数は `t3 ... --help` で確認済み(v0.1.7): `issue [--scope <s>]... [--label <l>] [--ttl <t>] [--json|--token-only]`、`list [--json]`、`revoke <session-id>`(session-id は位置引数)。

## ホスト側の受取ツール `miharashi t3-token-receive`

```
miharashi t3-token-receive --server <id> \
    ( --stdin | --from-file <絶対パス> --expect-uid <N> [--expect-gid <N>] ) \
    [--expect-environment-id <ID>] [--config FILE]
```

| 項目 | 内容 |
|---|---|
| 置き先 | `--server` の id を `[[agents.extra_servers]]` から引き、その `token_file` に置く。コマンドラインでパスや URL は受け取らない(取り違えない)。`primary` は断る |
| 入力(どちらか 1 つ) | `--stdin`: 配送側が標準入力へ流す(共有ファイルの所有者に依存しない。T4 の配送役はこちらを使う)。`--from-file`: ホスト側の受取用ディレクトリに届いたファイル。`--expect-uid` は必須(想像で決めず実機の値を明示)。**受取ツールは受け取り元のファイルを消さない**(下の「受け取り元の後始末」) |
| 秘密を渡さない | token は引数・環境変数に載せない(stdin かファイルで渡す) |
| 検査(入力) | リンクを追跡せず開く(`O_NOFOLLOW`)・FIFO で固まらない(`O_NONBLOCK`)・開いた FD の `fstat` で通常ファイル・所有者(`--expect-uid`)・権限(所有者以外に権限がない)・ハードリンクなし・置き先と別のディレクトリ → 同じ FD から 8192 バイトまで読む → 空白・制御文字・非 ASCII を含まない 1 語(複数行や、空白を含む設定行は不可) |
| 検査(置き先) | `token_file` は絶対パスで、ディレクトリはリンクや `..` を含まない正規の形。ディレクトリは実行ユーザー所有で、所有者以外に書き込み権限がない(0700 / 0750)。ディレクトリは `O_DIRECTORY\|O_NOFOLLOW` で開いた FD を基準に作成・置き換える。置き先のファイル名がリンクなら、リンク先を書き換えずリンクごと置き換える |
| 取り違え防止 | 他の接続・`[panel]` / `[admin]` / `[home_assistant]` の token と同じ値なら置かない(比べ方は下の「他の用途との比較」)。対象の T3 には先に認証なしで環境情報を取り、`--expect-environment-id` と違えば token を送らない。リダイレクトは追跡しない。URL は worker と同じ `check_url`(平文 HTTP は IP か localhost のみ) |
| 確認 | 受け取った token で `GET /api/orchestration/shell` が 200 になることを、**ホストから**確かめてから置く。401 / 403 / 302 / 接続失敗は置かない |
| 置き方 | 保存する形(token + 改行)が worker の規則(改行込みで 8192 バイト以内・1 語)に収まるかを**入れ替え前に**確かめる(改行なしで 8192 バイトの token は保存すると 8193 バイトで読めなくなるので、ネットワークにも出さず断る)。同じディレクトリに 0600 の一時ファイル(`O_EXCL\|O_NOFOLLOW`)→ `fsync` → `renameat` で原子的に置き換え → ディレクトリを `fsync`。置いたあと worker と同じ規則で読み直して照合。入れ替えより前の失敗では、それまでの token_file は変えず、一時ファイルも残さない。入れ替えのあとの読み直しだけが失敗したときは `installed-unchecked`(token_file は新しい token に替わっている。旧 token には戻さない) |
| 終了コード | 0 = 置いた / 1 = 受け取りの失敗 / 2 = 使い方の誤り(設定が読めない場合を含む) |

### 他の用途との比較

受け取った token が他の接続・`[panel]` / `[admin]` / `[home_assistant]` の token と同じ値でないかを、**token をネットワークへ送る前に**比べる。

- 比べ方は、その用途が実際にファイルを開くのに合わせて**リンクを追跡して**読む(パネル・Home Assistant の token_file はリンクでも使える)。前後の空白を落として比べる。
- **意図的な欠落として比較なしで進むのは、ファイルまたは途中のディレクトリが存在しない場合(`ENOENT`。ぶら下がったリンクを含む)だけ**。まだ置かれていないトークン、`/etc/miharashi/` が無い環境の既定の設定はこれに当たり、受け取れる。
- それ以外で比べられないとき(権限で読めない・通常のファイルではない・8192 バイトを超える・リンクの循環・パスの途中がファイル(`ENOTDIR`)・I/O エラー)は `compare-failed` で**token を送る前に止める**(安全側)。読めない権限のファイルが他の用途にあるときは、受取ツールを読める実行ユーザーで動かすか、権限を直す。
- 限界: 比べられるのは「受取ツールの実行ユーザーが今、読める値」だけ。別ユーザーしか読めない設定は比べられないので止める(上の規則)。

限界: 一覧 API が 200 を返すことまでは確かめるが、token の**権限が `orchestration:read` だけか**は受取側から安全に確かめられない(権限が広い token でも 200 になる。書き込み系 API で試すと実際に作用しうるので試さない)。権限は発行側(`miharashi-t3-token` の手順 2。`scopes` が `["orchestration:read"]` だけでなければ置き換えない)が確認する。

### 受け取りの失敗の見分け方

失敗は stderr に `t3-token-receive: <コード>: <理由>` の 1 行で出る(systemd の unit から呼べば journal に残る)。token の値は出ない。

| コード | 意味 | 見るところ |
|---|---|---|
| `unknown-server` / `primary-refused` | `--server` が設定にない / primary | 設定の `[[agents.extra_servers]]` の id |
| `bad-destination` | 置き先のディレクトリが安全でない(所有者・権限・リンク・存在) | ディレクトリを 0700、実行ユーザー所有、リンクなしの絶対パスに |
| `source-unsafe` / `source-owner` / `source-mode` / `source-hardlink` / `source-in-destination-dir` | 受け取り元がリンク・特殊ファイル・大きすぎる・所有者違い・権限が緩い・ハードリンク・置き先と同じディレクトリ | 配送側の書き方、`--expect-uid` の値 |
| `bad-token` | 空・複数行・空白を含む・保存すると改行込みで 8192 バイトを超える | 発行側の出力(token だけを 1 行で、8191 文字まで) |
| `duplicate-token` | 他の接続・用途と同じ値 | 配送の取り違え(primary の token を送っていないか) |
| `compare-failed` | 他の用途の token_file が存在するのに比べられない(token は送っていない) | そのファイルの権限・種類・大きさ、受取ツールの実行ユーザー |
| `bad-url` / `unreachable` | 接続先 URL が不正 / 届かない | ホストから対象への到達性(T4 で固定アドレスと待受を確認) |
| `environment-mismatch` | 接続先の environmentId が期待と違う(token は送っていない) | 接続先が別のサーバーになっていないか(IP の再割り当て) |
| `unauthorized-401` / `forbidden-403` | 接続先が token を拒否 | 発行したサーバーと `url` が同じか、権限、期限 |
| `rejected` | リダイレクトなど想定外の応答 | 接続先の前段にリバースプロキシなどがないか |
| `install-failed` | 置き換えに失敗(それまでの token_file は無事) | ディレクトリの空き・権限 |
| `installed-unchecked` | 置き換えは済んだが、置いたあとの読み直しで不備(token_file は**新しい token に替わっている**。旧 token には戻さない) | token_file の所有者・権限・内容。直せなければ手順 1 からやり直す |

更新が止まっていることは、管理ページの「トークン」表で接続ごとの「ファイルの更新」時刻(`tokens[].modified_at`)で気づける(primary は週 1 回、追加先も同じ周期で更新する想定。30 日の期限より十分前に気づけるよう、2 週間以上更新がないものは更新の失敗とみなして調べる)。

### 受け取り元の後始末(配送役の仕事)

受取ツールは受け取り元のファイルを**消さない**。成功後にパスで消すと、そのパスの途中のディレクトリが確認の最中に別の場所へのリンクに差し替えられていた場合、別のホストのファイルを受取役の権限で消してしまうため(localhost の mock で再現して確認した)。

- 受け取ったあとの token は、置いた token_file と同じ値で、失効させるまで有効。残った受け取り元のファイルは生きた資格情報なので、必ず後始末する。
- 後始末は、そのファイルを作った**配送役**が、自分の信頼境界(自分の受取用ディレクトリを `O_DIRECTORY|O_NOFOLLOW` で開いた FD を基準に `unlinkat`、またはコンテナ内の送り出し用ファイルをコンテナ内で `rm`)で、受取ツールが終了コード 0 を返したあとに行う。
- 後始末を要らなくする `--stdin` を勧める(ファイルが残らない)。`--from-file` の場合は、受取用ディレクトリを配送役だけが書ける場所にし、配送役の側で消す。
- 受取ツールは `--from-file` が成功したとき、消していないことを stderr に 1 行出す。

## ローテーションの手順(運用。手動で行う場合。自動化した配送役は下の「配送役」)

前提: 新旧どちらの token でも、手順中は worker が止まらない(旧 token は失効するまで有効)。値を端末に表示・貼り付けしない。

1. **発行側(コンテナ)**: 上記の `T3_TOKEN_REVOKE_MODE=defer` で実行。権限確認(`orchestration:read` だけ)・コンテナ内からの一覧 API の確認・送り出し用 token_file への置き換えまで行う。新しいセッション ID を控える。
2. **配送**: 送り出し用 token_file を、ホストの受取用ディレクトリ(`--from-file`)または stdin(`--stdin`)へ渡す。経路・UID/GID は T4 で決める。この文書は経路を決めない。
3. **受取(ホスト)**: `miharashi t3-token-receive --server container --from-file <受取用のパス> --expect-uid <N> --expect-environment-id <ID>` を実行。コード 0 を確認する(ホストから見た一覧 API の成功と、原子的な置き換えまで完了)。受取ツールは受け取り元を消さない。
   - 受け取り元の後始末: 0 を確認したあと、**配送役**が自分の信頼境界でホスト側の受取用ファイルとコンテナ内の送り出し用ファイルを消す(上の「受け取り元の後始末」)。
4. **ホストで確認**: 30 秒以内に worker が読み直す。管理ページで `agents:<id>` の取得元が「正常」、トークン表が「正常」で更新時刻が今になっていること、Agent 画面の接続別の見出しが LIVE のままであることを確認する。
5. **旧 token の失効(コンテナ)**: `T3_TOKEN_REVOKE_MODE=only T3_TOKEN_LABEL=miharashi-agent-monitor-container T3_TOKEN_KEEP_ID=<手順 1 で控えた ID> miharashi-t3-token`。発行時と同じ `T3_TOKEN_LABEL` を**必ず付ける**(省略すると既定のラベル `miharashi-agent-monitor`、つまり primary のセッションを失効させてしまう)。同じラベルの、その ID 以外のセッションを失効させる。
6. 失効後にも取得が「正常」のままであることを確認する。

失敗時: 手順 3 が置換前に失敗したら旧 token が残るので、原因を直して手順 2 からやり直す(新しい token はコンテナ側に未使用のまま残るので、やり直す前に手順 1 を再実行してよい。同じラベルの古いものは手順 5 でまとめて失効する)。手順 4 で取得が失敗したら、旧 token はまだ有効なので、手順 5 を実行せず原因を調べる。

## 配送役 `miharashi-t3-remote-token`(T4)

既存の LXC リモートの権限で、コンテナ内の t3 CLI を実行する**管理用の配送役**。`systemd/miharashi-t3-remote-token`(Python 3)と、unit/timer のテンプレート `miharashi-t3-remote-token@.service` / `@.timer`(インスタンス名 = 接続 id)、設定例 `miharashi-t3-remote-token.env.example`。

### 決めたこと(T4 の設計調整)

| 項目 | 決定 |
|---|---|
| 配送の経路 | `lxc exec`(既存のリモート)でコンテナ内の `t3 auth session issue` を実行し、標準出力で受けた JSON から token を取り出して、**パイプ(標準入力)で** `miharashi t3-token-receive --stdin` へ流す。共有ディレクトリ・`--from-file` は使わない(UID/GID の変換値も要らない)。token は引数・環境変数・ログ・一時ファイルに出さず、受取のあとバッファを 0 で潰す |
| 権限 | 新しい lxc 権限は付与しない。配送役は**既存の LXC クライアント設定を持つユーザー**(実機ではキオスクと同じ一般ユーザー)で動かす。受取ツールが置く token_file をキオスクの worker が読めるよう、所有者を同じにする必要があるため。限界: 同じユーザーなので、キオスクのプロセスも原理的には `~/.config/lxc` を読める(unit では配送役側だけが使う。権限を分けたい場合は別ユーザーにして、token_file を `root:<キオスクのグループ> 0640` で置く運用が要るが、この PR では扱わない) |
| コンテナ内の実行 | `exec_mode=su`(既定): `lxc exec -T -n <remote>:<container> -- su - <user> -c '<各語を引用した 1 本の文字列>'`。`exec_mode=direct`: `--user/--group/--env HOME=/--cwd` でシェルを通さず直接実行。どちらも実機で動作確認。値は実行前に厳格な正規表現で検証する(`;`・空白・`$()`・先頭 `-`・`..` などは設定の誤りとして拒否) |
| t3 CLI の引数 | 実機の `--help`(v0.1.7)で確認: `auth session issue [--scope <s>] [--ttl <t>] [--label <l>] [--json]`、`auth session list --json`、`auth session revoke <session-id>` |
| 検証ゲート | 実際の worker のコードを使う `miharashi agents-check --server <id> --expect-environment <id>=<environmentId>` が、**置いた token_file を worker と同じ読み方(リンク拒否・所有者・権限・1 語)で読み**、同じ取得処理で成功することを確かめる。管理ページの `lastConnectedAt` は使えない(実機で、ベアラー token による一覧 API では更新されないことを確認) |
| 失効の対象 | 同じラベルで、**権限が `orchestration:read` だけ**のベアラー token のセッションだけ。ラベルが他の権限のセッションと衝突していれば、発行する前に止める。primary の既定ラベル(`miharashi-agent-monitor`)は使えない。primary の token・設定・既存の timer には触れない(`config-get agents.url` も使わない) |
| 診断コマンド | `miharashi agents-check` は画面・DRM・パネルを起動せず(ディスプレイを占有しない)、選んだ接続の worker だけを別プロセスで動かして、接続ごとの状態を 1 行ずつ出す。`--watch` で状態の変化を観察できる。起動中の miharashi には影響しない |

### 手順と失敗時の扱い

1. 排他ロック(状態ディレクトリの `<id>.lock`)。取れなければ何もせず終了コード 75。
2. コンテナで `session list`: ラベルが専用か確認(他の権限のセッションがあれば終了コード 2)。
3. `session issue --scope orchestration:read --label <専用ラベル> [--ttl ..] --json`: 返った権限が `["orchestration:read"]` だけで、token が 1 語の形式であること。違えば**今作ったセッションを失効**して失敗。応答を受け取れない(タイムアウト・不正)ときも、同じラベルで増えたセッションを失効する。
4. 受取(`--stdin`): 受取ツールがホストから対象の T3 に問い合わせ、環境 ID を照合し、token_file を原子的に置き換える。
   - 置換前の失敗(環境 ID の違い・401・複製 token など): 旧 token と token_file は変わらないので、**今作ったセッションを失効**して終了コード 1。
   - `installed-unchecked`(置換後の読み直しの失敗)・受取のタイムアウト(置換されたか不明): **新旧どちらも失効しない**。終了コード 3(要調査)。
5. `agents-check`(実 worker で取得): 失敗なら**古いセッションを失効しない**。終了コード 3。
6. 起動中の miharashi の worker が token_file を読み直す時間(`T3R_REVOKE_GRACE`、既定 70 秒 > 30 秒ごとの読み直し)を待つ。
7. 一覧を取り直し(失敗なら失効せず終了コード 4)、新しいセッションが一覧にあることを確かめてから、同じラベル・読み取り権限だけの古いセッションを失効。もう一度一覧して残りがないことを確かめる。残れば終了コード 4(新しい token は有効。次の実行が同じラベルの残りをまとめて失効させる)。

中断(SIGTERM/SIGINT、systemd のタイムアウト): 受取に渡す前なら今作ったセッションを失効、それ以降は失効しない。子プロセス(lxc・受取・確認)は個別のタイムアウトでグループごと止める。状態は `<状態ディレクトリ>/<id>.state.json`(0600、秘密なし。結果・段階・新しいセッション ID・失効したセッション・残り・短い理由)に残り、`miharashi-t3-remote-token --status` で読める。終了コード 0 以外は unit が failed になるので、`systemctl --failed` や状況画面の failed ユニット数で気づける。

終了コード: 0 完了 / 1 失敗(旧 token と token_file は変わらない)/ 2 設定・使い方の誤り(何も実行していない)/ 3 token_file は新しい token に替わったが確認が終わっていない(要調査・何も失効していない)/ 4 更新は成功したが古いセッションが残っている / 75 別の実行が動いている。

### 限界(正直に)

- 起動中の miharashi の worker が新しい token を使い始めたことを、配送役は**直接は観測しない**。`agents-check` は同じコードの別プロセスで、置いた token_file からの取得成功を確かめる。起動中の worker は `token_file` を 30 秒ごとに読み直し、401 を受けても読み直して復帰する(実機の同時 2 接続の試験で、失効後に新しい token へ自動復帰することを確認)ので、古い token を失効させたあとも止まらないが、運用では管理ページの「トークン」表(`modified_at`)と取得状態(`agents:<id>`)で確かめる。
- 失効の確認は新旧の token での GET ではなく、コンテナの一覧(`session list`)でセッションが無くなったことで行う(古い token の値は配送役が保持しないため)。実機試験では、試験用の古い token で 401 になることを別に確かめた。
- 配送役は `/etc/miharashi/config.toml` を読む。管理ページの上書き(overlay)で `extra_servers` は変えられないが、`agents.url` などは上書きされうる。受取・確認は上書きを含む「次回起動の値」で動く。

## 配送役の導入(この PR では実施しない)

root で実施する本番作業。**途中でキオスク(`miharashi.service`)を再起動するのは、設定に `[[agents.extra_servers]]` を足した最後の 1 回だけ**で、ユーザーの確認を取ってから行う。

1. 新しい `miharashi`(`agents-check` を含む)を入れる。キオスクの入れ替えは別の手順(通常の本番反映)で、再起動を伴う。
2. スクリプトと unit を置く(`systemd/install.sh` には入れていない):
   ```bash
   sudo install -m 0755 systemd/miharashi-t3-remote-token /usr/local/bin/miharashi-t3-remote-token
   sed -e 's|@USER@|<キオスクの実行ユーザー>|' -e 's|@BINDIR@|/usr/local/bin|' systemd/miharashi-t3-remote-token@.service | sudo tee /etc/systemd/system/miharashi-t3-remote-token@.service >/dev/null
   sudo install -m 0644 systemd/miharashi-t3-remote-token@.timer /etc/systemd/system/miharashi-t3-remote-token@.timer
   ```
3. 接続ごとの設定: `sudo install -d -m 0750 -o root -g <グループ> /etc/miharashi/t3-remote-token`、`miharashi-t3-remote-token.env.example` を `<id>.env`(root:<グループ> 0640)として置き、リモート名・コンテナ名・environmentId・ユーザー・CLI を書く。
4. 設定ファイルに `[[agents.extra_servers]]`(`id` / `label` / `url` / `token_file`。token_file は実行ユーザー所有の 0700 ディレクトリ内、例 `/var/lib/miharashi/t3-<id>.token`)を足す。まだ miharashi は再起動しない。
5. 手で 1 回: `systemctl start miharashi-t3-remote-token@<id>`、`journalctl -u ...`、`miharashi-t3-remote-token --status`(環境変数は env ファイルと同じ)。0 で終わり、`token_file` が置かれ、`miharashi agents-check --server <id> --expect-environment <id>=<environmentId>` が成功することを確かめる。
6. miharashi を再起動(ユーザー確認後)。管理ページで `agents:<id>` が「正常」、トークン表が「正常」であること、Agent 画面に両方の接続が出ることを確認。
7. `systemctl enable --now miharashi-t3-remote-token@<id>.timer`。primary の `miharashi-t3-token.timer` は変えない。
8. 元に戻す: timer を止め、設定の `[[agents.extra_servers]]` を外して再起動。配送役が発行したセッションは、ラベルを指定してコンテナで `t3 auth session list` → `revoke`。

## テスト

- `cargo build -p miharashi-app && python3 -I systemd/test_miharashi_t3_remote_token.py`(18 件): 本物の受取ツールと実 worker(`agents-check`)に、偽の T3 Server・lxc・t3 CLI を組み合わせた結合テスト。ローテーション(su / direct)、発行コマンドの正確さ、権限が広いときの失効、受取失敗・環境 ID 違いでの旧 token 維持と新セッション失効、worker 確認の失敗で何も失効しない、失効の一部失敗と次の実行での回復、一覧失敗で失効しない、置換後の確認失敗・受取タイムアウト・token 出力の伏せ字、SIGTERM、二重実行(75)・タイムアウト・発行後の応答喪失の後始末、設定の注入拒否(21 通り)、引用の往復、primary に触れないこと、token が引数・環境変数・出力・状態・一時ファイルのどこにも残らないこと。
- `cargo test -p miharashi-app`(`agents-check` の引数・判定を含む)、`bash systemd/test-miharashi-t3-token.sh`(既存の primary 用スクリプト。変更なし)。
