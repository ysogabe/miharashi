//! だいだい画面(Muse からの通知)のデータ取得。PoC の取得元は本体ホスト上のローカル JSON ファイル。
//!
//! 取得元は `FeedSource` の後ろに隠してあり、`[daidai] source` で選ぶ(今は `file` だけ)。
//! 将来 HTTP フィードにするときは、`FeedSource` の実装と `source` の値を足すだけで、読み取った文字列を
//! `parse_feed` に渡す以降(カードの検証・期限・画面)は変わらない。
//!
//! 読み取りに失敗した・ファイルが無い・JSON が不正なときは「カード無し」として扱い、例外では落とさない。
//! 期限(`expires_at`)の判定は画面が描くたびに壁時計で行う(ファイルを読み直さなくても期限が切れる)。
use crate::config::ConfigDaidai;
use crate::screen::{Provider, World};
use chrono::{DateTime, FixedOffset, TimeZone};
use miharashi_render::Pixmap;
use miharashi_render::daidai::{self, CardKind, Pose};
use serde_json::Value;
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// フィードファイルの大きさの上限。これを超えるものは不正として「カード無し」にする。
pub const MAX_FEED_BYTES: u64 = 256 * 1024;
/// 取り込むカードの上限(画面に並べるのは `daidai::MAX_CARDS` まで)。
const MAX_CARDS_KEPT: usize = 16;
const MAX_TITLE_CHARS: usize = 120;
const MAX_BODY_CHARS: usize = 400;
/// 素材 PNG の大きさの上限(読み込む前にファイルの大きさで確かめる)。
const MAX_AVATAR_BYTES: u64 = 8 * 1024 * 1024;

/// 通知カード 1 枚(検証済み)。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Card {
    pub id: String,
    pub kind: CardKind,
    pub title: String,
    pub body: String,
    /// None は期限なし
    pub expires_at: Option<DateTime<FixedOffset>>,
}

/// 画面が読む最新のフィード。`World::daidai` に写される。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DaidaiSnapshot {
    pub pose: Pose,
    /// 期限の確認前のカード(フィードに書いてあった順)
    pub cards: Vec<Card>,
}

impl DaidaiSnapshot {
    /// `now` の時点で期限内のカード(期限ちょうどは切れたものとして扱う)。
    pub fn active<Tz: TimeZone>(&self, now: &DateTime<Tz>) -> Vec<&Card> {
        let now = now.fixed_offset();
        self.cards
            .iter()
            .filter(|c| c.expires_at.is_none_or(|e| e > now))
            .collect()
    }
}

fn clean(s: &str, max_chars: usize) -> String {
    let flat: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    flat.trim().chars().take(max_chars).collect()
}

/// カード 1 枚を検証する。title が無い・期限の形が不正なカードは捨てる(ほかのカードは残す)。
fn parse_card(v: &Value, index: usize) -> Option<Card> {
    let o = v.as_object()?;
    let title = clean(o.get("title")?.as_str()?, MAX_TITLE_CHARS);
    if title.is_empty() {
        return None;
    }
    let expires_at = match o.get("expires_at") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(DateTime::parse_from_rfc3339(s).ok()?),
        Some(_) => return None,
    };
    Some(Card {
        id: o
            .get("id")
            .and_then(Value::as_str)
            .map_or_else(|| format!("card-{index}"), |s| clean(s, 64)),
        kind: CardKind::parse(o.get("kind").and_then(Value::as_str).unwrap_or("")),
        title,
        body: clean(
            o.get("body").and_then(Value::as_str).unwrap_or(""),
            MAX_BODY_CHARS,
        ),
        expires_at,
    })
}

