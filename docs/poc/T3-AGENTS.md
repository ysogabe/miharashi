# T3 Agent カードボード — 実装・検証記録

2026-10-15 更新。状態: **実装済み・実機配備済み**。設計は [T3 Agent 画面の概要設計](../DESIGN-t3-agent-monitor.md) に記載。

この文書は実装と検証で確認できた内容を記録する。描画、GPU合成、取得処理、画面統合を含むローカル検証と、T3実サーバーへの認証済み接続試験を完了した。1080pと2560×1080の画像と、仮データによる1080pの動きの動画を確認した。

## 実装状況

- 表示専用のカード面、状態色、可変位置・サイズ・透明度を扱う render API と CPU 合成経路を追加。
- GPU 経路は画面背景とカード面のテクスチャを保持し、位置・拡縮・fade の更新だけではカード画素を再転送しない。カード面は表示中の最大64 IDを保持し、revision または寸法が変わると更新する。
- 状態色、中央の変更表示、scrim、カード順序、背景情報（header、legend、空・認証エラー・STALE表示）は画面層で構成する。
- app/runtimeへ統合し、ヘッダー、変更ポップアップ、新着カード、STALE表示を含む1080p PNG fixtureを確認した。本番serviceへ配備済み。実機KMSの画面状態は操作パネルAPIで確認した。

動きの確認用動画: [T3 Agent カードボード 1080p H.264 / 225 frames / 15秒 / 15fps](</tmp/miharashi-agents-verified.mp4>)。仮データから生成したレビュー用映像で、実T3のデータや実機の撮影映像ではない。

### T3 実サーバー接続試験

2026-10-15、`t3 auth session issue` で15分の接続確認セッションを発行し、`GET /api/orchestration/shell` を実行した。HTTP 200、応答7ms、snapshotSequence 6783、project 2件・thread 3件を確認した。個別のthread内容は記録せず、レスポンスも保存していない。

CLIが発行したセッションには `orchestration:read` のほか `orchestration:operate`、`terminal:operate`、`review:write`、`relay:read`、`relay:write`、`access:read`、`access:write` が付与された。疎通確認ではGETのみ実行し、試験後にセッションを失効、ローカルのトークンファイルを削除した。read専用スコープだけを発行するCLIオプションは見つからなかったため、miharashiの設定には登録していない。

確認したT3は **0.0.45 / orchestration protocol 1**。調査時のMCP bearerは一覧APIで `401 invalid_credential` となり、今回の接続には使っていない。

## 確認できた検証

### Render / CPU

次のテストは通過し、日本語テキストを含むカードを 1920×1080 と 2560×1080 へ合成した。PNG はテストから `/tmp/agents-1920x1080.png` と `/tmp/agents-2560x1080.png` に出力された。さらに `/tmp/miharashi-agents-verified-1080/` の1080p fixtureで、ヘッダー、新着、変更ポップアップ、STALE表示を目視確認した。

```sh
CARGO_TARGET_DIR=/tmp/miharashi-luna-render cargo test -p miharashi-render agents::tests::japanese_cards_compose_at_full_hd_and_ultrawide -- --nocapture
```

`cargo check -p miharashi-render -p miharashi-gpu` も通過。カード拡大時のジャギーを抑えるため、カード面のみGPU LINEAR / CPU Bilinear samplingに揃えた後のCPU/GPU合成比較テストも通過した。背景・写真のNEAREST samplingは維持している。

2560×1080のPNG fixture全種を生成し、終了コード0を確認した。`/tmp/miharashi-agents-verified-21x9/` の`changed-02`では中央配置、文字、背景上のhome位置を目視確認した。

### GPU / offscreen

root が実機 render node で次の offscreen テスト2件を実行し、両方通過した。

```sh
cargo test -p miharashi-gpu offscreen_agent -- --nocapture
```

- 上下左右で異なる色のカード面を使い、向きがCPU面と合うことを確認。
- 位置だけの変更では転送量が増えず、revision更新ではカード面だけが再転送されることを確認。
- alpha、拡縮、scrimを含むCPU/GPU合成結果を比較。

GPU結果は権限を許可したroot実行でrender nodeを開いて確認した。

### workspace test

PR作成前の最終検証で `cargo test --workspace --locked` を実行し、211 tests passed、1 ignoredを確認した。内訳は app 136、GPU 11、Linux 33、render 30、video 1。ignoredは次項の手動GPU計測。

