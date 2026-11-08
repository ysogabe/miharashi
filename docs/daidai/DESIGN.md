# だいだい画面(PoC、Issue #97)

Muse(だいだい)からの通知カードを縦に並べ、ドット絵のアバターを小さく上下に揺らす(ポーズ動画があればそれをループ再生する)画面。
画面 id は **`daidai`**(操作パネルとの約束なので変えない)。題名は `DAIDAI`。既定は無効で、`[daidai] enabled = true` のときだけ `agents` の次(最後)に登録する。

## データ

PoC の取得元は本体ホスト上のローカル JSON ファイル(`[daidai] feed_path`、既定 `/var/lib/miharashi/daidai-feed.json`)。
書式の見本は [feed.sample.json](feed.sample.json)。書き手は一時ファイルに書いてから rename で置き換える。

| フィールド | 意味 |
|---|---|
| `version` | 1(無ければ 1 として読む。ほかの値のフィードは読まない = カード無し) |
| `updated_at` | ISO 8601。表示には使わない |
| `avatar_pose` | 任意。`idle` / `working` / `milestone` / `making`。無い・知らない値は `idle` |
| `cards[]` | `id`、`kind`(`morning` / `request` / `info`。知らない値は `info`)、`title`(必須)、`body`、`expires_at`(ISO 8601 か null) |

読み取り規則(どれも例外では落とさず、画面は「カード無し」になる):

- ファイル無し・読めない・空・JSON が不正・256 KiB 超 → カード無し(理由は journal に、変わったときだけ 1 行)。
- 不正なカード 1 枚(`title` が無い、`expires_at` が日時として読めない・文字列でない)は、そのカードだけ捨てて残りは出す。
- `expires_at` を過ぎた(ちょうども含む)カードは、画面が描くたびに壁時計で外す。ファイルを読み直さなくても消える。
- 制御文字は空白にし、title は 120 字・body は 400 字、カードは 16 枚までで切る。画面に並べるのは 4 枚まで(残りは「ほか N 件」)、本文は 2 行まで。

## 構造

```
FeedSource(trait) ── FileSource ──> parse_feed ──> Shared ──(DaidaiProvider)──> World.daidai ──> DaidaiScreen
   (将来: HTTP)        [daidai].source           crates/miharashi-app/src/daidai.rs                 app/screens/daidai.rs
                                                                                                    └ 描画: crates/miharashi-render/src/daidai.rs
```

- 取得元は `[daidai] source` で選ぶ(今は `file` だけ)。HTTP にするときは `FeedSource` の実装と `source` の値を足すだけで、`parse_feed` 以降は変わらない。
- 取得は自前のスレッド(`poll_secs`、既定 10 秒)。Provider は最新値を `World.daidai` に写すだけでブロックしない。

## 描画

- アバターは 480x480 の PNG を **Nearest** で拡大縮小する(ドットが潰れない・にじまない)。
- 素材は背景が白く不透明なので、描くときに四隅を**角丸で切り抜く**(半径は表示した絵の短い辺の 14% = `AVATAR_CORNER_RATIO`、後ろの光の円になじむ大きさ)。
  絵を模様にして角丸の輪郭を塗るので、揺れ・拡大縮小・ポーズの差し替えでも同じ形になり、マスクの作り直しは要らない。角の外は背景と光がそのまま見える。素材 PNG は加工しない。
- 揺れは周期 3.2 秒・振れ幅 7px(高さ 1080 基準)の正弦波を**整数画素に丸めた**上下のずれ。描き直すのは画素が変わったときだけで、実行ループを動かし続けない(PNG のポーズ。動画のポーズは「ポーズ動画」)。
- 背景・カード・時計の層は取っておき、毎フレームは戻してアバターだけを重ねる。
- カードが無いとき: アバターと、大きな時計・日付・「通知はありません」だけの控えめな表示。自動巡回の `skip_if_empty` ではこの状態が「空」。

## ポーズ動画(Issue #99)

アバターは PNG の静止画のほかに、ポーズごとの MP4 をループ再生できる。ポーズは `avatar_pose`(idle / working / milestone / making)に対応する。

```
DaidaiScreen ──> VideoPlayback(daidai_video.rs)──> Player ──> FrameSource(trait)
 (描画は PNG と同じ                 ポーズが変われば開き直す     壁時計で枚数を決める   └ miharashi-video の CpuDecoder(`video` つきのビルド)
  draw_avatar に Pixmap を渡す)
```

- **素材**: `[daidai] avatar_dir` の `daidai-<pose>.mp4`(`daidai-idle.mp4` など。PNG と同じ名前の拡張子違い)。
  同梱の 4 本は `crates/miharashi-render/assets/daidai/daidai-{idle,working,milestone,making}.mp4`(H.264 / 720x720 / 24fps / 8 秒 = 192 枚、各 0.3〜0.6MB)。
  **これは差し替え用の素材で、ビルドには埋め込まない**(PNG は埋め込みだが、動画は `avatar_dir` に置いたものだけを使う。`avatar_dir` が空なら動画は使わない)。
  置き場所は `avatar_dir` に 4 本をコピーする(`install -m 644 crates/miharashi-render/assets/daidai/daidai-*.mp4 <avatar_dir>/`)。起動時でなく、ポーズが初めて表示されたときに開く。
