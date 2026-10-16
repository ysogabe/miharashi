//! 自宅の位置と市区町村。位置は Home Assistant の設定(`/api/config`)から、市区町村コードは国土地理院の
//! 逆ジオコーダーで引く(地震の地方・天気の予報の地域・自宅の市町村の警報のもとになる)。
//!
//! 一度引けた市区町村コードは位置と組で覚え(quake・weather・disaster のスレッドで共有)、systemd の
//! StateDirectory(`/var/lib/miharashi/home.json`)にも残す。逆ジオコーダーが止まっていても、位置が同じなら
//! 前回の値で始められる。位置が変わっていたら使わない(引けるまで失敗のまま)。
//! ファイルには自宅の位置が入るので、所有者だけが読めるようにする(0600)。ログにも位置とコードは出さない。
use crate::config::HomeAssistant;
use crate::quake::get_json;
use serde_json::{Value, json};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const GSI_RGEO: &str = "https://mreversegeocoder.gsi.go.jp/reverse-geocoder/LonLatToAddress";
/// 同じ位置とみなす差(度。約 1m)
const SAME_PLACE_DEG: f64 = 1e-5;

/// 覚えている市区町村コード(引いたときの位置と組)。
#[derive(Debug, Clone, PartialEq)]
struct Known {
    at: (f64, f64),
    muni: String,
}

impl Known {
    fn parse(text: &str) -> Option<Known> {
        let v: Value = serde_json::from_str(text).ok()?;
        let muni = v.get("muni")?.as_str()?;
        if muni.len() != 5 || !muni.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        Some(Known {
            at: (v.get("lat")?.as_f64()?, v.get("lon")?.as_f64()?),
            muni: muni.to_string(),
        })
    }

    fn to_json(&self) -> String {
        json!({"lat": self.at.0, "lon": self.at.1, "muni": self.muni}).to_string()
    }
}

fn same_place(a: (f64, f64), b: (f64, f64)) -> bool {
    (a.0 - b.0).abs() < SAME_PLACE_DEG && (a.1 - b.1).abs() < SAME_PLACE_DEG
}

/// 逆ジオコーダーの結果と覚えている値から、使う市区町村コードを決める。
/// 前回の値を使うときは、逆ジオコーダーの失敗の理由も返す(ログに出す)。
fn choose(
    here: (f64, f64),
    looked_up: Result<String, String>,
    known: Option<&Known>,
) -> Result<(String, Option<String>), String> {
    match (looked_up, known) {
        (Ok(muni), _) => Ok((muni, None)),
        (Err(e), Some(k)) if same_place(k.at, here) => Ok((k.muni.clone(), Some(e))),
        (Err(e), _) => Err(e),
    }
}

fn state_path() -> Option<PathBuf> {
    std::env::var_os("STATE_DIRECTORY").map(|d| PathBuf::from(d).join("home.json"))
}

/// 一時ファイル(0600)に書いてから置き換える。
fn save(path: &Path, k: &Known) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    // 前に残った一時ファイルを開いたときも 0600 にする
    f.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    f.write_all(k.to_json().as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

/// プロセスで 1 つの覚え。最初に使うときにファイルから読む。
fn known() -> &'static Mutex<Option<Known>> {
    static KNOWN: OnceLock<Mutex<Option<Known>>> = OnceLock::new();
    KNOWN.get_or_init(|| {
        let k = state_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| Known::parse(&t));
        Mutex::new(k)
    })
}

/// Home Assistant に登録された自宅の (緯度, 経度)。
pub fn home_location(agent: &ureq::Agent, ha: &HomeAssistant) -> Result<(f64, f64), String> {
    if ha.url.is_empty() {
        return Err("home_assistant.url が未設定".into());
    }
    let token = crate::screen_power::read_token(&ha.token_file)?;
    let base = ha.url.trim_end_matches('/');
    let conf = get_json(
        agent,
        &format!("{base}/api/config"),
        Some(&format!("Bearer {token}")),
    )?;
    match (
        conf.get("latitude").and_then(Value::as_f64),
        conf.get("longitude").and_then(Value::as_f64),
    ) {
        (Some(a), Some(b)) => Ok((a, b)),
        _ => Err("HA の設定に自宅の緯度経度がない".into()),
    }
}