`cargo clippy --workspace --all-targets --locked -- -D warnings` も終了コード0で、警告はなかった。`cargo fmt --all --check` とdiff checkも通過した。

### GPU / cached pose submission

実機render node上で、1920×1080背景と600×300のカード面6枚を一度転送し、位置・拡縮・alphaのみを変えた300フレームを描画した。

| 測定値 | 結果 |
|---|---:|
| 初回upload | 12,614,400 bytes |
| 300 pose-only frames後のupload累計 | 12,614,400 bytes |
| 平均CPU command submission time | 0.0037 ms/frame |
| GLES error | なし |

この時間はCompositorへのCPU側コマンド投入時間で、GPU完了時間、アプリ全体のCPU使用率、描画FPSを示す値ではない。

## 実機配備（2026-10-15）

- T3 v0.0.45向けtokenを30日で発行し、`/etc/miharashi/t3-agent.token`（root:hermes 0640）に配置。値はログ・文書に記録していない。
- 既存設定と画面を保持してAgent画面を起動時に選ぶよう更新し、既存バイナリ・設定のバックアップを保存。
- `/usr/local/bin/miharashi` を統合版へ更新し、systemd serviceを再起動。serviceはactive/running。
- 認証付きGET `/api/orchestration/shell` はHTTP 200、2 projects / 3 threads、8msを確認。
- 操作パネルのGET `/api/v1/status` はHTTP 200を返し、現在画面 `agents` / `T3 AGENTS`、登録画面5件を確認。

## 画面切替の修正と実機GPU確認（2026-10-15）

- Agent画面のGPU合成で、生成した背景を共有Canvasへコピーしていなかった。別画面から戻ると、その画面のCanvasがAgent用背景として転送され、前の画面の上にカードが重なる不具合を実機KMSキャプチャで再現した。
- 背景が変わったときは共有CanvasをAgent背景で置き換えてからGPUへ渡すよう修正。背景に変更がないフレームは既存のGPUテクスチャを再利用する。
- 背景キャッシュを持った状態での画面再入場、メニュー等を重ねた描画からの復帰、変更のないフレームでの転送抑止を回帰テストで確認。修正前は失敗し、修正後は成功した。appの136テスト、GPUのAgent合成2テスト、fmt、clippyも通過。
- 修正版を常用serviceへ配備し、status / photo / room / aircraftからAgentへ戻る4経路とメニュー開閉後を2560×1080のKMSキャプチャで目視確認。前画面は残らず、Agentのヘッダー・状態集計・背景・カードが表示される。確認画像は `/tmp/miharashi-screen-clear-qa/` に保存し、実データを含むためリポジトリへは追加していない。
- 稼働中の起動ログは `renderer: gles`、`OpenGL ES 3.2 Mesa 25.0.7`、`AMD Radeon Graphics (radeonsi)`。表示先は `/dev/dri/card0`、`amdgpu`、PCI `0000:10:00.0`。カード画像・文字の生成はCPU、移動・拡縮・フェード・画面合成はGPUで行う。
- 再起動後のmiharashiプロセスについて、同じDRM clientの `drm-engine-gfx` を3秒間測定し、実行時間が12.261ms増加した（区間平均0.409%）。プロセス別のGPU利用を確認した値で、GPU全体の使用率やFPSではない。同じclientを指す複数fdは合算していない。

30日有効のtokenはv0.0.45 CLI標準の広いscopeを持つ。miharashiは一覧APIのGETだけを行う。tokenファイルは `/etc/miharashi/t3-agent.token`、root:hermes 0640。

## トークンの自動更新

上の広い権限のトークンは、読み取りだけ(`orchestration:read`)のトークンを週 1 回作り直す仕組みに置き換えた([トークンの更新](../DESIGN-t3-token.md))。token ファイルは `/var/lib/miharashi/t3-agent.token`(実行ユーザーの 0600)。

- 試験用のラベルで 2 回続けて動かし、1 回目で読み取りだけのトークンができること、2 回目で 1 回目のセッションが失効することを確認した。
- 読み取りだけのトークンで、`GET /api/orchestration/shell` は 200、書き込み系の `POST /api/auth/pairing-token` は 403(`insufficient_scope`)。
- 置き場所が root 所有で書けないときは、T3 に何も作らずに止まる。
- 本番に入れ、最初の 1 回で広い権限のトークンを失効させた。そのあとも Agent 画面は取得を続けている。
