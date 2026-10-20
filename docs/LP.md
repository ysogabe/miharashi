# Miharashi — LP 構成書

版: 0.8 / 更新日: 2026-10-20(0.6: 2026-10-16、0.3: 2026-10-14、0.2: 2026-10-11)
公開先: https://ysogabe.github.io/miharashi/
実物: [site/index.html](../site/index.html)

## 1. LP の目的

Miharashi が何をするものかを、**実装済みの画面**で伝える。実測画像と仮データの見本は明記して区別する。訪問者が「何が映るか」「どう操作するか」「どこまでできていて、次に何をするか」を短時間で理解できるページにする。

0.8 では、画面紹介の T3 AGENTS 静止画の直後に Agent 専用の 30 秒版と 15 秒短縮版を追加した。連携の概念図と、通知から一覧へ戻る動きを伝える(§8)。

0.7 では、ヒーローの直後に紹介動画(30 秒版)を置いた。映像は本体の描画処理に仮データを渡して書き出したもの(§3、§7)。

0.6 では操作パネル専用の LP と専用 CSS を削除し、Miharashi 側からの配信を終了した。専用 LP へのリンクは機能紹介、設計書、アプリのソースへのリンクに差し替えた。

0.5 では実装と公開中の LP を再照合し、室内環境と天気・航空機・T3 Agent の仮データ画像を追加した。実測画像と見本を区別し、焼き付き対策、連携に必要な設定、GPU 描画の既定値も説明する。照合結果と掲載を見送る理由は §6 に記録する。

0.4 では 5 画面・自動巡回・パネル P3 の説明を実装にそろえた。パネル画像は v0.1.0 の記録のまま、現行 v0.2.0 との違いを明記している。

0.2 までは実装前のコンセプトプレビュー(HTML で描いた画面案とタブの操作デモ)だった。0.3 で、実機が描いた画面と Echo Show 5 の実機のスクリーンショットに置き換え、操作デモ(`app.js`、`panel/panel.js`)はやめた。

## 2. ページの構成

### Miharashi(`site/index.html`)

| 順序 | セクション | 役割 |
|---|---|---|
| 1 | ヘッダー | 名称、画面・機能・操作パネル・仕組み・FAQ への導線、「実機で毎日稼働中」 |
| 2 | ヒーロー | 「サーバーの今を、ひとつの画面に。」と用途、特徴 3 つ。注記で「下の静止画のうち状況と写真は実機から出力、ほかの 3 画面と紹介動画は仮データ」と対象を区別する |
| 3 | 紹介動画 | 30 秒の紹介動画(仮データ。音声なし、自動再生しない)。キャプションに仮データであることを明記 |
| 4 | 01 画面 | STATUS(OVERVIEW / DETAILS)・PHOTO FRAME の実機画像と、ROOM & WEATHER・AIRCRAFT・T3 AGENTS の仮データ画像。直後に Agent 専用動画(主動画 30 秒・折りたたみ内の 15 秒版) |
| 5 | 02 機能 | アラート、写真、航空機、室内環境と天気、T3 Agent、自動巡回、モニターの電源、操作パネル、GPU 描画、焼き付き対策の 10 枚と、連携を有効にする条件 |
| 6 | 03 操作パネル | Echo Show 5 のスクリーンショットと、パネルアプリのソースへの導線 |
| 7 | 04 仕組み | 共通の描画面 → DRM/KMS・PNG、表示と監視の GPU の役割、seat と systemd |
| 8 | 05 ロードマップ | 完了 4 段階と、MVP に残る日常運用の仕上げ |
| 9 | 06 FAQ | よくある質問 |
| 10 | 文書への導線 | GitHub のリポジトリ、docs、操作パネルの設計、パネルアプリ |

操作パネル専用の LP は配信しない。以前の `/miharashi/panel/` にリダイレクトや代替ページは置かず、公開対象から削除する。操作パネルの機能紹介と実機画像は製品 LP に残す。

## 3. 画像