/// 国土地理院の逆ジオコーダーで、位置の市区町村コード(5 桁)を引く。
fn lookup_muni(agent: &ureq::Agent, (lat, lon): (f64, f64)) -> Result<String, String> {
    let rg = get_json(agent, &format!("{GSI_RGEO}?lat={lat}&lon={lon}"), None)?;
    let muni = rg
        .get("results")
        .and_then(|r| r.get("muniCd"))
        .and_then(Value::as_str)
        .unwrap_or("");
    // 先頭の 0 が落ちた 4 桁でも返ってくる
    match muni.len() {
        5 if muni.bytes().all(|b| b.is_ascii_digit()) => Ok(muni.to_string()),
        4 if muni.bytes().all(|b| b.is_ascii_digit()) => Ok(format!("0{muni}")),
        _ => Err(format!("市区町村コードを得られない({muni:?})")),
    }
}

/// 自宅の位置の市区町村コード(5 桁。先頭 2 桁が都道府県)。逆ジオコーダーに届かなければ、同じ位置で前回引けた値。
pub fn home_muni_code(agent: &ureq::Agent, ha: &HomeAssistant) -> Result<String, String> {
    let here = home_location(agent, ha)?;
    let looked_up = lookup_muni(agent, here);
    let Ok(mut known) = known().lock() else {
        return looked_up;
    };
    let (muni, stale) = choose(here, looked_up, known.as_ref())?;
    match stale {
        Some(e) => eprintln!("home: 国土地理院に届かないため、前回の市区町村コードを使う: {e}"),
        None => {
            let k = Known {
                at: here,
                muni: muni.clone(),
            };
            if known.as_ref() != Some(&k) {
                if let Some(p) = state_path()
                    && let Err(e) = save(&p, &k)
                {
                    eprintln!("home: 市区町村コードを保存できない: {}: {e}", p.display());
                }
                *known = Some(k);
            }
        }
    }
    Ok(muni)
}

#[cfg(test)]
mod tests {
    use super::*;
    use miharashi_render::sample::HOME;

    fn chiyoda() -> Known {
        Known {
            at: HOME,
            muni: "13101".into(),
        }
    }

    /// 逆ジオコーダーが失敗しても、同じ位置で前回引けた値があれば使う。位置が変わっていたら使わない。
    #[test]
    fn falls_back_to_the_last_code_at_the_same_place() {
        let k = chiyoda();
        assert_eq!(
            choose(HOME, Ok("13102".into()), Some(&k)),
            Ok(("13102".into(), None)),
            "引けたら新しい値"
        );
        assert_eq!(
            choose(HOME, Err("timeout: global".into()), Some(&k)),
            Ok(("13101".into(), Some("timeout: global".into())))
        );
        let near = (HOME.0 + 0.000_001, HOME.1 - 0.000_001);
        assert!(
            choose(near, Err("x".into()), Some(&k)).is_ok(),
            "1m 未満の差は同じ位置"
        );
        let moved = (HOME.0 + 0.01, HOME.1);
        assert_eq!(
            choose(moved, Err("timeout: global".into()), Some(&k)),
            Err("timeout: global".into())
        );
        assert_eq!(
            choose(HOME, Err("timeout: global".into()), None),
            Err("timeout: global".into())
        );
    }

    #[test]
    fn known_round_trips_and_rejects_broken_files() {
        let k = chiyoda();
        assert_eq!(Known::parse(&k.to_json()), Some(k));
        for bad in [
            "",
            "{}",
            r#"{"lat": 35.0, "lon": 139.0}"#,
            r#"{"lat": 35.0, "lon": 139.0, "muni": "1310"}"#,
            r#"{"lat": "x", "lon": 139.0, "muni": "13101"}"#,
        ] {
            assert_eq!(Known::parse(bad), None, "{bad}");
        }
    }

    /// 保存したファイルは所有者だけが読める。読み直すと同じ値。
    #[test]
    fn saved_file_is_private() {
        let dir = std::env::temp_dir().join(format!("miharashi-home-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("home.json");
        save(&path, &chiyoda()).unwrap();
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(&path).unwrap().permissions(),
        );
        assert_eq!(mode & 0o777, 0o600);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(Known::parse(&text), Some(chiyoda()));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
