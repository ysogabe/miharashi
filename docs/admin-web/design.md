# Web 管理ページ 設計

## 概要

[PRD](prd.md)(Issue [#94](https://github.com/ysogabe/miharashi/issues/94))の要求を、既存の操作パネル向け HTTP サーバー(`crates/miharashi-app/src/panel.rs`)に「管理ページ」と「管理 API」を足して満たす。設定の変更は `/etc/miharashi/config.toml` を書き換えず、実行ユーザーが書ける**上書きファイル**(`/var/lib/miharashi/admin.toml`)に、変えられる項目だけを保存する。反映はキオスクが自分で終了し、systemd の `Restart=always` に再起動させる。

行番号は 2026-10-30 の `main`(6a6a6b4)時点。レビュー(Sol、Opus の安全面)の反映: 2026-10-30。

## 今の作り(調査結果)

### HTTP サーバー(panel.rs)

- `Panel::spawn` が `panel` スレッドを 1 本起こし、`serve` で `server.incoming_requests()` を 1 件ずつ順に処理する(`panel.rs:246-276`、`:351`)。SSE だけ接続ごとに別スレッド(`:369-383`、上限 `MAX_SSE = 4`)。**1 件の処理が長いと、その間パネルのキー操作も待たされる。** 本文の読み取りも同じスレッドで行うので、本文を途中までしか送らないクライアントがいると止まる(今の API も同じ弱点を持つが、本文は 1 KiB まで)。
- 振り分けは `match (&method, path.as_str())`(`:364-443`)。`healthz` だけ認証前に返す(`:354-357`)。それ以外は先頭の 1 か所で `authorized`(`:335-341`)が `Authorization: Bearer` を `same_secret`(`:86-88`、定数時間比較)で見てから、`last_request` を更新し(`:363`)、振り分ける。
- トークンは `read_token`(`:90-111`)。「その他」に読み取りがあれば拒否(`mode & 0o007`)、16 文字未満も拒否。`agents.rs:964-985` の `read_token` はもっと厳しく、0600 で所有者が実行ユーザー、または 0640 で所有者 root かつグループが実行グループ、だけを通す。
- レート制限 `RateLimit`(`:115-142`)は `input` / `screen` / `display` / `rotation` の POST だけに掛かる。
- 応答は JSON だけ(`json_response` `:324-329`、`error` `:331-333`)。HTML・静的ファイル・CORS・CSP のどれも無い。本文の上限は `MAX_BODY = 1024`(`:31`)。
- 実行ループとのつなぎ: `PanelInput`(`Key` / `Screen` / `Rotation`、`:62-69`)を `mpsc` で送り(`:266`、`:347`)、実行ループが `runtime.rs:678-695` で受ける。状態は `p.publish(app.panel_view(), ds)`(`runtime.rs:723-728`)で渡す。`runtime::run(cfg, make_app)`(`runtime.rs:469`)は `Box<dyn App>` を受け取るだけで、写真・T3 などの取得スレッドの状態は `app::kiosk`(`app.rs:87`)が作って各画面が持つ。
- テストは `start(rate)` / `start_with`(`:637-646`)で `127.0.0.1:0` に立て、`ureq` で叩く(`:834-866`)。お手本は `rotation_on_off_reaches_the_run_loop`(`:996-1020`)。

### 設定(config.rs)

- `Config` は 18 セクション(`config.rs:8-27`)。各セクションは `#[serde(default, deny_unknown_fields)]`。
- `Config::parse(&str)`(`:506-510`)= `toml::from_str` → `validate()`。`Config::load(path)`(`:512-515`)、`Config::discover`(`:518-535`)。`main.rs:104-123` の `load_config` が呼び、`check-config` は `main.rs:1042`。
- `validate`(`:537-713`)は値の形だけを見る。`home_assistant.url` は `screen_power::check_url`(`screen_power.rs:77-102`)、`agents.url` は**有効なときだけ** `agents::check_url`(`agents.rs:169-196`、`config.rs:700-701`)。**`geoframe.url` と `aircraft.url` は検査が無い。** `check_url` はどちらも「平文 http はホスト名不可(IP か localhost)」で DNS・mDNS の偽装を防ぐもので、**攻撃者が選んだ IP や https のホストは通る**。`agents::check_url` は https のポートを検査せず、空白・制御文字・`"` も通す。`screen_power::check_url` は `@`(userinfo)を通す。`ui.start_screen = "agents"` は `agents.enabled` が要る(`:612-613`)。`rotation.steps` に無効な画面があっても拒否せず、実行時に飛ばす。
- 依存は `toml = "1.1.8"` だけ。`toml_edit` は無い(コメントを保った書き戻しはできない)。
- サービスは `ExecStart=… run --config /etc/miharashi/config.toml`(`systemd/miharashi.service:38`)で**明示パス**。`/etc/miharashi` は root 0755、`config.toml` は root 0644(`install.sh:13-15`、実機でも確認)。実行ユーザーは**書けず、同じディレクトリへの rename もできない**。`StateDirectory=miharashi`(`:34`)で `/var/lib/miharashi` は実行ユーザー所有。
- `Restart=always`、`RestartSec=3`、`StartLimitIntervalSec=300`、`StartLimitBurst=5`(`miharashi.service:12-13`、`:42-43`)。終了コード 0 でも再起動する(`docs/DESIGN.md:106`、Esc/q もこの経路)。設定が不正だと `load_config` で即終了し、3 秒ごとに 5 回で systemd が止める(戻すには SSH で `systemctl reset-failed`、`docs/USAGE.md:117`)。
- `systemd/miharashi-t3-token` は `agents_key`(`:33-35`)で `$CONFIG`(既定 `/etc/miharashi/config.toml`)から `[agents]` の `url` と `token_file` を `sed` で読み(`:48-50`)、`URL` は**新しいトークンで一覧を読めるかの確認(GET、`Authorization` 付き)にだけ使う**(`:84-86`)。トークンの発行(`t3 auth session issue`、`:71`)は URL を受け取らず、t3 CLI がペアリング済みの自分のサーバーに対して行う。
- 原子的な書き込みの前例は `home.rs:105-116`(一時ファイルに書いて `sync_all` → `rename`。`.prev` は無い)と `photos.rs:332`。
- 写真の取得は、GEOFRAME が返した URL をそのまま `ureq::get`(`photos.rs:446`)で取り、ホストの制限も全体の時間制限も無い(「GEOFRAME を信じる」前提)。

### 接続状態

- `env::FeedStatus`(`env.rs:15-58`、`last_ok` / `failing` / `error`)を HA 室内(`env::Shared = Arc<Mutex<EnvData>>`、`env.rs:69`)、気象庁予報(`weather::Shared`、`weather.rs:76`)、警報(`disaster::HomeShared`、`disaster.rs:129`。**room 画面とは独立に `[alerts]` の設定で動く**)が持つ。
- GEOFRAME は `photos.rs:552-559` の `Online` / `Retrying{attempt, retry_in, error}`。最後に取得できた時刻は無い。
- T3 は `AgentSnapshot { connection, last_success, error_summary, … }`(`agents.rs:26-31`、`:117-131`)。`connection` は `Connecting` / `Live` / `Retrying` / `AuthenticationRequired` / `Forbidden` / `ApiError`。
- 航空機は `Arc<Mutex<Option<(Instant, AircraftView)>>>`(`aircraft.rs:20`)で取得時刻だけ。失敗の状態は無い。地震は状態を持たない。

### 個人情報

- 設定に座標・市町村は無い(自宅の位置は HA から取り `/var/lib/miharashi/home.json` に 0600 で保存)。管理ページが扱う項目のうち、LAN の IP(`agents.url`、`home_assistant.url`、`listen`)と受信機のホスト名(`aircraft.url`)は、公開するスクリーンショットに出せない。

## 方針

### 選んだ案

1. **上書きファイル方式。** 変えられる項目だけを持つ小さな TOML を `/var/lib/miharashi/admin.toml` に書き、起動時に設定ファイルの上に重ねる。
   - `/etc/miharashi` に書けない問題、`toml_edit` を足す問題、保存の失敗で起動できなくなる問題の 3 つを同時に避けられる。
   - 代償は「設定の出どころが 2 つになる」こと。ページには項目ごとに「効いている値 / ファイルの値 / 上書き / 次回起動」を並べ、「ファイルの値に戻す」を置く。`check-config` と起動ログに上書きの件数と値を出し、SSH で直す人が気づけるようにする。外のスクリプトは `miharashi config-get` で同じ規則の値を取る(方針 8)。
2. **上書きは型のある構造体で読む(汎用のテーブル結合はしない)。** `Overlay` と内側の構造体の全部に `deny_unknown_fields` を付け、変えられる項目だけを `Option` で持つ。読み込みでも API でも、この構造体を通らない項目は書けない。合わせた `Config` に対して既存の `validate()` を掛ける。
3. **トークンを送る接続先は守る。** `home_assistant.url` は上書きの対象から外す(表示だけ)。`agents.url` は、root 所有の設定ファイルにある `[admin] allowed_hosts` か、設定ファイルの `agents.url` と同じホストだけを認める。既存の `check_url` は DNS の偽装しか防がないので、ホストの制限は上書きに固有の規則として足す。上書きにある URL は、画面が無効でも必ず検査する。
4. **起動時の安全弁。** 上書きが読めない・合わせた設定が検証に通らない・許されないホストを含むときは、設定ファイルだけで起動し、理由を stderr と管理 API に出す。さらに、上書きを使って起動した直後に異常終了したときは、次の起動で上書きを外す(印のファイル)。管理ページから直せる状態を必ず残す。守る範囲は「管理ページが書いたもの」まで。設定ファイル自体が壊れていれば今までどおり起動しない。
5. **認証は管理ページ専用のトークン。** ブラウザは `Authorization` を自動で付けないので、ページの JavaScript が `sessionStorage` のトークンをヘッダーに付ける。Cookie は使わない。設定の書き換えと再起動はキー操作より強い権限なので、常時電源の入った Echo Show 5 に置くパネルのトークンとは分ける。ファイルだけでなく値も別でなければならず、同じ値なら管理ページを無効にする。CSRF が成り立たない前提は「安全面」に規則として書く。
6. **再起動は自分で終了して `Restart=always` に任せる。** `systemctl` は使えない。起動から 120 秒間と、受け付けてから終了するまでは 429 で断る。
7. **管理のリクエストは別スレッドで処理する。** パネルのスレッドは認証とパスの判定だけをして、`/admin*` と `/api/v1/admin/*` のリクエストを管理用のワーカースレッド(同時 1 本)に渡す。本文の読み取り・検証・保存・応答はワーカーで行う。遅いクライアントや fsync でパネルのキー操作を止めない。
8. **設定の値を取る口を 1 つにする。** `miharashi config-get <key>` サブコマンドが、起動時と同じ規則(設定ファイル + 上書き、壊れた・許されない上書きは無視)で値を出す。`miharashi-t3-token` はこれで URL を取る。`sed` で上書きファイルを読む案は、Rust 側が無視するファイルの値を拾ってしまうので採らない。
9. **ページは 3 ファイル(HTML・JS・CSS)をバイナリに埋め込む。** 外部のライブラリ・フォント・CDN は使わない。CSP は `'self'` だけ。値は `textContent` でしか出さない。

### 選ばなかった案

| 案 | 理由 |
|---|---|
| `/etc/miharashi/config.toml` を直接書く(所有者を実行ユーザーに変え、`toml_edit` でコメントを保つ) | ディレクトリが root 0755 なので一時ファイルからの rename ができず、原子的に置き換えられない。インストール手順と既存の配備の変更が要る。保存に失敗すると起動できなくなり、ページから直せない |
| sudoers か polkit で `systemctl restart` を許す | root の設定を増やす。今の「一般ユーザーだけで動く」方針に反する。終了して `Restart=always` に任せれば足りる |
| 設定の再読み込み(SIGHUP など) | 取得スレッドの作り直しが全セクションに及び、工数が大きい。ユーザーは「保存して再起動」を選んだ |
| `home_assistant.url` も上書きの対象にする(許可ホストで制限) | HA のトークンは家の機器を操作できる。送り先を変えられる口を、平文 HTTP で流れる管理トークンに開けない。PRD の場面で動かすのは GEOFRAME と T3 だけ |
| `agents.url` のホストをプライベート IP に限る | LAN の攻撃者は LAN の中にサーバーを立てられるので効かない。許可ホストの一覧(root 所有の設定ファイル)で限る |
| パネルのトークンを管理ページにも使う | 権限の強さが違う。片方を取り換えたときにもう片方が巻き込まれる |
| Cookie セッション | CSRF 対策(トークン・SameSite)が要る。Bearer をヘッダーで送る方が今の仕組みの延長で済む |
| 既存の `/api/v1/events`(SSE)をページでも使う | パネルのトークンが要る。接続数の上限(4)をパネルと分け合う。ページは 5 秒ごとの取得で足りる |
| 別プロセス(Node・Python)の管理サーバー | 常駐物は Rust の方針。ポートとトークンが増える |
| `miharashi-t3-token` が上書きファイルを `sed` で読む | Rust が無視する(壊れた・検証に通らない・許されないホストの)上書きの値を拾い、そこへトークンを送ってしまう。規則が 2 か所になる |
| 項目ごとの PATCH API | 別のタブや SSH との競合に弱い。全体を世代付きで置き換える方が単純で、409 で守れる |

## 構成

```mermaid
flowchart LR
    B[ブラウザ: /admin のページ<br>sessionStorage のトークンを Bearer で送る] -- "GET /api/v1/admin/overview(見えている間 5 秒ごと)" --> S[panel スレッド(tiny_http)<br>認証とパスの判定だけ]
    B -- "PUT /api/v1/admin/config(generation 付き)" --> S
    B -- "POST /api/v1/admin/restart" --> S
    S -- "管理のリクエストを渡す(同時 1 本)" --> W[admin ワーカースレッド]
    W -- "base を読み直し → 重ねて validate + 上書きの規則 → 一時ファイル → rename" --> O["/var/lib/miharashi/admin.toml"]
    W -- "PanelInput::Restart" --> R[実行ループ(runtime.rs)]
    R -- "exit 0" --> SD[systemd Restart=always]
    SD -- "run --config /etc/miharashi/config.toml" --> L["Config::load_with_overlay"]
    C["/etc/miharashi/config.toml<br>[admin] allowed_hosts"] --> L
    O --> L
    M["/var/lib/miharashi/admin.boot(印)"] --> L
    L -- "合わせた Config か、失敗なら base と理由" --> R
    A[app::kiosk が作る取得スレッド群] -- "AdminSources(読むだけの参照)" --> W
    T[miharashi-t3-token] -- "miharashi config-get agents.url" --> L
```

モジュールの変更:

| 場所 | 変更 |
|---|---|
| `config.rs` | `Admin` セクション(`allowed_hosts` を含む)、`Overlay` 構造体と上書きに固有の規則(`Overlay::check(&base)`)、`Config::load_with_overlay`、`LoadedConfig`(base・effective・report)、`Config::value(key)`、印のファイルの判定。`geoframe.url` と `aircraft.url` の最小の検査 |
| `main.rs` | `load_config` が `LoadedConfig` を返す。`run` では上書きが効いていれば直後に印を書く。`check-config` が上書きの件数と値を出す。`config-get <key>` サブコマンド |
| `panel.rs` | 管理トークンの読み込み、管理ワーカースレッド、`/admin*` の静的配信、`/api/v1/admin/*`、`PanelInput::Restart`、管理用の本文上限とレート制限、`AdminSources` と `FeedReport`、JSON 応答への `nosniff` |
| `screen.rs`、`app.rs` | `App` トレイトに `admin_sources(&self) -> AdminSources`(既定は空)。`KioskApp` が各画面の取得状態の参照を返す |
| `runtime.rs` | `PanelInput::Restart` を受けて終了。`LoadedConfig`・起動時刻・`app.admin_sources()` を `Panel::spawn` に渡す。印のファイルの 60 秒後の削除・正常終了時の削除(書くのは `main`) |
| `photos.rs` | GEOFRAME の「最後に取得できた時刻」(通信が成功した時刻。キャッシュの表示は含まない) |
| `aircraft.rs` | 取得の失敗を状態として持つ(`last_error`) |
| `admin/`(新規、`panel.rs` から `include_str!`) | `index.html`、`app.js`、`app.css` |
| `systemd/miharashi-t3-token` | 関数の定義と本処理を分け、URL を `miharashi config-get` で取る |
| `config/example.toml`、`docs/USAGE.md`、`docs/panel/DESIGN.md` §5.2、`systemd/install.sh` | `[admin]` の説明、トークンの置き方と重み、API の表 |

## インターフェース

### 設定(`config/example.toml` に足す)

```toml
[admin]
# ブラウザから見る管理ページ(docs/admin-web/design.md)。[panel] が有効なときだけ動き、同じ listen で配信する。既定は無効。
# 管理トークンを持つ人は、画面の有効/無効と GEOFRAME・tar1090・T3 の接続先(T3 は下の allowed_hosts の範囲)を変え、
# キオスクを再起動できる。Home Assistant の接続先はここでは変えられない(表示だけ)。
# トークンは panel.token とは別のファイルで、値も別にする(同じ値なら管理ページは無効)。0600 で実行ユーザー所有、
# または 0640 で root:<実行ユーザー>。16 文字以上を 1 行。
# 例: head -c 24 /dev/urandom | base64 | sudo install -o root -g hermes -m 640 /dev/stdin /etc/miharashi/admin.token
# enabled = false
# token_file = "/etc/miharashi/admin.token"
# 管理ページから保存した値の置き場所。設定ファイルより優先する。あれば [admin] が無効でも読む。
# 壊れていたり検証に通らなければ無視して、この設定ファイルだけで起動する(理由は journal と管理ページに出る)。
# overlay_file = "/var/lib/miharashi/admin.toml"
# 管理ページから T3 の接続先(agents.url)に使えるホスト。この設定ファイルの agents.url のホストは常に使える。
# ホスト名か IP アドレスだけを書く(ポートとパスは管理ページで自由に変えられる)。
# allowed_hosts = []
```

- `admin.enabled = true` で `panel.enabled = false` は `validate` で拒否(「admin.enabled = true には panel.enabled = true が必要」)。
- `overlay_file` は `admin.enabled` に関係なく、ファイルがあれば読む。管理ページを止めても保存した値が消えないため。
- `allowed_hosts` の各要素はホスト名(英数字・`-`・`.`)か IP アドレス(IPv6 は `[]` 無し)だけ。それ以外は `validate` で拒否。

### 上書きファイル(`admin.toml`)

変えられる項目だけ。全部 `Option`。無いキーは「上書きしない」。`home_assistant` のテーブルは無い(あれば知らないキーとして無視・拒否)。

```toml
# miharashi の管理ページが書くファイル。手で直してもよいが、ここに無い項目は書けない(起動時に無視される)。
[geoframe]
url = "http://192.0.2.20:8080"
[agents]
enabled = true
url = "http://192.0.2.30:3773"
[aircraft]
enabled = false
url = "https://adsb.example.test/tar1090/data/aircraft.json"
[room]
enabled = true
[cube]
enabled = false
[vrm]
enabled = false
```

```rust
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Overlay {
    pub geoframe: OverlayUrl,
    pub agents: OverlayScreenUrl,
    pub aircraft: OverlayScreenUrl,
    pub room: OverlayScreen,
    pub cube: OverlayScreen,
    pub vrm: OverlayScreen,
}
// 内側の構造体にも必ず #[serde(default, deny_unknown_fields)] を付ける(外側だけでは内側の知らないキーが黙って捨てられる)
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OverlayUrl { pub url: Option<String> }
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OverlayScreenUrl { pub enabled: Option<bool>, pub url: Option<String> }
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OverlayScreen { pub enabled: Option<bool> }

impl Overlay {
    /// 上書きに固有の規則。読み込みと保存の両方から呼ぶ(1 か所に書く)。
    /// - 3 つの url: 共通の文字の検査(制御文字・空白・'"'・'\\'・'@' を拒否、http(s) で始まり、host が解釈でき、port は数字)
    /// - agents.url: 画面が無効でも agents::check_url を通す。host が base.admin.allowed_hosts か base.agents.url の host と一致すること
    /// - aircraft.url / geoframe.url: 共通の文字の検査だけ(トークンを送らない)
    pub fn check(&self, base: &Config) -> Result<(), String>;
    pub fn apply(&self, cfg: &mut Config);          // Some の項目だけ代入
    pub fn entries(&self) -> Vec<(&'static str, String)>; // ("aircraft.enabled", "false") の一覧(ログ・check-config 用)
    pub fn is_empty(&self) -> bool;
}
pub struct OverlayReport {
    pub path: PathBuf,
    pub exists: bool,
    pub applied: Vec<(&'static str, String)>,   // 効いた項目
    pub error: Option<String>,                   // 無視した理由(読めない・知らないキー・規則違反・検証に通らない・前回の起動が早く終わった)
}
/// 起動時に読んだもの一式。実行ループと panel が持ち回る。
pub struct LoadedConfig {
    pub base_path: Option<PathBuf>,
    pub base: Config,       // 上書き前(ページの「ファイルの値」)
    pub effective: Config,  // 実際に使う(上書き後。無視したときは base と同じ)
    pub report: OverlayReport,
}
impl Config {
    /// base を読んで検証し、base.admin.overlay_file があれば check → apply → validate。どこかで失敗したら effective = base にし、理由を report に入れる。
    /// 印(overlay_file と同じディレクトリの admin.boot)が残っていれば(前回、上書きで起動して 60 秒以内に異常終了)、overlay を読まずに無視し、理由を report に入れる。
    /// consume_marker = true(run)なら印を消し、新しい印を書く準備をする。false(check-config、config-get)なら印に触らない。
    /// overlay_file と印のパスは base から決まる(テストでは base の admin.overlay_file に一時ディレクトリを書く)。
    pub fn load_with_overlay(base: &Path, consume_marker: bool) -> Result<LoadedConfig, String>;
    /// 管理ページが扱うキー("aircraft.enabled" など)の値を文字列で返す。知らないキーは None。
    pub fn value(&self, key: &str) -> Option<String>;
}
```

- 読み込みの順: base を `Config::load`(失敗なら今までどおりエラーで終了)→ 印が残っていれば overlay を無視(`report.error` = 「前回の起動が 60 秒以内に終わったので上書きを外した」。印は消す)→ overlay が無ければ `report.applied` は空 → あれば `toml::from_str::<Overlay>`(失敗は `report.error`)→ `check(&base)`(失敗は `report.error`)→ `base.clone()` に `apply` → `validate`(失敗は `report.error`、effective は base)。
- 印のファイル(`admin.boot`、`overlay_file` と同じディレクトリ): 上書きが効いた(`report.applied` が空でない)状態で `load_with_overlay` が返ったら、**`main` がすぐに書く**(表示の初期化、`app::kiosk`、DRM・GL の初期化より前)。上書きで有効にした画面(cube・vrm・agents)の初期化はこれらの中で失敗するので、実行ループに入ってから書くのでは遅い。実行ループが 60 秒動いたら消す。正常終了の経路(SIGTERM・SIGINT、キーの quit、`max_seconds`、管理ページの再起動)でも消す。描画エラーでの終了、`kill -9`、初期化の途中の失敗では残る。管理ページ由来の再起動は起動から 120 秒たたないと受け付けないので、誤って外すことはない。`check-config` と `config-get` は印を見て結果に反映するが、消しも書きもしない(`consume_marker = false`)。
- `check-config` の出力(上書きが効いているとき):

```
OK
上書き: /var/lib/miharashi/admin.toml(2 項目)
  aircraft.enabled = false
  agents.url = http://192.0.2.30:3773
```

無視したときは `上書き: /var/lib/miharashi/admin.toml を無視した: <理由>` を stderr に出し、終了コードは 0(base は正しい)。起動時も同じ 1 行を stderr に出す。`check-config` は印を見るが消さない。

- `miharashi config-get <key> [--config FILE]`: `load_with_overlay` を通した effective の値を 1 行で stdout に出す(印は消さない)。知らないキーは終了コード 1、設定が読めなければ 2。`miharashi-t3-token` の `URL=${T3_URL:-$(miharashi config-get agents.url --config "$CONFIG")}` で使う。

### HTTP API

既存の表(`docs/panel/DESIGN.md` §5.2)に足す。認証は管理トークン(`Authorization: Bearer`)。`/admin`、`/admin/app.js`、`/admin/app.css` の静的ファイルだけ認証なし(中身は固定で、データを含まない)。`[admin]` が無効なら `/admin*` と `/api/v1/admin/*` は 404。

振り分けは**完全一致の表**にし、経路ごとに要るトークン(パネル / 管理 / なし)を決める。前方一致や「どちらかのトークンなら通す」はしない。パネルのスレッドは、認証(その経路のトークン)とパスの判定だけをして、管理の `Request` を管理ワーカーのチャネルに渡す。ワーカーが忙しければ `429 {"error":"管理の処理中", "retry_after_s":1}`。ワーカーは 1 本で、本文の読み取り(16 KiB 上限、読み取り全体で 5 秒の期限。超えたら 408 で切る)・検証・保存・応答を行う。静的ファイルも同じワーカーで返す。

| メソッド | パス | 認証 | 内容 |
|---|---|---|---|
| GET | `/admin`、`/admin/app.js`、`/admin/app.css` | なし | 埋め込んだ静的ファイル。`Content-Type` はそれぞれ `text/html; charset=utf-8`、`text/javascript`、`text/css`。ヘッダーに `Content-Security-Policy: default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'`、`X-Frame-Options: DENY`、`Cache-Control: no-store`、`X-Content-Type-Options: nosniff`、`Referrer-Policy: no-referrer` |
| GET | `/api/v1/admin/overview` | 管理 | 下の JSON。ページはタブが見えている間、5 秒ごとに読む |
| PUT | `/api/v1/admin/config` | 管理 | 本文 `{"generation": "<overview で受け取った値>", "overlay": {...}}`。`overlay` は上書き全体(`Overlay` と同じ形の JSON。無いキーと `null` は上書きしない)。手順: 今の上書きファイルの世代と `generation` を比べ、違えば `409 {"error":"上書きファイルが変わっている。読み直してから保存する"}` → 今の設定ファイルを読み直して `parse`(失敗は `409 {"error":"設定ファイルが読めない: …"}`)→ `Overlay::check(&base)`(失敗は `400`)→ 重ねて `validate`(失敗は `400 {"error":"<validate の文>"}`)→ 書く(失敗は `500`)→ `204`。知らないキー(内側も)・読めない JSON は `400`。本文の上限は 16 KiB(`413`) |
| POST | `/api/v1/admin/restart` | 管理 | 本文なし。起動から 120 秒未満、または既に受け付けて終了待ちなら `429 {"error": "...", "retry_after_s": n}`。起動中(`Shared.state` がまだ無い)は `503`。受け付けたら `restart_requested` を立て、印のファイルを消し、`202 {"restart_in_s": 1}` を返してから実行ループに `PanelInput::Restart` を送る |
| POST | `/api/v1/admin/probe` | 管理 | (R18、あると良い)`{"target": "geoframe" \| "agents" \| "aircraft", "url": "..."}`。URL は保存と同じ規則(`Overlay::check` の該当する URL の規則。許可ホストを含む)を通してから GET を試す。規則: `Authorization` を付けない、`max_redirects(0)`、`timeout_global(3s)`、応答は状態コードと所要時間と短い理由だけ(本文・ヘッダーは返さない)、対象と送り元の IP を journal に 1 行出す。`200 {"ok": bool, "status": 200 \| null, "elapsed_ms": n, "error": "..." \| null}`。管理ワーカーとは別の probe スレッドで行い、同時に 1 本まで(`429`)。`home_assistant` は対象外 |
| OPTIONS | すべて | — | `405`(CORS の事前確認に応えない。`Access-Control-Allow-*` はどの応答にも付けない) |

JSON の応答(既存のパネル API を含む)にも `X-Content-Type-Options: nosniff` を付ける。トークンは `Authorization` ヘッダーからだけ受け取り、クエリ(`?token=`)や本文のものは見ない。

世代(`generation`)は上書きファイルの内容の SHA-256 の先頭 16 桁(16 進)。ファイルが無ければ `"none"`。

`GET /api/v1/admin/overview` の形(`schema_version` は管理 API だけのもので、パネルの `status` とは別):

```json
{
  "schema_version": 1,
  "host": "kiosk-example",
  "version": "0.3.0",
  "uptime_s": 5321,
  "screen": {"id": "photo", "title": "写真"},
  "config_file": {"path": "/etc/miharashi/config.toml", "modified_at": "2026-10-30T09:12:00+09:00"},
  "overlay": {
    "path": "/var/lib/miharashi/admin.toml",
    "exists": true,
    "generation": "9f2c1a7e3b5d8c40",
    "error": null,
    "error_kind": null,
    "pending_restart": false
  },
  "allowed_hosts": ["192.0.2.30", "t3.example.test"],
  "items": [
    {"key": "geoframe.url", "kind": "url", "effective": "http://192.0.2.20:8080", "file": "http://127.0.0.1:8080", "overlay": "http://192.0.2.20:8080", "next": "http://192.0.2.20:8080", "note": null},
    {"key": "home_assistant.url", "kind": "readonly", "effective": "http://192.0.2.10:8123", "file": "http://192.0.2.10:8123", "overlay": null, "next": "http://192.0.2.10:8123", "note": "設定ファイルでだけ変えられる"},
    {"key": "agents.url", "kind": "url", "effective": "http://192.0.2.30:3773", "file": "http://192.0.2.30:3773", "overlay": null, "next": "http://192.0.2.30:3773", "note": "ホストは allowed_hosts の範囲"},
    {"key": "aircraft.enabled", "kind": "bool", "effective": true, "file": true, "overlay": null, "next": true, "note": null},
    {"key": "room.enabled", "kind": "bool", "effective": true, "file": true, "overlay": null, "next": true, "note": "HA の url か room.forecast_area が無いと画面は出ない"},
    {"key": "vrm.enabled", "kind": "bool", "effective": false, "file": false, "overlay": null, "next": false, "note": "有効にするには vrm.model と vrm.screens が設定ファイルに要る"}
  ],
  "feeds": [
    {"id": "geoframe", "label": "GEOFRAME(写真)", "state": "ok", "last_ok": "2026-10-30T10:40:12+09:00", "error": null},
    {"id": "home_assistant", "label": "Home Assistant(室内)", "state": "failing", "last_ok": "2026-10-30T10:20:00+09:00", "error": "接続できない"},
    {"id": "weather", "label": "気象庁(予報)", "state": "ok", "last_ok": "...", "error": null},
    {"id": "disaster", "label": "気象庁(警報)", "state": "ok", "last_ok": "...", "error": null},
    {"id": "aircraft", "label": "tar1090(航空機)", "state": "not_running", "last_ok": null, "error": null},
    {"id": "agents", "label": "T3(エージェント)", "state": "auth_required", "last_ok": null, "error": "401"}
  ],
  "tokens": [
    {"id": "home_assistant", "path": "/etc/miharashi/ha-token", "state": "ok"},
    {"id": "agents", "path": "/var/lib/miharashi/t3-agent.token", "state": "missing"},
    {"id": "panel", "path": "/etc/miharashi/panel.token", "state": "loose_permissions"},
    {"id": "admin", "path": "/etc/miharashi/admin.token", "state": "ok"}
  ],
  "restart": {"allowed": true, "retry_after_s": 0, "requested": false},
  "generated_at": "2026-10-30T10:40:15+09:00"
}
```

- `items[].effective` は今動いているプロセスの値(`LoadedConfig.effective`)。`file` は起動時に読んだ設定ファイルの値(`LoadedConfig.base`)。`overlay` は**今の `admin.toml` の値**(リクエストのたびに読み直す)。`next` は「次回起動の値」で、今の設定ファイルを読み直し、今の `admin.toml` を起動時と同じ規則(壊れている・規則違反なら無視)で重ねて求める。設定ファイルが読めないときは `next` を `null` にし、`overlay.error` に理由を入れる。
- `overlay.pending_restart` は、どれかの項目で `next != effective` のとき `true`。壊れた上書きの警告(`overlay.error`)とは別。ただし `overlay.error` が「前回の起動が 60 秒以内に終わった」(印による無視)のときは、上書きファイルがまだ残っていて `next != effective` になるが、その上書きで起動に失敗した直後なので、ページは「この上書きで起動に失敗した。直してから再起動する」と出し、再起動ボタンを強調しない(`overlay.error_kind: "boot_failed"` で区別する)。
- `allowed_hosts` の照合: ホスト名は小文字にそろえ、IPv6 の `[]` は外し、完全一致だけ(末尾一致やワイルドカードは無い)。設定ファイルの `agents.url` のホストも同じ規則で比べる。
- `feeds[].error` と `overlay.error` は 200 文字で切り、URL のクエリは落とす。外部サービスの応答本文はそのまま入れない(既存の `error_summary` と同じ短い要約)。
- `feeds[].state` と各取得元の対応:

| id | 元 | `not_running`(取得が動いていない) | `connecting`(初回の取得前) | `ok` | `failing` | `auth_required` / `forbidden` | `last_ok` の意味 |
|---|---|---|---|---|---|---|---|
| `geoframe` | `photos.rs` の状態 | 写真の取得が無い(無効化は無いので起動直後だけ) | 最初の一覧を取る前 | `Online` | `Retrying`(`error` に理由) | — | GEOFRAME との通信が最後に成功した時刻(新しく足す。キャッシュの表示は含まない) |
| `home_assistant` | `env::Shared` の `FeedStatus` | `room` が無効、または HA の url が空 | `last_ok` 無しで `failing` でない | `!failing` | `failing` | — | `FeedStatus.last_ok` |
| `weather` | `weather::Shared` | `room` が無効、または予報の地域が決まらない | 同上 | 同上 | 同上 | — | 同上 |
| `disaster` | `disaster::HomeShared` | `[alerts] disaster = false` または HA の url が空(**`room` とは無関係**) | 同上 | 同上 | 同上 | — | 同上 |
| `aircraft` | `aircraft::Shared` + 新しい `last_error` | `aircraft.enabled = false` | 一度も取れていない | 直近が成功 | 直近が失敗 | — | 最後に `aircraft.json` を取れた時刻 |
| `agents` | `AgentSnapshot` | `agents.enabled = false` | `Connecting` | `Live` | `Retrying` / `ApiError` | `AuthenticationRequired` / `Forbidden` | `last_success` |

`AdminSources` の各参照は `Option` で、`None` は「取得が動いていない」を表す。理由は `AdminSources` を作る側(`KioskApp::admin_sources`)が決め、文字列で持つ(`"room.enabled = false"` など)。

- `tokens[].state`: `ok` / `missing` / `unreadable`(`open` できない) / `wrong_owner` / `loose_permissions`。判定関数 `check_token_file(path, uid, gid) -> TokenState` は `agents.rs` の `read_token` と同じ規則で、中身は読まず `open` の成否と `metadata` だけを見る(値は出さない)。HA のトークンは今は検査していないので、`loose_permissions` は警告として出すだけで動きは変えない。管理トークン自体が不正なときは管理ページが無効なので、この表には出ない(journal で確かめる)。
- `restart.retry_after_s` は起動から 120 秒に足りない秒数。`requested` は受け付けて終了待ちなら `true`。

### ページ(`/admin`)

1 画面。上から順に:

1. **ログイン**: トークンを貼る欄(`type="password" autocomplete="off"`)。`sessionStorage` に置く(タブを閉じると消える)。401 が返ったらここに戻る。ログアウトボタンで消す。
2. **稼働**: ホスト・バージョン・起動からの時間・今の画面・設定ファイルの更新時刻。上書きが無視されていれば赤い帯で理由。
3. **接続先と画面**: 項目ごとに 1 行。入力欄(URL)かスイッチ(有効/無効)、「上書き」のチェック(外すと「ファイルの値に戻す」= `null`)、その右に「ファイルの値」「次回起動の値」。`readonly` の項目(HA の URL)は値だけ出し、入力欄は無い。T3 の URL の下に「使えるホスト: …」。下に「保存」。
   - フォームはログイン直後と保存成功後に `overview` から作る。5 秒ごとの更新は、稼働・取得元・トークンの表だけを描き直し、**フォームには触らない**。フォームに手が入っている(dirty)間に `overlay.generation` が変わったら、「別の場所で上書きが変わった。読み直すか」の帯を出す。
   - 保存は、フォーム全体から `Overlay` の JSON を組み立て(未変更の項目もそのまま含める)、読み込んだときの `generation` を付けて `PUT`。`409` なら読み直しを促す。`400` ならエラー文をそのまま出す。
4. **再起動**: 「再起動して反映」ボタン。`pending_restart` が `true` のときだけ強調。押すと確認を 1 回出し、`202` のあと「再起動中…」と出して 5 秒後から `overview` を取り直す(`uptime_s` が小さくなったら「再起動しました」)。`429` なら残り秒数。
5. **取得元**: `feeds` の表(状態・最後に取得・エラー)。
6. **トークン**: `tokens` の表(パス・状態)。値は出さない。

JavaScript はフレームワークを使わず、`fetch` と DOM 操作だけ。応答の文字列は必ず `textContent` で出し、`innerHTML`・`insertAdjacentHTML`・`document.write` は使わない(無いことをテストで文字列検査する)。URL をリンク(`<a href>`)にしない。エラー文は 300 文字で切る。定期更新は Page Visibility API でタブが見えているときだけ(平文でトークンが流れる回数を減らす)。文字は日本語。幅 400px 以上のスマートフォンでも読める簡単な CSS。

### 実行ループと App

- `PanelInput::Restart` を足す。`runtime.rs:678-695` の受け取りで `restart = true` にし、`for` の後で `break "管理ページからの再起動"`。終了処理は既存(`:816-845`)のまま、終了コード 0。
- `runtime::run` は `LoadedConfig` を受け取る(`cfg` は `effective`)。印は `main` が `load_with_overlay` の直後に書き(上の「印のファイル」)、実行ループが 60 秒後と正常終了の経路で消す。`Panel::spawn` に `LoadedConfig`(base・report・base_path)、`started: Instant`、`app.admin_sources()` を渡す。
- `App` トレイト(`screen.rs`)に `fn admin_sources(&self) -> AdminSources { AdminSources::default() }` を足す。`KioskApp`(`app.rs`)が、各画面が持つ `env::Shared`、`weather::Shared`、`disaster::HomeShared`、写真の状態、`aircraft::Shared`、T3 のスナップショットの参照を `Option` で詰めて返す。参照は読むだけ。
- `AdminSources` と `FeedReport` の型は `panel.rs` に置く(`app.rs` が依存する向きは今の `panel_view` と同じ)。

## データ

| もの | 置き場所 | 形 | 権限 |
|---|---|---|---|
| 上書き | `admin.overlay_file`(既定 `/var/lib/miharashi/admin.toml`) | TOML(上の `Overlay`) | 実行ユーザー所有、0600(秘密は入らないが、読むのも同じユーザーだけなので最小にする) |
| 直前の上書き | 同じ場所の `admin.toml.prev` | 同上 | 同上 |
| 印 | 同じ場所の `admin.boot` | 空ファイル(起動時刻を 1 行書いてもよい) | 同上 |
| 管理トークン | `admin.token_file`(既定 `/etc/miharashi/admin.token`) | 1 行、16 文字以上 | 0600 実行ユーザー、または 0640 root:実行グループ |

- 書き込みの手順(どの時点で止めても `admin.toml` は前の内容か新しい内容のどちらか):
  1. 同じディレクトリに `admin.toml.tmp-<pid>` を `create_new`(既にあれば失敗)で作って書き、`sync_all`。
  2. 既存の `admin.toml` があれば、`admin.toml.prev` を消してから `hard_link(admin.toml, admin.toml.prev)`(コピーではなくリンク。失敗しても続ける。`.prev` は保険)。
  3. `rename(admin.toml.tmp-<pid>, admin.toml)`(1 回だけ)。
  4. ディレクトリを `open` して `sync_all`。
  起動時と保存時に、残った `admin.toml.tmp-*` は消す。
- 上書きを全部取り消して保存したとき(`Overlay::is_empty`)は、2 のあと `admin.toml` を削除する(`.prev` は残る)。
- 移行は不要。ファイルが無ければ今までと同じ動き。やめるときはファイルを消す。
- `miharashi-t3-token` との関係:
  - URL は `miharashi config-get agents.url` で取る(= 次回起動の値。許されないホストの上書きは無視される)。接続の確認(GET)に使い、届かなければ「トークンは置き換えません」で止まる(今と同じ)。
  - トークンの発行先は t3 CLI がペアリング済みのサーバーで決まり、URL では変わらない。保存して再起動する前に更新が走っても、発行されるトークンは同じサーバーのもの。URL が同じサーバーの別の IP やポートなら問題なく、別のサーバーなら確認で止まる(安全側)。別の T3 サーバーへ移すには、そのサーバーとのペアリングと `allowed_hosts` の追加を SSH で行う(PRD「やらないこと」)。
  - スクリプトは関数の定義と本処理(`main`)を分け、`BASH_SOURCE` で直接実行されたときだけ `main` を呼ぶ。テストは `source` して関数だけを呼ぶ。

## 安全面

- **脅威の置き方**: 管理トークンは平文 HTTP で LAN を流れる(パネルと同じ受け入れ)。盗まれたときにできることを「画面を消す・再起動する・GEOFRAME と tar1090 の接続先を変える・T3 の接続先を許可ホストの範囲で変える」に限り、**HA のトークンの送り先には届かない**ようにする。これが HA の URL を上書きの対象から外し、T3 の URL に許可ホストを置く理由。
- **入力の検証**: PUT の本文は `serde_json` で `{generation, overlay: Overlay}` に読む(外側も内側も `deny_unknown_fields`)。読めない・知らないキーは 400。文字列は 2 KiB まで。`Overlay::check(&base)` で上書きに固有の規則(共通の文字の検査、`agents.url` のホストの制限、画面が無効でも URL を検査)を掛け、合わせた `Config` を `validate()` で検証(既存の規則をそのまま使う。`agents.url` の形、`ui.start_screen` と `enabled` の整合)。`geoframe.url` と `aircraft.url` には最小の検査(`http://` か `https://` で始まり、host が解釈できる)を `validate` に足す。この検査は base にも掛かるが、既定値と今の本番の値は通る。
- **共通の文字の検査(上書きの URL)**: 制御文字・空白・`"`・`\`・`@` を拒否し、`http://` か `https://` で始まり、`ureq::http::Uri` で読めて、port があれば数字。journal・TOML・スクリプトでの読み方が食い違わないようにする。
- **権限の分離**: 振り分けは完全一致の表で、経路ごとに要るトークンを 1 つ決める。管理トークンはパネルの経路で 401、パネルのトークンは管理の経路で 401。トークンの比較は `same_secret`。管理トークンは `agents.rs` の厳しい規則で読み、満たさないか、パネルのトークンと同じ値(`same_secret` で比べる)なら、管理ページだけを無効にして起動する(キオスクは止めない。stderr に 1 行)。
- **CSRF が成り立たない前提(守る規則)**: (1) Cookie を使わない。(2) 状態を変える経路はすべて `Authorization` ヘッダーを要する(このヘッダーがあると、ブラウザは他のサイトからの送信の前に事前確認を行う)。(3) `Access-Control-Allow-*` を一切返さず、`OPTIONS` は 405。(4) トークンを URL のクエリや本文で受け取らない。この 4 つのどれかを崩す変更は、CSRF 対策を別に足す必要がある。
- **Host ヘッダーと DNS rebinding**: 確かめていない。すべての経路が `Authorization` を要するので、他のサイトから名前を向け直しても呼べない。前提 (2) が崩れると効いてくるので、ここに書いておく。
- **書けるファイルの範囲**: 書くのは `overlay_file` とその `.prev` / `.tmp-*`、印だけ。パスは設定から取り、リクエストからは受け取らない。`/etc/miharashi` には触らない。
- **接続先の誘導**: `agents.url` のホストは `allowed_hosts` か設定ファイルの `agents.url` のホストに限る。既存の `check_url` が「平文 http はホスト名不可(IP か localhost)」で DNS の偽装を防ぐ。GEOFRAME と tar1090 はトークンを送らないので、形の検査だけ。GEOFRAME は写真の URL をキオスクが取りに行くので、`geoframe.url` を変えると任意の URL(ループバックを含む)を取らせられる(「リスクと対策」。写真の URL を同じ origin に限る対応は別の Issue)。
- **再起動の抑止**: 起動から 120 秒間と、受け付けてから終了するまでは 429。起動の数で数えると 300 秒に最大 3 回(管理ページ由来の再起動は 2 回)で、5 回には届かない。上書きが原因で起動直後に落ちる経路は、印のファイルで次の起動から上書きを外す(R9)。残るリスク: 管理操作の再起動とクラッシュの再起動が 300 秒に合計 5 回重なると systemd が止まり、SSH の `systemctl reset-failed` が要る。
- **秘密情報**: トークンの値はどの応答にも入れない。`admin.toml` に秘密は入らない(userinfo の拒否)。ページは `sessionStorage` にトークンを置く(タブを閉じると消える。`localStorage` は使わない)。エラー文に外部サービスの応答本文をそのまま入れない。R19 の `t3-agent.meta.json` に `sessionId` を入れる場合も、`overview` には出さない。
- **外部への通信**: 管理ページ自体はキオスクから外へ通信しない。R18 の疎通確認だけは、保存と同じ規則を通した URL に GET を行い、`Authorization` を付けず、`max_redirects(0)`、`timeout_global(3s)`、応答の本文とヘッダーは読まず、journal に 1 行出す。ループバックの空きポートを状態コードと時間で探れることは、家で 1 人が使う前提で受け入れる。ブラウザの `fetch` は `connect-src 'self'` で listen 以外へ出せない。
- **サーバーの詰まり**: 管理のリクエストは管理ワーカー(同時 1 本)で処理し、本文の読み取りは 5 秒で諦める。パネルのスレッドは認証と受け渡しだけ。疎通確認はさらに別のスレッドで同時 1 本。
- **レート制限**: `PUT config` / `restart` / `probe` は管理用の `RateLimit`(1 秒に 5 回)。`overview` は制限なし。管理のリクエストは `last_request`(`panel=connected`)を更新しない。401 の回数制限は置かない(16 文字以上・定数時間比較で総当たりは現実的でない。journal に 1 行ずつ残る)。
- **平文 HTTP**: LAN のインターフェースの IP だけで待ち受ける前提はパネルと同じ。Tailscale や公開は対象外。
- **ブラウザ**: CSP は `default-src 'none'` に `'self'` だけを許し、`frame-ancestors 'none'; base-uri 'none'; form-action 'none'` と `X-Frame-Options: DENY`。応答の値は `textContent` で出す。`innerHTML` 系は使わない(テストで文字列検査)。URL をリンクにしない。トークンの入力欄は `type=password`。
- **個人情報**: 応答に LAN の IP とホスト名が含まれる。docs とテストの例は `192.0.2.x` と `example.test` だけを使う。管理ページのスクリーンショットは LP・README・docs に載せない。

## テスト計画

| AC | 確かめ方 |
|---|---|
| AC1 | 単体(`panel.rs`): `/admin` と `app.js` の応答ヘッダー(CSP・X-Frame-Options・nosniff・no-store・Content-Type)。実機: 別の端末のブラウザで開き、開発者ツールの Network で `listen` 以外への通信が無いこと |
| AC2 | 単体: 管理トークン無し・パネルのトークンで `/api/v1/admin/overview` → 401。管理トークンで `/api/v1/input` → 401。緩い権限の admin.token、パネルと同じ値の admin.token で `Panel::spawn` → 管理ページ無効の報告と、`/admin` が 404 |
| AC3 | 単体(`config.rs`): base に `aircraft.url = A`、overlay に `B` で `load_with_overlay` → effective B、base A。`items` を作る関数に `LoadedConfig` と今の overlay を渡して期待の JSON(`home_assistant.url` が `readonly`) |
| AC4 | 単体(`panel.rs`): base を `agents.enabled = true`・`url` 正しい・`ui.start_screen = "agents"` にし、PUT で `{"agents":{"enabled":false}}` → 400 と「ui.start_screen = agents には agents.enabled = true が必要」。`admin.toml` が変わらない |
| AC5 | 単体: 書き込みの手順を関数に分け、(a) tmp を書いた直後、(b) `.prev` を作った直後、(c) rename 後、の各状態をディレクトリに作って `load_with_overlay` を呼び、前の内容か新しい内容のどちらかになること。残った tmp が起動時に消えること |
| AC6 | 単体: `{"panel":{"listen":"..."}}`、`{"agents":{"token_file":"..."}}`、`{"geoframe":{"photo_interval_secs":1}}`、`{"home_assistant":{"url":"..."}}`、userinfo 付き URL、空白・`"`・制御文字を含む URL、`agents.enabled=false` で `agents.url` が不正、許可されないホスト → すべて 400。`admin.toml` が変わらない |
| AC7 | 単体(`config.rs`): `Overlay::check` に base(`agents.url = http://192.0.2.30:3773`、`allowed_hosts = ["t3.example.test"]`)を渡し、`http://192.0.2.30:4000` と `https://t3.example.test/` は Ok、`http://192.0.2.31:3773` は Err。`load_with_overlay` で許されないホストの overlay が無視され `report.error` に理由 |
| AC8 | 単体: base のパスに壊れた TOML を置いて PUT → 409。`admin.toml` が変わらない |
| AC9 | 単体: `{"aircraft":{"enabled":null}}` を PUT → `admin.toml` に `aircraft.enabled` が無く、`items` の `next` が base の値。全部 `null` で PUT → `admin.toml` が無く `.prev` がある |
| AC10 | 単体: `POST restart` → `p.inputs()` に `Restart`、2 回目は 429 と `requested: true`。`started` を 10 秒前にして 429 と `retry_after_s`。実機: ボタン → journal に「管理ページからの再起動」→ 3 秒後に起動。モニター消灯中に再起動して、再起動後の状態を確認 |
| AC11 | 単体(`panel.rs`): 4 通り(値を変える保存・同じ値・取り消しで同じ・壊れた overlay)で `pending_restart` |
| AC12 | 単体(`config.rs`): 壊れた TOML / 知らないキー / 検証に通らない、の 3 つの overlay で `load_with_overlay` が base を返し `report.error` に理由。stderr の文言はテストで組み立てた文字列と比べる |
| AC13 | 単体: 印のファイルの書き込み・削除・判定を関数に分け、印がある状態の `load_with_overlay(consume_marker = true)` が overlay を無視して理由(`error_kind: boot_failed`)を返し、印を消すこと。`consume_marker = false` では印が残ること。実機: 上書きで `cube.enabled = true` にして起動 → 初期化中(数秒以内)に `kill -9` → 次の起動のログに理由、ページの赤い帯、再起動ボタンが強調されない |
| AC14 | 単体(`main.rs` か `config.rs`): `check-config` の出力を組み立てる関数に `OverlayReport` を渡して文字列を比べる(効いている・無い・無視した、の 3 通り) |
| AC15 | 単体: `FeedReport` への変換を、対応表の各行(None・初回前・ok・failing・auth)で確かめる。実機(または `photos.rs` の模擬サーバー): GEOFRAME を止める・戻す。`room.enabled = false` で `disaster` が `ok`。T3 のトークンを外して `auth_required`。各取得元の取得間隔の 2 倍まで待つ |
| AC16 | 単体: 一時ファイルに 0644 / 0600 / 無し / 読めない(0000)で `check_token_file`。所有者違いは `uid` 引数を偽の値にして判定 |
| AC17 | 単体(`main.rs`): `config-get` の本体関数に base と overlay のパスを渡して、上書きあり・なし・壊れた上書き・許されないホスト・知らないキー。スクリプト: `source systemd/miharashi-t3-token` して `resolve_url` 関数を呼び、`miharashi` をスタブに差し替えて期待の出力、`t3` と `curl` のスタブが呼ばれないこと |
| AC18 | スクリプトの関数テスト: `config-get` のスタブが「次回起動の URL」を返し、`curl` のスタブが失敗したときに `die` し、token_file が変わらない |
| AC19 | 実機: ページの表示。タブを隠すと `overview` の取得が止まる(開発者ツール) |
| AC20 | 単体: `overview` の `generation` を取り、`admin.toml` を直接書き換えてから古い `generation` で PUT → 409。正しい `generation` → 204。実機: 入力中に 5 秒更新が走っても入力が残る |
| AC21 | 単体: `Content-Length: 1000` を宣言して 10 バイトだけ送った接続を開いたまま、別の接続で `POST /api/v1/input` → 1 秒以内に 204。5 秒後に遅い接続が 408 か切断 |
| AC22 | 単体: `Origin: http://evil.example.test` 付きで `Authorization` 無しの PUT と POST → 401、応答に `Access-Control-Allow-*` が無い。`OPTIONS /api/v1/admin/config` → 405。`GET /api/v1/admin/overview?token=<正しい値>` → 401。`include_str!` した `app.js` に `innerHTML`・`insertAdjacentHTML`・`document.write` が無い |
| AC23 | 単体: 応答を保留する模擬サーバー(`TcpListener` で受けて返さない)に probe → 3 秒 ± 0.5 秒で `ok: false`。probe 中の `POST /api/v1/input` が 1 秒以内。同時 2 本目は 429。模擬サーバーが受けたリクエストに `Authorization` が無い。302 を返す模擬サーバーで `status: 302`(追わない) |
| AC24 | 単体: `t3-agent.meta.json` がある・無い・壊れている、で `tokens[].expires_at` と `scopes` |
| AC25 | 実機: SSH で `touch` して `config_file.modified_at` が変わる |
| AC26 | `cargo test` 全体。`/api/v1/status` の既存テストが変わらない |
| AC27 | レビューで docs とテストの文字列を見る(`192.168.`、実際のホスト名が無いこと) |

## リスクと対策

| リスク | 対策 |
|---|---|
| 管理トークンが盗まれ、HA や T3 のトークンの送り先を変えられる | HA の URL は対象外。T3 の URL は許可ホストの範囲。`check_url` で平文のホスト名を拒否。管理トークンの重みを `example.toml` と USAGE に書く |
| 設定の出どころが 2 つになり、SSH で直した値が効かず戸惑う | ページに「効いている / ファイル / 上書き / 次回起動」を並べる。`check-config` と起動ログに上書きの件数と値。`example.toml` と USAGE に説明。`admin.toml` の先頭にコメント。外のスクリプトは `config-get` で同じ規則 |
| 保存と再起動の間に SSH で設定ファイルが変わり、再起動時に合わせた設定が検証に通らない | 起動時の安全弁で base だけで起動し、ページに理由が出る。保存時は**その時点の**設定ファイルで検証する(起動時のコピーは使わない) |
| 上書きが検証には通るが起動の途中で落ち、3 秒ごとの再起動で systemd が止まる | 印のファイル(R9)で次の起動から上書きを外す。それでも止まったら SSH で `systemctl reset-failed`(USAGE に既にある) |
| `miharashi-t3-token` が古い URL や無視された上書きの URL で確認する | `config-get` で次回起動の値を取る。届かなければトークンを置き換えない(今と同じ)。発行先は t3 CLI のペアリングで決まり、URL では変わらない |
| 不正な上書きで起動できなくなる | 起動時の安全弁(R8)。保存前の検証(R4・R5) |
| 再起動の連打で systemd が止まる | 120 秒の抑止と受け付け済みフラグ(起動は 300 秒に最大 3 回)。残るリスク: 管理操作の再起動とクラッシュの再起動が 300 秒に合計 5 回重なると systemd が止まる |
| 遅いクライアントや fsync で Echo Show 5 のキーが待たされる | 管理ワーカーを分け、本文の読み取りに 5 秒の期限。疎通確認はさらに別スレッド |
| 別のタブや SSH と保存が競合し、上書きが消える | 世代付きの PUT(409)。フォームは全体を送る |
| 5 秒ごとの更新で入力が消える | フォームは更新で触らない(dirty の管理) |
| ブラウザにトークンが残る・XSS で盗まれる | `sessionStorage`。ログアウトボタンで消す。CSP に `'unsafe-inline'` を入れない。`textContent` だけで描く(テストで文字列検査) |
| `geoframe.url` を変えられると、GEOFRAME が返す写真の URL をキオスクが何でも取りに行く(任意の画像の表示、ループバックへの GET、遅いサーバーで写真を止める) | 今回はリスク表に書くだけ。写真の URL を `geoframe.url` と同じ origin に限り、時間制限付きの agent で取る対応を別の Issue にする(PRD「決まっていないこと」) |
| `aircraft.url` / `geoframe.url` に検査を足して既存の設定が通らなくなる | 検査は「http(s) で始まり host が解釈できる」だけ(userinfo と文字の検査は上書きにだけ)。既定値と `example.toml` のテストで確認 |
| 本番反映のとき、他のセッションが入れ替えた直後でないか | 既存の決まり(最新 main で全画面の回帰確認)に従う |

## 実装の順番

タスクに分けて別々に実装させるため、最初に共有する型(契約)を固める。

0. **契約(config.rs、panel.rs、screen.rs。最初のコミット)**: `Admin` セクション(`allowed_hosts` を含む)、`Overlay` と内側の構造体(`deny_unknown_fields`)、`OverlayReport`、`LoadedConfig`、`Config::value`、`AdminSources`・`FeedReport`・`TokenState` の型、`App::admin_sources` の既定実装、`PanelInput::Restart` の列挙子。中身は最小で、`cargo build` と既存テストが通ること。
1. **設定の上書き(config.rs、main.rs)**: `Overlay::check`(共通の文字の検査、許可ホスト、画面が無効でも検査)、`load_with_overlay`(印の判定を含む)、起動ログと `check-config` の出力、`config-get` サブコマンド、`geoframe.url` / `aircraft.url` の最小の検査、`example.toml` に `[admin]`。テスト: AC3(読み込み側)・AC6(検査)・AC7・AC12・AC13(単体)・AC14・AC17(Rust 側)。
2. **管理トークンと読み取り API(panel.rs、runtime.rs)**: `admin.token_file` の読み込み(厳しい規則、パネルと同値の拒否)、完全一致の振り分け表、管理ワーカースレッドと受け渡し、`/api/v1/admin/overview` の `host` / `version` / `uptime_s` / `screen` / `config_file` / `overlay`(`generation` を含む)/ `allowed_hosts` / `items`(`next` と `pending_restart` を含む)、`restart` と 120 秒の抑止と受け付け済みフラグ、実行ループの終了、印の書き込みと削除、JSON への `nosniff`、`OPTIONS` の 405。テスト: AC2・AC3(API 側)・AC10(単体)・AC11・AC21・AC22(HTTP 側)。
3. **保存(panel.rs)**: `PUT /api/v1/admin/config`、世代の比較、設定ファイルの読み直し、`Overlay::check` と検証、原子的な書き込み、`.prev`、tmp の掃除。テスト: AC4・AC5・AC6(API 側)・AC8・AC9・AC20。
4. **取得元とトークンの状態(app.rs、photos.rs、aircraft.rs、panel.rs)**: `KioskApp::admin_sources`、`FeedReport` への変換、GEOFRAME の `last_ok`、航空機の `last_error`、`check_token_file`、エラー文の切り詰め。テスト: AC15(単体)・AC16。
5. **ページ(admin/ の 3 ファイル、panel.rs の静的配信)**: ログイン・稼働・項目(dirty と世代、readonly、使えるホスト)・保存・再起動・取得元・トークン。CSP ほかのヘッダー。Page Visibility。テスト: AC1(ヘッダー)・AC22(`app.js` の文字列検査)。実機で AC1・AC19・AC20。**3 と 4 の後に完成**(静的ファイルの配信とヘッダーだけなら 2 の後に先行できる)。
6. **スクリプトと文書**: `miharashi-t3-token` の関数と `main` の分離、`config-get` の利用(AC17 のスクリプト側・AC18)、`install.sh` に admin.token の案内、`docs/USAGE.md` の「管理ページ」節(管理トークンの重み、`allowed_hosts`、印の意味)、`docs/panel/DESIGN.md` §5.2 の表、`docs/README.md` の索引、`config/example.toml` の最終形。
7. **実機の確認**: 本番に入れて AC10・AC11・AC13・AC15・AC19・AC20・AC25・AC26 を確かめ、全画面の回帰確認。
8. **あると良い(時間があれば同じ PR、無ければ別 Issue)**: R18 の疎通確認(AC23)、R19 の T3 トークンの期限(`miharashi-t3-token` が `scopes` と `expiresAt` を `t3-agent.meta.json` に書き、`tokens[].expires_at` に出す。AC24)、R20 の設定ファイルの最終更新時刻(`config_file.modified_at` は 2 で入れる。AC25)。

依存: 0 → 1 → 2 → 3。4 は 0 と 2 の後。5 は 3 と 4 の後(静的配信だけ 2 の後)。6 は 1 の後ならいつでも。`panel.rs` を同時に触るタスクは重ねない(2 → 3 → 4 の順に 1 本ずつ)。
