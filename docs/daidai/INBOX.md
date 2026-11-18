# だいだいの通知の登録口(`miharashi-daidai-inbox`)

別の VM にいるだいだい(Muse)が、tailnet 越しに通知カードを登録するための小さな HTTP サービス。受けた通知を、本体が読むフィード(`[daidai] feed_path`、既定 `/var/lib/miharashi/daidai-feed.json`)に書き足すだけで、本体(`miharashi`)のコードと動きは変えない。
設計の理由と書き込みの規則は [DESIGN.md](DESIGN.md) の「通知の入れ方: ファイルを書き換える」。

```
だいだいの VM ──(runtime proxy :3130)──> tailnet ──> miharashi-daidai-inbox ──> daidai-feed.json <── miharashi(poll_secs ごとに読む)
```

## 導入

本体と同じホストで、本体と同じユーザーとして動かす(フィードを本体が読めるようにするため)。

1. ビルド: `cargo build --release -p miharashi-daidai-inbox`
2. 導入(root): `sudo systemd/install-daidai-inbox.sh <LISTEN> <許可する送信元 IP>...`
   - `LISTEN` はこのホストの tailnet の IP とポート。例: `100.64.0.1:8097`。`0.0.0.0` や `::` は起動時に拒否される。
   - 許可する送信元は、だいだい側が tailnet に出ていく IP(runtime proxy 経由なら proxy のホストの IP)。何本でも並べられる。例: `100.64.0.2`。
   - バイナリを `/usr/local/bin` に、unit を `/etc/systemd/system` に置いて `daemon-reload` するだけ。起動もトークンの作成もしない。
   - 書き込み先の `/var/lib/miharashi` は、この unit の `StateDirectory=miharashi` が起動時に作る。本体の unit(`miharashi.service`)も同じ `StateDirectory=miharashi` で、同じユーザーなら所有者もモード(0755)も同じなので、導入の順番は問わない。実行ユーザーを本体と変えると、systemd が起動時に `/var/lib/miharashi` と配下の所有者を書き換え、本体の状態保存を壊しうる。そのため導入スクリプトは、`miharashi.service` の `User` と `/var/lib/miharashi` の所有者が実行ユーザーと違えば止まる。
   - 実行ユーザーは `sudo` の呼び出し元。別のユーザーにするときは `MIHARASHI_USER=<ユーザー>` を付ける。
3. トークンを作る(実行ユーザー所有の 0600。値は画面に出ない):
   ```
   head -c 32 /dev/urandom | base64 | tr -d '=+/\n' | sudo install -o <実行ユーザー> -m 600 /dev/stdin /etc/miharashi/daidai-inbox.token
   ```
   同じ値をだいだい側にも渡す(チャットなどに貼らず、秘密の置き場を使う)。
4. 起動: `sudo systemctl enable --now miharashi-daidai-inbox`
5. 確認: 下の `GET /api/v1/health` が 200 を返し、`journalctl -t miharashi-daidai-inbox -f` に 1 要求 1 行のログが出る。本体側は `[daidai] enabled = true` のときだけフィードを読む。

トークンファイルは起動時と要求ごとに読み直す。差し替えるだけで切り替わり、再起動は要らない(古い値は使えなくなる)。規則に合わなくなった(権限が緩い、リンクになった、短い)ときは起動を拒否し、動作中は 503 を返す。値はログにも応答にも出ない。

## API

URL は `http://<LISTEN>/api/v1/...`。宛先は `/api/v1/notices` の形(origin-form)でも、HTTP の forward proxy 経由で来る `http://<host>:<port>/api/v1/notices` の形(absolute-form)でもよい。クエリ文字列は付けられない(400)。
応答は常に `Connection: close` で、本文は JSON。どのエンドポイントも `Authorization: Bearer <トークン>` が要る。

### `GET /api/v1/health`

疎通確認。200 `{"ok":true}`。フィードの中身は返さず、フィードも読み書きしない。

### `POST /api/v1/notices`

通知カードを 1 枚登録する。`Content-Type: application/json`(`; charset=utf-8` つきも可)と `Content-Length` が要る(`Transfer-Encoding` は受けない)。本文は 8 KiB まで。

```json
{"kind":"info","title":"…","body":"…","avatar_pose":"working","expires_at":"2026-11-18T18:00:00+09:00"}
```

| キー | 必須 | 内容 |
|---|---|---|
| `kind` | 必須 | `morning` / `request` / `info` |
| `title` | 必須 | 前後の空白を除いて 1〜120 文字(Unicode の文字数) |
| `body` | 任意 | 0〜400 文字(省略は空)。改行(`\n`)だけ使える。読み手は空白にする |
| `avatar_pose` | 任意 | `idle` / `working` / `milestone` / `making`。指定したときだけ、フィードのアバターのポーズを書き換える。省略なら今の値のまま |
| `expires_at` | 任意 | 期限(RFC 3339)。今より後で、今から 7 日以内 |
| `ttl_secs` | 任意 | 期限までの秒数(整数、1〜604800)。`expires_at` とは**どちらか一方だけ** |

- 期限を両方とも省略すると、24 時間後になる(期限なしのカードを溜めない)。
- これ以外のキー(`id`、`posted_at`、`priority` を含む)は 400。`id` と時刻はサーバーが付け、`priority` は書かない(= 通常)。値が `null` のキーも 400。
- 制御文字は拒否する(`body` の `\n` だけは可)。双方向制御文字(U+061C、U+200E / U+200F、U+202A〜U+202E、U+2066〜U+2069)とゼロ幅文字(U+200B、U+2060、U+FEFF)も拒否する(表示の偽装を防ぐ)。
- 文字数は切らずに受ける(題 120 / 本文 400)。ただし操作パネルの通知は題を 40 文字、本文を 200 文字で切って出す。パネルでも全部読めるように、短く書く。応答には切られたかどうかは書かない。