- **ビルド**: ソフトウェアデコードに ffmpeg(libavcodec / libavformat)を使うので、`cargo build --release --features video`(ffmpeg の開発用パッケージが要る)でビルドしたときだけ動画が再生される。
  既定のビルド(`video` なし)では、動画があっても読まず PNG のまま(理由は journal に 1 回)。
- **フォールバック**(どれも画面は落とさず、そのポーズだけ PNG に戻る。理由はポーズにつき 1 回だけ journal に残し、起動のあいだは開き直さない):
  ファイルが無い / 32MiB を超える / 開けない・映像が無い / H.264 などでデコードできない・YUV420P でない / フレームレートが 1〜120 の外 /
  最初の 1 枚を読めない / 再生の途中で読めなくなった / このビルドに `video` が無い。
- **再生**: 経過時間 x フレームレートで出す枚を決める(「描いた回数」ではない)。描き直しの上限を動画のフレームレートより小さくしても、再生は遅くならず、
  間の枚は読み飛ばす(読み飛ばす枚は色の変換をしない)。先頭に戻るときはデコーダーの残りを吐き出させてから戻るので、1 周はちょうど 192 枚(8 秒)で、周回を重ねてもずれない(B フレームの最後の数枚が落ちない)。ポーズが変わったら、そのポーズの動画を先頭から始める。
  大きく遅れた(12 枚を超える)ときは取り戻さず、時計を合わせ直す(一時停止のあとに固まらない)。
- **描画**: デコードしたフレームを一辺 480(`AVATAR_SIZE`)以下に双線形で縮め、`Pixmap` にして PNG と同じ `draw_avatar` に渡す。
  なので四隅の角丸の切り抜き・位置・大きさは PNG のときと同じ。**動画のあいだは揺らさない**(動画自体が動くので。PNG のポーズは従来どおり揺れる)。
  動画は毎周期描き直す(`tick` が `true`、`animating`)。実行ループは `frame_interval`(`1 / video_max_fps`)で上限をとるので、空回りしない。
- **止める**: 画面が見えていない(別の画面に移った)・モニターが消えている(`set_presentation` の `visible && display_on` が偽)ときは、
  デコーダーごと捨てて何も読まない(メモリも持たない)。戻ったら先頭から。
  アラートやメニューが上に載っているあいだ(`covered`)も読み進めない(`WithScene` と同じ規則)が、デコーダーは持ったまま最後のフレームを出し続け、閉じたら続きから進む(0.5 秒を超えて止まっていたら時計を合わせ直す)。
- **設定**: `[daidai] video_max_fps`(既定 12、1〜30)が描き直しの回数の上限。モニターの垂直同期に切り上がる(60Hz なら 12 / 15 / 20 / 30)。
  動画を無効にする専用の設定は無い(動画のファイルを `avatar_dir` に置かない、または `video` なしでビルドすれば PNG のまま)。
- **負荷(開発機で、試験の中で 2560x1080 の描画を繰り返して計測。1 回の描き直しあたり)**: 1 枚のデコードは約 0.35〜0.45ms、480x480 への変換が約 1.5ms、
  背景の復元とアバターの重ねで約 1ms。12 回/秒で 1 秒あたり約 41ms(CPU 1 コアの約 4%)、24 回/秒で約 60ms(約 6%)。画面への転送(present)は別。実機の CPU・描画の間隔の確認は別途。

## アバター素材

`crates/miharashi-render/assets/daidai/daidai-{idle,working,milestone,making}.png`(480x480、RGB・背景は白で不透明)。動画(`.mp4`)は上の「ポーズ動画」。
本物の素材に差し替え済み(ビルドに埋め込まれる)。ポーズを足す・直すときも**同じ名前で置き換える**。
ビルドし直さずに差し替えるなら `[daidai] avatar_dir` に同じ名前で置く(起動時に読む。無い・壊れたポーズだけ埋め込みに戻る)。
素材の四隅は描画側で角丸に切り抜くので、素材自体は四角のままでよい(透過 PNG にしても動く)。

仮素材の生成用に `daidai_placeholders` example が残っている。これは**仮の絵を書き出す**もので、出力先に本物の素材があると上書きしてしまうので、
`assets/daidai` を指定して実行しない(試すときは別のディレクトリへ)。

## 確認

`miharashi png --out work/out` が `daidai.png`(カードあり)・`daidai-working.png`・`daidai-milestone.png`・`daidai-making.png`・`daidai-empty.png` を書く
(見本のフィードを本物の読み取りの経路で読み、時刻は固定)。

## やらないこと(PoC)

HTTP フィード、災害アラート層への表示、miharashi-panel 側の改修、管理ページ(`/api/v1/admin` の feeds)への取得状態の表示。
ポーズ動画では、動画を埋め込みの既定にすること、VA-API でのデコード(下の「ポーズ動画」は CPU のソフトウェアデコード。GL の画面に載せる別の形は将来)、
GPU 経路(`renderer = gles`)での合成、音声。

画面イメージ(ヘッドレス PNG の出力): [assets/daidai.png](assets/daidai.png)(カードあり)、[assets/daidai-empty.png](assets/daidai-empty.png)(カード無し)。