| ファイル | 中身 | 撮り方 |
|---|---|---|
| `site/images/status.jpg` | 状況画面 OVERVIEW | `miharashi png --live --seconds 300 --size 2560x1080` の出力を 1600×675 に縮小 |
| `site/images/status-details.jpg` | 状況画面 DETAILS | 同上 |
| `site/images/photo.jpg` | フォトフレーム | 同上に `--photo <生成した風景画像> --caption "夕暮れの湖"` |
| `site/images/room-demo.png` | 室内環境と天気(仮データ) | `png` の `room.png`(2560×1080) |
| `site/images/aircraft-demo.png` | 航空機(仮データ) | `png` の `aircraft.png`(2560×1080) |
| `site/images/agents-demo.png` | T3 Agent(仮データ) | `png` の `agents-overview.png`(2560×1080) |
| `site/videos/miharashi-30s.mp4` | 紹介動画 30 秒版(1920×1080、約 2.2 MB、音声なし)。仮データ | `miharashi intro-video`(§7) |
| `site/videos/miharashi-30s-poster.jpg` | 動画のポスター(1280×720) | 同じ動画の 1.8 秒の 1 コマ |
| `site/videos/miharashi-agents-30s.mp4` | Agent 専用 30 秒版(1920×1080、約 1.35 MB、音声なし)。仮データ | `miharashi agents-intro-video`(§8) |
| `site/videos/miharashi-agents-15s.mp4` | Agent 専用 15 秒短縮版(1920×1080、約 1.04 MB、音声なし)。仮データ | 同上 |
| `site/videos/miharashi-agents-{15,30}s-poster.jpg` | 各版と同じ映像の連携図(1280×720) | 15 秒版の 1.5 秒 / 30 秒版の 2 秒 |
| `site/images/panel.png` | Echo Show 5 の操作パネル | `adb exec-out screencap -p`(960×480 のまま) |

### 載せないもの(プライバシー)

- 家族の写真、旅行先が分かる写真や旅のカード。フォトフレームは公開用に生成した風景画像で撮る。
- T3 Agent の実データ: プロジェクト名やスレッド名を含むため、実画面は掲載しない。既存の `agent_demo` の仮データで描いた見本だけを載せる。
- 自宅の場所が分かる画面: 室内環境と天気(自宅の地域と室内センサー)、航空機(受信機の位置が中心)、災害アラート(自宅から半径 50km の市町村)、地震アラートの地方。実データの画面は載せない。室内環境と天気・航空機は、`sample.rs` の東京駅付近を使う仮データの見本だけを載せる。アラートは説明文で紹介する。
- トークン、IP 以外のネットワークの詳細、Home Assistant のエンティティの中身。

状況画面の数値(CPU 使用率など)は撮影したときの実測値で、作者の機材の値である。`*-demo.png` の数値・予報・航空機・作業名は仮データであり、実測値や現在の状況ではない。

## 4. 表示上の約束

- 表示名は Miharashi、リポジトリ・実行ファイル名は `miharashi` に統一する。
- 文章で特定の CPU/GPU 型番・メモリ容量を製品の前提にしない(スクリーンショットに写る実機の値は除く)。
- 「できること」と「次にやること」を混ぜない。予定はロードマップにだけ書く。
- 連絡フォーム、アカウント作成、アナリティクス、外部 API、外部フォント・CDN を追加しない。
- モバイルでは画像と説明を縦に積み、水平スクロールを発生させない。
- 文書へのリンクは GitHub の URL にする(Pages に公開するのは `site/` だけなので、`../docs/` の相対リンクは切れる)。

## 5. ローカルでの確認

`site/index.html` をブラウザーで開けば見られる。ローカルサーバーを使う場合は、プロジェクトのフォルダで次を実行し、`http://127.0.0.1:18765/` を開く。

```sh
python3 -m http.server 18765 --bind 127.0.0.1 --directory site
```

`main` の `site/` が変わると、GitHub Actions(`.github/workflows/pages.yml`)が GitHub Pages に公開する。


## 6. 実装との照合(2026-10-16)

公開中の LP はリポジトリの 0.4 と同じ説明だった。実装のない機能を新たに作る必要はなく、主な差は画像と説明の不足、既定値の説明だった。