/// フィードの JSON 文字列を読む。壊れていれば `DaidaiSnapshot::default()`(カード無し・idle)。
pub fn parse_feed(text: &str) -> DaidaiSnapshot {
    let Ok(Value::Object(root)) = serde_json::from_str::<Value>(text) else {
        return DaidaiSnapshot::default();
    };
    // version は 1 だけを知っている。無ければ 1 として読み、知らない版は読まない
    if root.get("version").is_some_and(|v| v.as_u64() != Some(1)) {
        return DaidaiSnapshot::default();
    }
    let pose = Pose::parse(
        root.get("avatar_pose")
            .and_then(Value::as_str)
            .unwrap_or(""),
    );
    let cards = root
        .get("cards")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .enumerate()
                .filter_map(|(i, v)| parse_card(v, i))
                .take(MAX_CARDS_KEPT)
                .collect()
        })
        .unwrap_or_default();
    DaidaiSnapshot { pose, cards }
}

/// フィードの取得元。`fetch` は JSON 全文を返す(失敗は理由の文字列)。
pub trait FeedSource: Send {
    fn fetch(&mut self) -> Result<String, String>;
}

/// ローカルファイル。
pub struct FileSource {
    path: PathBuf,
}

impl FileSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl FeedSource for FileSource {
    fn fetch(&mut self) -> Result<String, String> {
        let f =
            std::fs::File::open(&self.path).map_err(|e| format!("{}: {e}", self.path.display()))?;
        let mut text = String::new();
        // 上限 + 1 バイトまで読み、超えたら不正。無限に読み続けるファイル(デバイスなど)でも止まる
        f.take(MAX_FEED_BYTES + 1)
            .read_to_string(&mut text)
            .map_err(|e| format!("{}: {e}", self.path.display()))?;
        if text.len() as u64 > MAX_FEED_BYTES {
            return Err(format!(
                "{}: {MAX_FEED_BYTES} バイトを超えている",
                self.path.display()
            ));
        }
        Ok(text)
    }
}

/// 1 回取得して読む。取得に失敗したときは (カード無し, 理由)。
pub fn poll_once(source: &mut dyn FeedSource) -> (DaidaiSnapshot, Option<String>) {
    match source.fetch() {
        Ok(text) => (parse_feed(&text), None),
        Err(why) => (DaidaiSnapshot::default(), Some(why)),
    }
}

pub type Shared = Arc<Mutex<DaidaiSnapshot>>;

/// 設定の `source` から取得元を作る。
pub fn source_from_config(cfg: &ConfigDaidai) -> Result<Box<dyn FeedSource>, String> {
    match cfg.source.as_str() {
        "file" => Ok(Box::new(FileSource::new(&cfg.feed_path))),
        other => Err(format!("daidai.source {other:?} は未対応(file だけ)")),
    }
}

/// 取得を始める。無効なら None。最初の値が入るまでは「カード無し」。
pub fn start(cfg: &ConfigDaidai) -> Option<Shared> {
    if !cfg.enabled {
        return None;
    }
    let mut source = match source_from_config(cfg) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("daidai: {e}");
            return None;
        }
    };
    let shared: Shared = Arc::new(Mutex::new(DaidaiSnapshot::default()));
    let out = shared.clone();
    let poll = Duration::from_secs(cfg.poll_secs.max(1));
    let spawned = std::thread::Builder::new()
        .name("daidai".into())
        .spawn(move || {
            let mut last_error: Option<String> = None;
            loop {
                let (snap, error) = poll_once(source.as_mut());
                // 理由が変わったときだけ書く(ファイルがまだ無い間、毎周期は書かない)
                if error != last_error {
                    match &error {
                        Some(why) => {
                            eprintln!("daidai: フィードを読めない(カード無しで表示): {why}")
                        }
                        None => eprintln!("daidai: フィードを読めた"),
                    }
                    last_error = error;
                }
                if let Ok(mut g) = out.lock() {
                    *g = snap;
                }
                std::thread::sleep(poll);
            }
        });
    if let Err(e) = spawned {
        eprintln!("daidai: スレッドを起動できない: {e}");
        return None;
    }
    Some(shared)
}

