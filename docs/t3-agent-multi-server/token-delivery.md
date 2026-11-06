# T3 token の配送と更新(追加先の T3 Server)

状態: T3(オフラインで確かめられる範囲)まで実装済み。配送の接続方法・UID/GID・実際のコンテナでの動作は、実機の試験(tasks.md の T4)で決める。この文書の例のアドレス・パスは例示用(`192.0.2.x`、`/var/lib/miharashi/...`)で、実環境の値ではない。
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
 ② 配送(経路は T4 で決める) ───────────→  ホスト専用ディレクトリの token_file(worker が読む)
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

発行側(コンテナ内)での実行例(実環境のパスは T4 で決める。トークンは引数・環境変数・ログに出さない):

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
| 入力(どちらか 1 つ) | `--stdin`: 配送側が標準入力へ流す(共有ファイルの所有者に依存しない。経路は T4 で決める)。`--from-file`: ホスト側の受取用ディレクトリに届いたファイル。`--expect-uid` は必須(想像で決めず実機の値を明示)。**受取ツールは受け取り元のファイルを消さない**(下の「受け取り元の後始末」) |
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

## ローテーションの手順(運用)

前提: 新旧どちらの token でも、手順中は worker が止まらない(旧 token は失効するまで有効)。値を端末に表示・貼り付けしない。

1. **発行側(コンテナ)**: 上記の `T3_TOKEN_REVOKE_MODE=defer` で実行。権限確認(`orchestration:read` だけ)・コンテナ内からの一覧 API の確認・送り出し用 token_file への置き換えまで行う。新しいセッション ID を控える。
2. **配送**: 送り出し用 token_file を、ホストの受取用ディレクトリ(`--from-file`)または stdin(`--stdin`)へ渡す。経路・UID/GID は T4 で決める。この文書は経路を決めない。
3. **受取(ホスト)**: `miharashi t3-token-receive --server container --from-file <受取用のパス> --expect-uid <N> --expect-environment-id <ID>` を実行。コード 0 を確認する(ホストから見た一覧 API の成功と、原子的な置き換えまで完了)。受取ツールは受け取り元を消さない。
   - 受け取り元の後始末: 0 を確認したあと、**配送役**が自分の信頼境界でホスト側の受取用ファイルとコンテナ内の送り出し用ファイルを消す(上の「受け取り元の後始末」)。
4. **ホストで確認**: 30 秒以内に worker が読み直す。管理ページで `agents:<id>` の取得元が「正常」、トークン表が「正常」で更新時刻が今になっていること、Agent 画面の接続別の見出しが LIVE のままであることを確認する。
5. **旧 token の失効(コンテナ)**: `T3_TOKEN_REVOKE_MODE=only T3_TOKEN_LABEL=miharashi-agent-monitor-container T3_TOKEN_KEEP_ID=<手順 1 で控えた ID> miharashi-t3-token`。発行時と同じ `T3_TOKEN_LABEL` を**必ず付ける**(省略すると既定のラベル `miharashi-agent-monitor`、つまり primary のセッションを失効させてしまう)。同じラベルの、その ID 以外のセッションを失効させる。
6. 失効後にも取得が「正常」のままであることを確認する。

失敗時: 手順 3 が失敗したら旧 token が残るので、原因を直して手順 2 からやり直す(新しい token はコンテナ側に未使用のまま残るので、やり直す前に手順 1 を再実行してよい。同じラベルの古いものは手順 5 でまとめて失効する)。手順 4 で取得が失敗したら、旧 token はまだ有効なので、手順 5 を実行せず原因を調べる。

## 実機(T4)で決める・確かめること

この文書とコードは次を**決めていない**。実環境の値を想像で設定しない。

- 配送の接続方法(コンテナ → ホストの経路。`--stdin` で流す方式か、受取用ディレクトリ経由か)と、その認可の範囲。キオスク実行ユーザーに広い lxc 権限を与えない。
- 非特権コンテナの UID/GID の変換後の値(`--expect-uid` / `--expect-gid`)、受取用ディレクトリの所有者・権限。
- 追加先の固定アドレスまたは証明書検証付き HTTPS 名、待受とファイアウォール、T3 のバージョン(v0.1.0 以降)、`--expect-environment-id` の値。
- 受取ツールの実行の仕方(unit / timer / 配送側からの呼び出し)と更新の周期、失敗の通知。この PR には具体的な unit を含めない。
- 実際のコンテナで、手順 1〜6 を通しで 1 回行い、取得の継続・旧 token の失効後の継続・更新失敗の表示を確かめる(確認前は本番対応済みとしない)。
