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
./target/release/miharashi kms-info                            # DRM カード・コネクターの一覧と、設定で選ばれる表示先
./target/release/miharashi run --config config/example.toml    # 実機表示(直接オープンは video・input グループが必要)
./target/release/miharashi bench                               # CPU 描画のベースライン(PoC-005)
```

`png` の主なオプション:

| オプション | 内容 |
|---|---|
| `--live --seconds N` | N 秒間、1 秒ごとに実測してグラフを貯めてから描く |
| `--size WxH` | 出力の大きさ(既定 1920x1080) |
| `--photo FILE --caption TEXT` | 写真サーバーの代わりに 1 枚のファイルを表示する |
| `--config FILE` | 設定ファイル |

### キー操作

テンキー(NumLock の状態は問わない)、キーボードの数字列と矢印キー、操作パネルで同じに動きます。

| キー | 動き |
|---|---|
| 4 / 6(← / →) | 前 / 次の画面。端で折り返す |
| 5 | 画面の中の決定。状況画面は OVERVIEW ↔ DETAILS、写真画面は写真の情報(撮影地・日付)の表示切替 |
| 8 / 2(↑ / ↓) | 画面の中の前後。写真画面は前 / 次の写真 |
| 0(Insert) | 画面の一覧(メニュー)を開く・閉じる。メニュー中は 8/2(4/6)で選び、5 で移る |
| Esc / q | 終了(開発用) |

アラート(地震・災害)が出ているときは、どのキーも閉じるだけです。

## 3. 常駐(systemd)

```sh
sudo systemd/install.sh <実行ユーザー>     # root では動かさない。起動はしない
sudo systemctl start miharashi
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
| `[ui]` | フォント、起動時の画面 |
| `[panel]` | 操作パネル向けの API(既定は無効) |

### Home Assistant で画面を消す

`[home_assistant]` と `[screen_power]` を埋め、長期アクセストークンを `token_file` に置きます。手動オーバーライドには、選択肢が `auto` / `force_on` / `force_off` の input_select を使います。HA と通信できないときは点灯させておきます。

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
