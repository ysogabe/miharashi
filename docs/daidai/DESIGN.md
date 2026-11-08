# だいだい画面(PoC、Issue #97)

Muse(だいだい)からの通知カードを縦に並べ、ドット絵のアバターを小さく上下に揺らす画面。
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
- 揺れは周期 3.2 秒・振れ幅 7px(高さ 1080 基準)の正弦波を**整数画素に丸めた**上下のずれ。描き直すのは画素が変わったときだけで、実行ループを動かし続けない。
- 背景・カード・時計の層は取っておき、毎フレームは戻してアバターだけを重ねる。
- カードが無いとき: アバターと、大きな時計・日付・「通知はありません」だけの控えめな表示。自動巡回の `skip_if_empty` ではこの状態が「空」。

## アバター素材

`crates/miharashi-render/assets/daidai/daidai-{idle,working,milestone,making}.png`(480x480)。いまは仮の画像
(`cargo run --release -p miharashi-render --example daidai_placeholders -- crates/miharashi-render/assets/daidai` で再生成できる)。
実ファイルへは**同じ名前で差し替える**(ビルドに埋め込まれる)。ビルドし直さずに差し替えるなら `[daidai] avatar_dir` に同じ名前で置く
(起動時に読む。無い・壊れたポーズだけ埋め込みに戻る)。

## 確認

`miharashi png --out work/out` が `daidai.png`(カードあり)・`daidai-working.png`・`daidai-milestone.png`・`daidai-making.png`・`daidai-empty.png` を書く
(見本のフィードを本物の読み取りの経路で読み、時刻は固定)。

## やらないこと(PoC)

HTTP フィード、災害アラート層への表示、miharashi-panel 側の改修、管理ページ(`/api/v1/admin` の feeds)への取得状態の表示。

画面イメージ(ヘッドレス PNG の出力): [assets/daidai.png](assets/daidai.png)(カードあり)、[assets/daidai-empty.png](assets/daidai-empty.png)(カード無し)。