/// 取得した最新値を毎周期 `World::daidai` に写す。ブロックしない。
pub struct DaidaiProvider {
    shared: Shared,
}

impl DaidaiProvider {
    pub fn new(shared: Shared) -> Self {
        Self { shared }
    }
}

impl Provider for DaidaiProvider {
    fn update(&mut self, world: &mut World) {
        // 書き込み中なら前回の値のまま
        if let Ok(g) = self.shared.try_lock()
            && *g != world.daidai
        {
            world.daidai = g.clone();
        }
    }
}

/// ポーズごとのアバター画像。
pub struct AvatarSet {
    poses: [(Pose, Pixmap); 4],
}

impl AvatarSet {
    /// `dir` に `daidai-<pose>.png` があればそれを、無い・読めないポーズはリポジトリ内のプレースホルダーを使う。
    /// `dir` が空なら全部プレースホルダー。素材の置き換えは同じファイル名で行う。
    pub fn load(dir: &str) -> Self {
        let one = |pose: Pose| -> Pixmap {
            if !dir.is_empty()
                && let Some(p) = load_file(dir, pose)
            {
                return p;
            }
            daidai::decode_avatar(daidai::embedded_avatar_png(pose)).unwrap_or_else(|e| {
                eprintln!("daidai: 埋め込み素材 {} を読めない: {e}", pose.name());
                Pixmap::new(daidai::AVATAR_SIZE, daidai::AVATAR_SIZE).expect("固定の大きさ")
            })
        };
        Self {
            poses: Pose::ALL.map(|p| (p, one(p))),
        }
    }

    pub fn get(&self, pose: Pose) -> &Pixmap {
        &self
            .poses
            .iter()
            .find(|(p, _)| *p == pose)
            .expect("全ポーズを持つ")
            .1
    }
}

fn load_file(dir: &str, pose: Pose) -> Option<Pixmap> {
    let path = std::path::Path::new(dir).join(format!("daidai-{}.png", pose.name()));
    let result = std::fs::metadata(&path)
        .map_err(|e| e.to_string())
        .and_then(|m| {
            if m.len() > MAX_AVATAR_BYTES {
                Err(format!("{} バイトを超えている", MAX_AVATAR_BYTES))
            } else {
                std::fs::read(&path).map_err(|e| e.to_string())
            }
        })
        .and_then(|bytes| daidai::decode_avatar(&bytes));
    match result {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!(
                "daidai: 素材 {} を使えない(プレースホルダーを使う): {e}",
                path.display()
            );
            None
        }
    }
}