- **5 画面**: `app.rs::kiosk` に登録済み。状況・写真だけが画像になっていたのは §3 のプライバシー方針による。個人情報を含まない見本の出力が既にあるため、0.5 で残り 3 画面を仮データとして掲載した。
- **写真・アラート・室内環境と天気・航空機・T3 Agent・巡回・電源**: `photos.rs` / `album.rs`、`quake.rs` / `disaster.rs`、`env.rs` / `weather.rs`、`aircraft.rs`、`agents.rs`、`app/manager.rs`、`screen_power.rs` / `runtime.rs` と照合。既存の機能紹介に対応する実装がある。5 画面を出す条件と、T3 Agent・巡回・パネル API が既定無効であることを LP に補足した。外部サービスや利用環境の設定が必要であり、未実装が理由ではない。
- **GPU 描画**: `config.rs` の既定は `cpu`。LP の「内蔵 GPU で行う」に選択条件がなかったため、設定で選ぶことと既定値を追記した。`auto` では GPU が使えなければ CPU に戻る。
- **焼き付き対策**: `app/manager.rs` の refresh と `[ui] refresh_minutes` / `refresh_screens` に実装済みだが、機能一覧から漏れていた。既定の航空機画面・5 分間隔と、T3 Agent 未対応の制限を追記した。
- **パネル P3**: `panel.rs` はキー操作のほか、画面の直接選択と巡回の切り替えも受け付ける。LP の「キーを送るだけ」と接続図を現行 API に合わせた。パネルアプリは別リポジトリのため、アプリの実機再検証は今回の照合に含めない。
- **パネル画像を更新しない理由**: このリポジトリにある実機画像は v0.1.0 だけ。v0.2.0 の画像として加工・流用せず、旧版である旨と現行版との差の説明を維持する。更新には現行アプリの公開可能な実機スクリーンショットが必要。
- **掲載しない将来機能**: BLE 接続はネットワーク API を採用したため不採用。ホストの自動発見は固定 IP 運用のため見送り、夜間減光は操作盤の無操作時消灯で足りるため作らない(`panel/DESIGN.md` §7)。ブラウザー向け操作画面は未実装で、FAQ でもその旨を維持する。
- **MVP を完了扱いにしない理由**: 現行版の長時間試験、ブート時の自動起動確認、ディスクキャッシュ、テスト CI、モニターのつなぎ替え、リリースノートは残件。PoC 時点の実績と現在の再検証を混同せず、ロードマップの記載を維持する。

### 仮データ画像の再出力

ネットワークからの実データ取得を避け、設定ファイルと公開済みのローカル画像を明示する。`--live` は付けない。生成結果のうち、以下の 3 枚だけを掲載する。

```sh
cargo build --release
./target/release/miharashi png --config config/example.toml \
  --photo site/images/photo.jpg --size 2560x1080 --out work/lp-fixtures
cp work/lp-fixtures/room.png site/images/room-demo.png
cp work/lp-fixtures/aircraft.png site/images/aircraft-demo.png
cp work/lp-fixtures/agents-overview.png site/images/agents-demo.png
```

## 7. 紹介動画の再生成

動画は `miharashi intro-video` が、各画面の見本を仮想の時刻で描いて合成する(実機も、通信も、撮影素材も使わない)。構成・長さ・見出し・補足文は [docs/intro-video/edit.json](intro-video/edit.json)。10 秒版と 60 秒版はリポジトリに置かず、必要なときに作る。`ffmpeg`(libx264)が要る。`--out-dir`(`demo-video` なら `--out` の親)は、無ければ作る。途中で止めた(Ctrl-C、SIGTERM)ときは、書き出し中の動画を残さない。

```sh
cargo build --release
./target/release/miharashi intro-video --version all --out-dir work/intro --overwrite   # 10 / 30 / 60 秒版
# LP 用(30 秒版)。ポスターは同じ動画の 1.8 秒の 1 コマ
ffmpeg -y -i work/intro/miharashi-30s.mp4 -an -c:v libx264 -preset slow -crf 22 -pix_fmt yuv420p \
  -colorspace bt709 -color_primaries bt709 -color_trc bt709 -color_range tv \
  -movflags +faststart -fflags +bitexact -flags:v +bitexact -map_metadata -1 site/videos/miharashi-30s.mp4
ffmpeg -y -ss 1.8 -i work/intro/miharashi-30s.mp4 -frames:v 1 -vf scale=1280:720 -q:v 4 site/videos/miharashi-30s-poster.jpg
```

