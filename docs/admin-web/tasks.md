# Web 管理ページ タスク

- PRD: prd.md / 設計: design.md(Issue [#94](https://github.com/ysogabe/miharashi/issues/94))
- ブランチ: t3/admin-screen-study
- 承認: 2026-10-30(ユーザー)。設計の「実装の順番」0〜7 をタスクにする。8(あると良い R18〜R20)は時間があれば

| ID | 内容 | AC | 依存 | 種類 | 状態 | 開始 SHA | コミット | メモ |
|---|---|---|---|---|---|---|---|---|
| T0 | 契約: `Admin` セクション(allowed_hosts)、`Overlay` と内側の構造体、`OverlayReport`・`LoadedConfig`・`Config::value`、`AdminSources`・`FeedReport`・`TokenState`、`App::admin_sources` の既定、`PanelInput::Restart`。中身は最小、ビルドと既存テストが通る | (土台) | - | quick | 完了 | 8dc24b0 | 4bd36dd | Opus 必須なし。推奨「OverlayErrorKind に Serialize」は T1 に含める |
| T1 | 設定の上書き: `Overlay::check`(文字の検査、origin 照合、格下げ禁止、base_agents_url、無効でも検査)、`load_with_overlay`(印: pid/boot_id/世代、書く・消す・書けない)、`main` のシグナル登録と印、起動ログ、`check-config`、`config-get`、geoframe/aircraft url の最小検査、example.toml の `[admin]` | AC3(読込), AC6(検査), AC7, AC12, AC13(単体), AC14, AC17(Rust 側) | T0 | full | 完了 | 90291ed | 65a86ab, bd6cb0f | Sol 必須 2(agents.url のパス、base の userinfo)→修正、再レビューで指摘なし。見送り: pid 再利用(設計どおり)。設計の「パスは自由」を既存規則に合わせ修正(836596b) |
| T2 | 管理トークンと読み取り API: admin.token_file(厳しい規則、パネルと同値拒否)、完全一致の振り分け表、管理ワーカースレッド、`/api/v1/admin/overview`(items/next/pending_restart/generation/allowed_hosts/error_kind)、`restart`(120 秒、受付済み、boot_failed 409)、実行ループの終了、印の 60 秒後・正常終了時の削除、JSON の nosniff、OPTIONS 405 | AC2, AC3(API), AC10(単体), AC11, AC21, AC22(HTTP 側) | T1 | full | 完了 | be3fd90 | 47f96d9, 39c515a, b25c5ba, 5f65121, d970285 | Sol 5 回(ユーザー許可で上限超え)。r1 必須 4(待ち受けの受信期限で accept 停止、Expect、拒否スレッド、本文の期限)、r2 必須 1(forget のリーク)、r3〜r4 必須各 1(先読み判定の食い違い)、r5 指摘なし。受け入れ: 黙った相手への 408 は相手が送るか切ったとき(tiny_http の制約。設計書に追記)。未読本文の後始末は要求ごとのスレッド(上限なし、tiny_http と同じ方針) |
| T3 | 保存: `PUT /api/v1/admin/config`、世代の比較、設定ファイルの読み直し、check と validate、原子的な書き込み(create_new・hard link の .prev・rename・dir fsync)、tmp の掃除 | AC4, AC5, AC6(API 側), AC8, AC9, AC20 | T2 | full | 完了 | 3a6d49b | 6d898cc, 9d85e66 | Sol 必須なし。推奨 2(generation の形の検証、409 のエラー文に設定ファイルの抜粋)は T4 に含める。参考: 世代比較後の競合は受け入れ(AC20 の文言を調整) |
| T4 | 取得元とトークンの状態: `KioskApp::admin_sources`、`FeedReport` の変換(対応表)、GEOFRAME の last_ok、航空機の last_error、`check_token_file`、エラー文の切り詰め | AC15(単体), AC16 | T2 | full | 完了 | a285759 | 0963c5e, b43ec81 | Sol 必須 2(HA トークンの特例(オーケストレータの指示の誤り)、GEOFRAME の last_ok)→修正、再レビューで指摘なし。据え置き: overview の overlay.error に TOML エラー本文(持ち主だけが見る・設定に秘密なし) |
| T5 | ページ: admin/ の index.html・app.js・app.css、静的配信とヘッダー(CSP ほか)、ログイン・稼働・項目(dirty/世代/readonly/使えるホスト)・保存・再起動(boot_failed で無効)・取得元・トークン、Page Visibility、textContent のみ | AC1(ヘッダー), AC22(app.js の文字列検査) | T3, T4 | full | 完了 | ba23b75 | c8d3947, 9eaf96a, 872fdb2 | Sol 3 回。r1 必須 2(保存中の編集が消える、効いている値の表示)、r2 必須 1(読み直しの応答が保存中のフォームを有効化)、r3 指摘なし。実機の確認は T7 |
| T6 | スクリプトと文書: miharashi-t3-token の関数/main 分離と `config-get`、install.sh の admin.token、USAGE の「管理ページ」節、panel/DESIGN.md §5.2、docs/README.md の索引、example.toml 最終形 | AC17(スクリプト側), AC18 | T1 | full | 完了 | 0e3aa39 | 15d4b85, c0314a8 | Sol 必須 1(テストの後始末の検証漏れ)→修正、再レビューで指摘なし |
| T7 | 実機の確認(自分で): 本番に入れて AC10・AC11・AC13・AC15・AC19・AC20・AC26 と全画面の回帰確認。本番反映は auto mode で拒否されたらユーザーに依頼 | AC1(実機), AC10, AC11, AC13, AC15, AC19, AC20, AC26 | T5, T6 | manual | 実装中 | daccd690a2c9ca6aab32a898133197dd58d9987a | | |
| T8 | 全体レビュー(Sol)の修正: .prev の作成を必須(AC5)、https→http の格下げは allowed_hosts でも不可(AC7、PRD 優先で設計書を直す)、印の 60 秒は書いた時点から(AC13)、既定 IP の転載をやめる(AC27) | AC5, AC7, AC13, AC27 | T6 | full | 実装中 | a450011 | | Sol の全体レビュー 4 件(中 3・低 1)。Opus の security-review は別に進行中 |

AC24(R19)・AC23(R18)・AC25(R20)は T9(あると良い)で、時間があれば足す。AC27(例示用アドレスだけ)は全体のレビューで確かめる。
