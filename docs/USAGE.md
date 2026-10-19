# Miharashi — 使い方

ビルド、実行、常駐、設定の手順です。プロダクトの紹介は [README](../README.md) と [LP](https://ysogabe.github.io/miharashi/) を見てください。

リポジトリ名・実行ファイル名は `miharashi`、systemd ユニット名は `miharashi.service` に統一しています。CPU/GPU の型番、搭載メモリ容量、デバイス番号を特定の利用環境に固定しません。

## 1. ビルド

```sh
# Linux。libseat のために pkg-config と libseat-dev が必要(Debian: apt install pkg-config libseat-dev)
cargo build --release
cargo test
```

## 2. 実行

```sh
./target/release/miharashi png --live --out work/out           # ヘッドレス: 画面を PNG に出力する
./target/release/miharashi agents-demo --size 1920x1080 --seconds 15 --fps 15 --out work/agents-demo # T3画面のオフライン連番
./target/release/miharashi kms-info                            # DRM カード・コネクターの一覧と、設定で選ばれる表示先
./target/release/miharashi run --config config/example.toml    # 実機表示(直接オープンは video・input グループが必要)
./target/release/miharashi bench                               # CPU 描画のベースライン(PoC-005)
./target/release/miharashi bench-scene                         # キューブの GPU 時間(Issue #74 T4。1280x720・1920x1080・2560x1080、`--size WxH` で 1 つだけ)
```

`png` の主なオプション:

| オプション | 内容 |
|---|---|
| `--live --seconds N` | N 秒間、1 秒ごとに実測してグラフを貯めてから描く |
| `--live`(室内環境と天気) | 実際の HA と気象庁から取った室内環境と天気の画面を `room-live.png` に出す(最大 40 秒待つ) |
| `--size WxH` | 出力の大きさ(既定 1920x1080) |
| `--photo FILE --caption TEXT` | 写真サーバーの代わりに 1 枚のファイルを表示する |
| `--config FILE` | 設定ファイル |

T3 Agent 画面の静止 PNG fixture は `png` コマンドから出力できます。写真サーバーへの接続を避けるため、ローカル写真を明示してください。

```sh
./target/release/miharashi png --config config/example.toml \
  --photo docs/poc/assets/poc-001-photo-frame.png --size 1920x1080 --out work/agent-fixtures
```

`agents-demo` は実接続せず、`AgentScreen` に仮データと仮時刻を流して連番 PNG を作ります。作成した連番は例えば次のように動画へ変換できます。

```sh
ffmpeg -framerate 15 -i work/agents-demo/frame-%06d.png -pix_fmt yuv420p work/agents-demo.mp4
```

設定を変更したら、表示を起動せずに構文と設定値を確認できます。トークンファイルの権限や接続先の疎通まで確認するコマンドではありません。

```sh
./target/release/miharashi check-config --config config/example.toml
```

### キー操作

テンキー、キーボードの数字列と矢印キー、操作パネルは共通の操作に変換されます。NumLock OFF の対応表は単体テスト済みですが、物理テンキーの 0 と NumLock OFF は実機未確認です。

| キー | 動き |
|---|---|
| 4 / 6(← / →) | 前 / 次の画面(状況 → 写真 → 室内環境と天気 → 航空機 → T3 Agent。有効な画面だけ)。端で折り返す |
| 5 | 画面の中の決定。状況画面は OVERVIEW ↔ DETAILS、写真画面は写真の情報(撮影地・日付)の表示切替、航空機の画面はいちばん近い機体に戻す |
| 8 / 2(↑ / ↓) | 画面の中の前後。写真画面は前 / 次の写真、航空機の画面は注目する機体(近い順) |
| 0(Insert) | 画面の一覧(メニュー)を開く・閉じる。メニュー中は 8/2(4/6)で選び、5 で移る |
| Esc / q | 終了(開発用) |

室内環境と天気・T3 Agent では、画面内の操作はありません。

アラート(地震・災害)が出ているときは、通常の 6 種類の操作キーはアラートを閉じるだけです。Esc / q は終了します。消灯中の通常キーは点灯だけに使います(手動の「常に消灯」では点灯しません)。

自動巡回(`[rotation] enabled = true`)のときは、キーや操作パネルの操作で巡回が `pause_after_input_secs`(既定 5 分)止まり、
最後の操作から時間がたつと次の手順の画面から巡回に戻ります。API の `POST /api/v1/rotation` に `{"mode":"off"}` を送ると、再開するまで止められます。`{"mode":"on"}` で次の手順から再開します。停止状態は再起動すると戻ります。状態ログは `rotation=on` / `paused` / `off` です。

航空機の画面を 5 分続けて出していると、焼き付き対策に約 1.6 秒の演出(グリッチ → 暗転 → 再表示)が入り、そのたびに画面全体が数 px ずれます(9 か所を順に回る)。間隔と対象の画面は `[ui] refresh_minutes` / `refresh_screens` で変えられます。T3 Agent 画面はこの焼き付き対策演出に未対応です。

## 3. 常駐(systemd)

```sh
sudo systemd/install.sh <実行ユーザー>     # root では動かさない。起動はしない
sudo systemctl start miharashi
sudo systemctl enable miharashi             # ブート時も起動する場合(任意。現在の版での実機確認は未完了)
journalctl -t miharashi -f                  # ログ。PAM セッションに入るので -u ではなく -t で見る
```

`systemd/install.sh` は、バイナリを `/usr/local/bin` に、設定を `/etc/miharashi/config.toml` に置きます(既存の設定は変えません)。5 分以内に 5 回落ちると systemd は再起動をあきらめます。戻すときは `sudo systemctl reset-failed miharashi && sudo systemctl start miharashi`。

運用手順と復旧方法の詳細は [PoC-002 の検証記録](poc/POC-002.md) にあります。

## 4. 設定

設定は TOML です。項目と既定値は [`config/example.toml`](../config/example.toml) にあります。未知のキーは typo として起動時に拒否します。

| 節 | 内容 |
|---|---|
| `[runtime]` | seat の取り方、状態ログの間隔、試験用の自動終了 |
| `[display]` | 表示先のカード・コネクター・解像度、描画方式(auto / cpu / gles) |
| `[input]` | 使うキーボード(名前の一部)。空なら 4/6 か矢印キーを持つ全キーボード |
| `[geoframe]` | 写真サーバー(GEOFRAME)の URL、切替間隔、旅と判定する距離 |
| `[metrics]` | 監視する GPU、取得間隔 |
| `[power]` | 電気代の単価など |
| `[home_assistant]` | Home Assistant の URL とトークンのファイル |
| `[screen_power]` | 在宅判定のエンティティ、手動オーバーライドの input_select、消灯までの分 |
| `[alerts]` | 地震・災害アラート、取得間隔、半径、表示時間 |
| `[aircraft]` | ADS-B 受信機(readsb / tar1090)の aircraft.json |
| `[room]` | Home Assistant の室内センサー、履歴、気象庁の予報地域 |
| `[rotation]` | 自動巡回の有効化、画面と表示秒数、操作後の休止時間 |
| `[agents]` | T3 Code の閲覧用 Agent 一覧(既定は無効) |
| `[cube]` | 3D のキューブ画面(全面 / 中央 1/3 幅、fps の上限。既定は無効) |
| `[gl_check]` | GL の状態を汚す試験画面(実機の確認用。既定は無効) |
| `[ui]` | フォント、起動時の画面 |
| `[panel]` | 操作パネル向けの API(既定は無効) |

### Home Assistant で画面を消す

`[home_assistant]` と `[screen_power]` を埋め、長期アクセストークンを `token_file` に置きます。手動オーバーライドには、選択肢が `auto` / `force_on` / `force_off` の input_select を使います。HA と通信できないときは点灯させておきます。

### 室内環境と天気

室内センサーは `[home_assistant]` の接続情報と `[room] tiles` で指定します。天気だけなら `forecast_area` を指定すれば表示できます。両方の接続先がなく、地域も指定していなければ、室内環境と天気の画面は登録されません。

```toml
[room]
enabled = true
forecast_area = "130010" # 気象庁の予報地域。例は東京地方
# HA のセンサーを表示する場合。entity は自分の環境に合わせる
# tiles = [{ label = "温度", entity = "sensor.living_temperature" }]
```

### 画面を自動で巡回する

既定は無効です。`steps` に画面の id と表示秒数を並べます。有効にすると、起動時は `ui.start_screen` より最初の巡回手順が優先されます。登録されていない画面は飛ばします。

```toml
[rotation]
enabled = true
steps = [
  { screen = "photo", secs = 600 },
  { screen = "room", secs = 60 },
  { screen = "aircraft", secs = 60, skip_if_empty = true },
  { screen = "status", secs = 30 },
]
pause_after_input_secs = 300
```

T3 Agent も巡回させるなら、`[agents]` を有効にしたうえで `steps` に `{ screen = "agents", secs = 60 }` などを足してください。

### T3 Agent 一覧

`[agents]` はT3の bearer tokenを専用ファイルから読みます。Bearer tokenは `GET /api/orchestration/shell` にだけ使い、環境表示用には認証なしの `GET /.well-known/t3/environment` も行います。更新系APIは呼び出しません。既存ブラウザーのcookieやMCP tokenは使いません。tokenファイルは実行ユーザーの `0600`、またはroot所有で実行ユーザーのprimary groupに属する `0640` にします。平文HTTPはIPアドレスかlocalhostの接続先だけを受け付け、別ホストへ移るredirectは追いません。

T3 の bearer は 30 日で切れ、延長の仕組みがありません。T3 と同じマシン・同じユーザーで動かしているなら、`miharashi-t3-token.timer` が週 1 回、**読み取りだけ(`orchestration:read`)のトークン**を作り直して `token_file` を置き換えます(仕組みは [DESIGN-t3-token.md](DESIGN-t3-token.md))。

```toml
[agents]
enabled = true
url = "http://192.0.2.10:3773"
# 自動更新のスクリプトが書き換えるので、実行ユーザーが書ける場所に置く
token_file = "/var/lib/miharashi/t3-agent.token"
```

```sh
sudo systemd/install.sh <実行ユーザー>             # miharashi-t3-token とユニットも入る
sudo systemctl start miharashi-t3-token            # 最初の 1 回(トークンを作って置く。1 分ほどかかる)
sudo journalctl -u miharashi-t3-token -n 5         # 「新しいトークンを … に置きました(権限 orchestration:read、有効 30 日)」
sudo systemctl enable --now miharashi-t3-token.timer
t3 auth session list                               # ラベル miharashi-agent-monitor・権限 orchestration:read が 1 つ
```

- miharashi は `token_file` を 30 秒ごとに読み直すので、置き換えても再起動は要りません。
- 古いセッション(同じラベル)は、置き換えてから 60 秒後にスクリプトが失効させます。
- 失敗すると `miharashi-t3-token.service` が failed になり、状況画面の DETAILS の failed ユニットに出ます。原因は `journalctl -u miharashi-t3-token` で見ます。
- 自動更新のスクリプトには T3 v0.1.0 以降が要ります(`t3 auth session issue --scope` を使うため)。
- T3 を別のマシンで動かしているときは自動更新を使えません。T3 のマシンで `t3 auth session issue --scope orchestration:read --ttl 30d --label miharashi-agent-monitor --token-only` を実行し、出たトークンを `token_file` に 0600(実行ユーザー)で置き、30 日以内に同じことを繰り返します。古いセッションは `t3 auth session list` で探し、`t3 auth session revoke <ID>` で失効させます。

取得は専用 worker が `GET /api/orchestration/shell` を 5 秒ごとに行い、接続時に環境情報を `GET /.well-known/t3/environment` で取得します。画面は一覧の状態だけを表示し、会話本文や承認内容は取得しません。認証が失効した場合は 401 / 403 を区別して表示し、token ファイルが変更されたときに再試行します。

### 操作パネル(Echo Show 5)をつなぐ

1. トークンを作る: `head -c 24 /dev/urandom | base64 | sudo install -o root -g <実行ユーザー> -m 640 /dev/stdin /etc/miharashi/panel.token`
2. 設定に足す。待ち受けは LAN のインターフェースの IP にする(`0.0.0.0` は不可)。
   ```toml
   [panel]
   enabled = true
   listen = "192.168.0.132:8790"
   ```
3. 再起動して、別の端末から確かめる。
   ```sh
   curl -H "Authorization: Bearer <token>" http://192.168.0.132:8790/api/v1/status
   curl -H "Authorization: Bearer <token>" -d '{"key":"next"}' http://192.168.0.132:8790/api/v1/input
   ```
4. Echo Show 5 にアプリを入れる: [miharashi-panel](https://github.com/ysogabe/miharashi-panel) の README を参照。

パネルが有効なら、キーボードが無くても起動します(あとから挿したキーボードも 10 秒以内に使い始めます)。API の仕様は [操作パネルの設計](panel/DESIGN.md) §5.2 にあります。