- 長さはフレーム単位(300 / 900 / 1800)で合わせる。同じ環境(OS、Noto Sans CJK JP と Noto Sans Mono のフォント、ffmpeg / libx264 のビルド)で同じ引数なら、毎回同じバイト列になる。環境が違うと、フォントと ffmpeg の違いでバイト列は変わりうる。
- 映像の中の値(使用率、予報、機体、作業名、写真)はすべて仮データで(状況画面は 1 秒に 1 サンプル進む、決定的な疑似乱数の緩やかな変動。`video/status_data.rs`)、写真はプログラムで作った架空の風景。右上のバッジは「仮データ」。
- 旧版(Mac でスマホから実モニターを撮影)は、画質が悪く自宅周辺の地名が映っていたため、この仕組みに置き換えた。

## 8. Agent 専用紹介動画の再生成

### 構成と掲載位置

Mac の旧 `video-creator` の編集原本(`agents-edit.json`)は、この作業環境の一時フォルダーから削除済みで、今回 Mac 側の原本は参照していない。履歴で確認できた構成要素(15 秒: 連携概念図・Agent 一覧・状態変更通知、30 秒: 連携と読み取り専用の説明・長めの一覧と通知)から、現行の配色・フォントで再構成した。旧版の秒割りや文言の完全復元ではなく、旧動画素材も使わない。

編集原本は [docs/agents-intro-video/edit.json](agents-intro-video/edit.json)。`text` が文言、`versions.15` / `versions.30` が区間の長さ・通知時刻・見出し切替。CLI の既定値はビルド時に埋め込んだ同じ原本で、`--edit` を指定すれば再ビルドせず別の編集ファイルを読める。`intro-video` の 10 / 30 / 60 秒版やその原本とは独立している。

- **15 秒版**: 概念図 3 秒 → 実画面 10 秒 → 案内 2 秒(450 フレーム)。実画面の 0.3 秒で新着、5.0 秒で同じスレッドが応答完了。初期一覧は短く、通知の動きを手早く見せる。新着の復帰は 4.8 秒、完了の復帰は 9.5 秒で、最後の完了一覧を 0.5 秒残す。
- **30 秒版**: 概念図 6 秒 → 実画面 20 秒 → 案内 4 秒(900 フレーム)。実画面の 3 秒で新着、10 秒で応答完了。新着の復帰 7.5 秒から完了通知まで 2.5 秒、完了の復帰 14.5 秒から締めまで 5.5 秒、一覧を読みやすく保持する。
- 実画面は既存の `DemoRun` / `agent_demo::Scenario` が公開用の仮データを実 `AgentScreen` に渡す。1 本の連続した仮想時刻で動かし、見出しを替えるために時計・スレッド・通知をリセットしない。中央通知は通常の 700ms 導入 + 3 秒保持 + 800ms 復帰を使う。通常のアプリ挙動は変えない。
- 概念図は Rust の Canvas で描く。T3 Code(Codex / Claude) → プロジェクト / スレッドの状態 → Miharashi の一覧 / 中央通知という情報の方向だけに矢印と動く点を置く。読み取り専用・会話本文を取得しないことを図と実画面の補足に明記する。

掲載先は `#screens` の T3 AGENTS 静止画直後、`shot-grid` の外に置く全幅の `#agents-video`。静止画が示す「何が並ぶか」に続けて、動画で「変化がどう届くか」を説明でき、通知の中央表示も大きく見せられるため。ヒーロー直後の総合 30 秒動画は製品全体を知る役割を保つ。Agent 専用の主動画は一覧を読む時間がある 30 秒版とし、15 秒版は同じブロック内の標準 HTML `details`「15秒版を見る」に収める。両版とも `controls muted loop playsinline preload="none"`、自動再生なし、説明と MP4 リンク付き。JavaScript は追加しない。

### 正確な生成・軽量化・ポスターのコマンド