/// 書式の見本(docs/daidai/feed.sample.json)。`png` の fixture が本物の読み取りの経路で使う。
pub const SAMPLE_FEED: &str = include_str!("../../../docs/daidai/feed.sample.json");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sample_file_in_docs_parses() {
        let s = parse_feed(SAMPLE_FEED);
        assert_eq!(s.cards.len(), 2);
        assert_eq!(s.pose, Pose::Idle);
        assert_eq!(s, parse_feed(SAMPLE), "docs の見本とテストの見本は同じ内容");
    }

    const SAMPLE: &str = r#"{"avatar_pose":"idle","cards":[{"body":"これはPoCのサンプル表示です。実際の朝の通知がここに出ます。","expires_at":"2026-11-08T12:00:00+09:00","id":"sample-morning-1","kind":"morning","title":"おはようございます"},{"body":"確認してほしいことがあるときに、だいだいからここで知らせます（サンプル）。","expires_at":null,"id":"sample-request-1","kind":"request","title":"お願い"}],"updated_at":"2026-11-08T07:00:00+09:00","version":1}"#;

    fn at(s: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(s).unwrap()
    }

    #[test]
    fn parses_the_sample_feed() {
        let s = parse_feed(SAMPLE);
        assert_eq!(s.pose, Pose::Idle);
        assert_eq!(s.cards.len(), 2);
        assert_eq!(s.cards[0].kind, CardKind::Morning);
        assert_eq!(s.cards[0].title, "おはようございます");
        assert_eq!(s.cards[1].kind, CardKind::Request);
        assert_eq!(s.cards[1].expires_at, None);
    }

    #[test]
    fn expired_cards_are_not_shown_but_open_ended_ones_are() {
        let s = parse_feed(SAMPLE);
        let ids =
            |t: &str| -> Vec<String> { s.active(&at(t)).iter().map(|c| c.id.clone()).collect() };
        assert_eq!(
            ids("2026-11-08T07:30:00+09:00"),
            ["sample-morning-1", "sample-request-1"]
        );
        // 期限ちょうどは切れたもの。別のタイムゾーンの表記でも同じ瞬間なら同じ結果
        assert_eq!(ids("2026-11-08T12:00:00+09:00"), ["sample-request-1"]);
        assert_eq!(ids("2026-11-08T03:00:00Z"), ["sample-request-1"]);
        assert_eq!(
            ids("2026-11-08T02:59:59Z"),
            ["sample-morning-1", "sample-request-1"]
        );
        assert_eq!(ids("2027-01-01T00:00:00+09:00"), ["sample-request-1"]);
    }

    #[test]
    fn broken_input_means_no_cards() {
        for text in [
            "",
            "   ",
            "not json",
            "{",
            "[]",
            "null",
            "42",
            "\"cards\"",
            "{}",
            r#"{"version":1}"#,
            r#"{"version":1,"cards":null}"#,
            r#"{"version":1,"cards":"x"}"#,
            r#"{"version":2,"cards":[{"title":"x"}]}"#,
            r#"{"version":"1","cards":[{"title":"x"}]}"#,
            "\u{0}\u{1}\u{2}",
        ] {
            let s = parse_feed(text);
            assert!(s.cards.is_empty(), "{text:?}");
            assert_eq!(s.pose, Pose::Idle, "{text:?}");
        }
    }

    #[test]
    fn a_bad_card_is_dropped_without_losing_the_others() {
        let s = parse_feed(
            r#"{"version":1,"cards":[
                {"id":"ok1","title":"A","expires_at":null},
                {"id":"no-title","body":"x"},
                {"id":"empty-title","title":"   "},
                {"id":"bad-date","title":"B","expires_at":"yesterday"},
                {"id":"num-date","title":"C","expires_at":123},
                "string",
                null,
                {"id":"ok2","title":"D","kind":"unknown-kind"}
            ]}"#,
        );
        let ids: Vec<_> = s.cards.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["ok1", "ok2"]);
        assert_eq!(s.cards[1].kind, CardKind::Info, "知らない kind は info");
    }

    #[test]
    fn unknown_pose_is_idle_and_known_poses_are_read() {
        for (v, want) in [
            ("idle", Pose::Idle),
            ("working", Pose::Working),
            ("milestone", Pose::Milestone),
            ("making", Pose::Making),
            ("sleeping", Pose::Idle),
        ] {
            let s = parse_feed(&format!(
                r#"{{"version":1,"avatar_pose":"{v}","cards":[]}}"#
            ));
            assert_eq!(s.pose, want, "{v}");
        }
        assert_eq!(
            parse_feed(r#"{"version":1,"avatar_pose":3}"#).pose,
            Pose::Idle
        );
        assert_eq!(parse_feed(r#"{"version":1}"#).pose, Pose::Idle);
    }

    #[test]
    fn text_is_flattened_and_bounded() {
        let s = parse_feed(&format!(
            r#"{{"version":1,"cards":[{{"title":"a\nb\u0007c","body":"{}"}}]}}"#,
            "あ".repeat(5000)
        ));
        assert_eq!(s.cards[0].title, "a b c");
        assert_eq!(s.cards[0].body.chars().count(), MAX_BODY_CHARS);
        let many: String = (0..100)
            .map(|i| format!(r#"{{"id":"c{i}","title":"t"}}"#))
            .collect::<Vec<_>>()
            .join(",");
        let s = parse_feed(&format!(r#"{{"version":1,"cards":[{many}]}}"#));
        assert_eq!(s.cards.len(), MAX_CARDS_KEPT);
    }

    #[test]
    fn file_source_handles_missing_empty_invalid_and_oversized_files() {
        let dir = std::env::temp_dir().join(format!("miharashi-daidai-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("feed.json");
        let _ = std::fs::remove_file(&path);
        let mut src = FileSource::new(&path);

        // ファイル無し: カード無しと理由
        let (s, why) = poll_once(&mut src);
        assert!(s.cards.is_empty() && why.is_some());
        // 空 / 不正: カード無し(読み取り自体は成功)
        for body in ["", "{ broken", "[1,2,3]"] {
            std::fs::write(&path, body).unwrap();
            let (s, why) = poll_once(&mut src);
            assert!(s.cards.is_empty() && why.is_none(), "{body:?}");
        }
        // 正常
        std::fs::write(&path, SAMPLE).unwrap();
        assert_eq!(poll_once(&mut src).0.cards.len(), 2);
        // 大きすぎる: カード無しと理由
        std::fs::write(&path, " ".repeat(MAX_FEED_BYTES as usize + 1)).unwrap();
        let (s, why) = poll_once(&mut src);
        assert!(s.cards.is_empty() && why.is_some());
        // UTF-8 でない中身
        std::fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
        let (s, why) = poll_once(&mut src);
        assert!(s.cards.is_empty() && why.is_some());
        // ディレクトリを指している
        let (s, why) = poll_once(&mut FileSource::new(&dir));
        assert!(s.cards.is_empty() && why.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn provider_copies_the_latest_value_and_a_failed_feed_clears_it() {
        let shared: Shared = Arc::new(Mutex::new(parse_feed(SAMPLE)));
        let mut p = DaidaiProvider::new(shared.clone());
        let mut world = World::default();
        p.update(&mut world);
        assert_eq!(world.daidai.cards.len(), 2);
        *shared.lock().unwrap() = DaidaiSnapshot::default();
        p.update(&mut world);
        assert!(world.daidai.cards.is_empty());
    }

    #[test]
    fn unknown_source_is_refused_and_disabled_does_not_start() {
        let cfg = ConfigDaidai {
            source: "http".into(),
            ..ConfigDaidai::default()
        };
        assert!(source_from_config(&cfg).is_err());
        assert!(start(&ConfigDaidai::default()).is_none(), "既定は無効");
    }

    #[test]
    fn avatar_set_falls_back_to_the_placeholders() {
        let set = AvatarSet::load("");
        for p in Pose::ALL {
            assert_eq!(set.get(p).width(), daidai::AVATAR_SIZE);
        }
        // 存在しない置き場・壊れた PNG でも落ちずにプレースホルダー
        let dir = std::env::temp_dir().join(format!("miharashi-daidai-av-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("daidai-idle.png"), b"not a png").unwrap();
        let set = AvatarSet::load(dir.to_str().unwrap());
        assert_eq!(set.get(Pose::Idle).width(), daidai::AVATAR_SIZE);
        let set = AvatarSet::load("/nonexistent/miharashi-daidai");
        assert_eq!(set.get(Pose::Making).width(), daidai::AVATAR_SIZE);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_replacement_asset_in_the_directory_is_used() {
        let dir = std::env::temp_dir().join(format!("miharashi-daidai-rep-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut px = Pixmap::new(64, 64).unwrap();
        px.fill(miharashi_render::canvas::rgba(255, 0, 0, 255));
        px.save_png(dir.join("daidai-working.png")).unwrap();
        let set = AvatarSet::load(dir.to_str().unwrap());
        assert_eq!(set.get(Pose::Working).width(), 64, "差し替えた素材");
        assert_eq!(
            set.get(Pose::Idle).width(),
            daidai::AVATAR_SIZE,
            "無いポーズは既定"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