成功すると 201 で、書いたカードの値を返す。

```json
{"id":"muse-20261118T030000Z-3fa9c1d2","posted_at":"2026-11-18T12:00:00+09:00","expires_at":"2026-11-19T12:00:00+09:00"}
```

フィードには `{"id","kind","title","body","posted_at","expires_at"}` のカードが**先頭に**入り、最上位の `updated_at` が更新される。期限切れのカードはこのとき取り除かれる。通知が操作パネルに届くまでの遅れは、本体の `poll_secs`(既定 10 秒)ぶん。

### エラー

本文は `{"error":"<短い理由>"}`。理由には入力もトークンも含まれない。

| ステータス | `error` の例 | 意味 |
|---|---|---|
| 400 | `unknown key` / `invalid kind` / `title must be 1 to 120 characters` など | 本文の検証に失敗。HTTP の形の不正(`query string not allowed`、`duplicate header` など)も 400 |
| 401 | `unauthorized` | トークンが無い・違う(`WWW-Authenticate: Bearer` つき) |
| 403 | `forbidden` | 送信元 IP が許可リストに無い(何も読まずに切る) |
| 404 / 405 | `not found` / `method not allowed` | パスが違う / メソッドが違う(405 は `Allow` つき) |
| 408 | `request timeout` | 読み取りが合計 5 秒を超えた |
| 409 | `feed full` | 取り込まれるカードがすでに 16 枚ある。追い出さずに断る。期限が切れるのを待つか、フィードから古いカードを外す(既読にしてもフィードからは消えない) |
| 411 | `content-length required` | `Content-Length` が無い |
| 413 | `body too large` / `feed too large` | 本文が 8 KiB を超える / 書いた結果が 256 KiB を超える |
| 415 | `content-type must be application/json` | `Content-Type` が違う |
| 429 | `too many requests` | 送信元 IP ごとに 1 分あたり 10 件(バースト 5)を超えた。許可リストを通った送信元の要求は、認証に成功した `GET /api/v1/health` を除いて、成功も失敗も(認証前に断られる 400・401・404・405 なども)すべて数える。枠が空のあいだは、これらの応答も 429 になる。`Retry-After`(秒)のあとに再送する |
| 431 | `request header too large` | 要求行 + ヘッダーが 8 KiB を超える |
| 500 | `feed write failed` | 書き込みに失敗した(`rename` の前の失敗なら、一時ファイルは消し、フィードは元のまま) |
| 501 | `transfer-encoding is not supported` | `Transfer-Encoding` つきの要求 |
| 503 | `feed unreadable` | 既存のフィードが読めない・不正(JSON が壊れている、`version` が 1 でない、`cards` が配列でない、256 KiB 超、リンクなど)。**何も書き換えない**ので、直すまで登録できない |
| 503 | `feed busy` / `token unavailable` | 5 秒待ってもロックを取れない / トークンファイルを使えない |

429 と 5xx のあと、同じ内容をすぐに何度も送り直さない。登録に失敗した通知は、カードが作られていない(書き込みは 1 回の `rename` で、途中の状態は残らない)ので、再送しても重複しない。ただし 201 を受け取れなかった(通信が切れた)ときは、書けているかもしれない。

### curl の例

トークンはコマンドラインに出さず、ファイルから渡す。`-v` は使わない(`Authorization` ヘッダーが画面に出る)。

```sh
# だいだい側: 認証ヘッダーを 0600 のファイルに作る(トークンはシェルの履歴に残さない)
( umask 077; printf 'Authorization: Bearer %s\n' "$(cat /path/to/daidai-inbox.token)" > "$HOME/.inbox-auth" )

curl -sS -H @"$HOME/.inbox-auth" http://100.64.0.1:8097/api/v1/health

curl -sS -H @"$HOME/.inbox-auth" -H 'Content-Type: application/json' \
  -d '{"kind":"info","title":"ビルドが終わりました","body":"テストも全部通りました","avatar_pose":"milestone","ttl_secs":7200}' \
  http://100.64.0.1:8097/api/v1/notices
```

`Expect: 100-continue` を付ける古い curl は、本文が 1 KiB を超えると 1 秒ほど待つ(登録口は `100 Continue` を返さない)。待たせたくないときは `-H 'Expect:'` を付ける。

runtime proxy(ポート 3130)経由のときは、`-x` で proxy を指定する(宛先は absolute-form で送られる)。

```sh
curl -sS -x http://<proxy のホスト>:3130 -H @"$HOME/.inbox-auth" -H 'Content-Type: application/json' \
  -d '{"kind":"request","title":"確認をお願いします"}' \
  http://100.64.0.1:8097/api/v1/notices
```

このとき登録口が見る送信元 IP は、proxy が tailnet に出ていく IP になる。`--allow` にはその IP を入れる。

## 運用メモ

- ログは stderr(journal)に 1 要求 1 行: 時刻、送信元 IP、メソッド、パス、ステータス、登録した `id`。ヘッダー・トークン・本文は出さない。
- 接続は 1 本ずつ順に処理する。遅い相手は 5 秒で切るので、ほかの要求を止め続けることはない。
- 登録口を止めても本体は影響を受けない(フィードが読めるだけ)。フィードを書く別の書き手がいるときは、`<フィードのファイル名>.lock` を `flock` してから読み書きすると競合しない。
- 同じ秒に何度登録しても `id` は重ならない(乱数 8 桁。既存の `id` と重なったら作り直す)。