中間 PNG 連番は作らず、既存の `Sink` に RGBA フレームを流す。完成仕様は両版とも 1920×1080・30fps・無音 H.264 / yuv420p / BT.709 / faststart。深紺 `#07131c`、白 `#e7f0f4`、ミント `#72e2c5`、Noto Sans CJK JP と Noto Sans Mono、区間境界の 6 フレームのフェードを使う。

```sh
cargo build --release -p miharashi-app
TZ=Asia/Tokyo ./target/release/miharashi agents-intro-video --version all \
  --out-dir work/agents-intro --edit docs/agents-intro-video/edit.json --overwrite
# 個別生成なら --version 15 または --version 30

# LP 用の軽量化。bitexact でも SPS に BT.709 の三属性を確実に残す。
for seconds in 15 30; do
  ffmpeg -y -i "work/agents-intro/miharashi-agents-${seconds}s.mp4" \
    -an -c:v libx264 -preset slow -crf 22 -pix_fmt yuv420p \
    -colorspace bt709 -color_primaries bt709 -color_trc bt709 -color_range tv \
    -bsf:v h264_metadata=colour_primaries=1:transfer_characteristics=1:matrix_coefficients=1 \
    -movflags +faststart -fflags +bitexact -flags:v +bitexact -map_metadata -1 \
    "site/videos/miharashi-agents-${seconds}s.mp4"
done
# 各版と同じ完成映像から連携図を取り出す
ffmpeg -y -ss 1.5 -i site/videos/miharashi-agents-15s.mp4 -frames:v 1 \
  -vf scale=1280:720 -q:v 4 site/videos/miharashi-agents-15s-poster.jpg
ffmpeg -y -ss 2 -i site/videos/miharashi-agents-30s.mp4 -frames:v 1 \
  -vf scale=1280:720 -q:v 4 site/videos/miharashi-agents-30s-poster.jpg
```

生成結果(2026-10-20): 15 秒版 **1,036,416 bytes**(約 1.04 MB)、30 秒版 **1,353,050 bytes**(約 1.35 MB)。ポスターは順に 54,603 / 54,616 bytes。`ffprobe -count_frames` で 450 / 900 フレーム、15.000000 / 30.000000 秒、1920×1080・30/1fps・映像ストリーム 1 本(音声なし)、yuv420p、色空間・原色・伝達特性がすべて bt709 と確認。MP4 の `moov` が `mdat` より前にあることも確認した。

### 再現条件と確認

同じ OS・フォントファイル・描画ライブラリ・タイムゾーン・ffmpeg / libx264 のビルドと同じ JSON を使う。今回の生成環境は Linux、`TZ=Asia/Tokyo`、ffmpeg 7.1.5-0+deb13u1、NotoSansCJK-Regular.ttc / Bold、NotoSansMono-Regular.ttf。実 API・既存画面・個人設定は探さず、外部通信もしない。壁時計は 2026-10-16 14:35:00 を動画全体の起点にする。環境が違えばフォントやエンコーダの差でバイト列が変わりうる。

```sh
cargo test --release -p miharashi-app video::
# 同じ 52 フレーム(概念図の動く点・フェード・新着通知)を 2 回生成し、MP4 のバイト一致を確認
TZ=Asia/Tokyo cargo test --release -p miharashi-app encoded_excerpt_is_byte_identical -- --ignored
for seconds in 15 30; do
  ffprobe -v error -count_frames -show_streams -show_format -of json \
    "site/videos/miharashi-agents-${seconds}s.mp4"
done
```

代表コマは完成 MP4 から `work/agent-video-review/{15,30}s-01.png` ～ `07.png` に保存した(作業成果物、git 管理外)。順に概念図・初期一覧・新着中央・一覧復帰・完了中央・完了一覧・締め。15 秒版のフレーム番号は 45 / 96 / 135 / 236 / 285 / 381 / 420、30 秒版は 60 / 240 / 330 / 435 / 525 / 660 / 840(先頭 0)。`{15,30}s-contact.png` は同じコマの一覧。映像の時計が進み続け、復帰後の一覧と中央通知の状態・同じ作業名が一致することを確認した。ブラウザーで両版を再生し、幅 320 / 375px・折りたたみの開閉時にも横溢れがないことを確認した。
