//! 実モニター表示(`vrm-view`)の演出。時間 [秒] から表情の weight とカメラの角度を決めるだけで、GL には触れない。
use crate::vrm::Vrm;
use std::f32::consts::PI;

/// まばたきの間隔 [秒]
pub const BLINK_PERIOD: f32 = 4.0;
/// 1 回のまばたき(閉じて開くまで)の長さ [秒]
pub const BLINK_LEN: f32 = 0.3;
/// 表情をゆっくり 0 → 1 → 0 と動かす周期 [秒](blink 以外)
pub const SWELL_PERIOD: f32 = 4.0;
/// カメラを左右に振る周期 [秒]
pub const YAW_PERIOD: f32 = 16.0;
/// カメラを左右に振る振れ幅 [度]
pub const YAW_DEG: f32 = 20.0;

/// `name` の表情の weight(0..=1)。blink は数秒おきに一瞬だけ閉じ、それ以外はゆっくり出入りする。
pub fn expression_weight(name: &str, t: f32) -> f32 {
    if name == "blink" {
        let p = t.rem_euclid(BLINK_PERIOD);
        // 周期の 1 秒目から BLINK_LEN のあいだだけ 0 → 1 → 0(最初のフレームは開いた目)
        let q = p - 1.0;
        if (0.0..BLINK_LEN).contains(&q) {
            (q / BLINK_LEN * PI).sin()
        } else {
            0.0
        }
    } else {
        0.5 - 0.5 * (t * 2.0 * PI / SWELL_PERIOD).cos()
    }
}

/// カメラの Y 軸まわりの角度 [rad]。±`YAW_DEG` 度を `YAW_PERIOD` 秒で往復する(t = 0 は正面)。
pub fn camera_yaw(t: f32) -> f32 {
    YAW_DEG.to_radians() * (t * 2.0 * PI / YAW_PERIOD).sin()
}

/// 口の形(母音)のプリセット名。`Syllable::vowel` の番号と対応する
pub const VOWELS: [&str; 5] = ["aa", "ih", "ou", "ee", "oh"];
/// 話す区間と間をあわせた 1 フレーズの長さ [秒]
pub const PHRASE_LEN: f32 = 4.0;
/// 1 音の長さの範囲 [秒](話し言葉くらい)
pub const SYLLABLE_MIN: f32 = 0.08;
pub const SYLLABLE_MAX: f32 = 0.15;

/// 決まった入力から 0..1 の値を作る(乱数の代わり。モーションを毎回同じにするため)
fn hash01(a: u32, b: u32, c: u32) -> f32 {
    let mut h = a
        .wrapping_mul(0x9e37_79b1)
        .wrapping_add(b.wrapping_mul(0x85eb_ca77))
        .wrapping_add(c.wrapping_mul(0xc2b2_ae3d));
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297a_2d39);
    h ^= h >> 15;
    (h >> 8) as f32 / (1u32 << 24) as f32
}

/// 口を 1 回動かす音。時刻はフレーズの先頭からの秒数。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Syllable {
    pub start: f32,
    pub dur: f32,
    /// `VOWELS` の番号
    pub vowel: usize,
    /// 口の開き(0..1)
    pub peak: f32,
}

/// フレーズ `k` の音を順に返す。音と音のあいだには短い無音があり、数音ごとに息継ぎ程度の長い間が入る。
/// 話すのは冒頭の 2〜3.4 秒で、残りは黙る。
pub fn syllables(k: u32) -> impl Iterator<Item = Syllable> {
    let speak_end = 0.15 + 2.2 + 1.2 * hash01(k, 0, 1);
    let mut at = 0.15;
    let mut i = 0u32;
    std::iter::from_fn(move || {
        let dur = SYLLABLE_MIN + (SYLLABLE_MAX - SYLLABLE_MIN) * hash01(k, i, 2);
        if at + dur > speak_end {
            return None;
        }
        let s = Syllable {
            start: at,
            dur,
            vowel: ((hash01(k, i, 3) * 5.0) as usize).min(4),
            peak: 0.55 + 0.45 * hash01(k, i, 4),
        };
        // 無音は 20〜60ms、4 音に 1 回は 120〜200ms の間(語の切れ目)
        let gap = if i % 4 == 3 {
            0.12 + 0.08 * hash01(k, i, 5)
        } else {
            0.02 + 0.04 * hash01(k, i, 5)
        };
        at += dur + gap;
        i += 1;
        Some(s)
    })
}

/// 時刻 `t` の口。音の中では開いて閉じる(立ち上がり・立ち下がりをなめらかに)。無音なら None。
pub fn mouth(t: f32) -> Option<(usize, f32)> {
    let k = (t / PHRASE_LEN).floor().max(0.0) as u32;
    let u = t - k as f32 * PHRASE_LEN;
    syllables(k)
        .take_while(|s| s.start <= u)
        .find(|s| u < s.start + s.dur)
        .map(|s| {
            let x = (u - s.start) / s.dur;
            // 端 30% で 0 → 1 にあがる(中ほどは一定)
            let e = (x.min(1.0 - x) / 0.3).clamp(0.0, 1.0);
            (s.vowel, s.peak * e * e * (3.0 - 2.0 * e))
        })
}

/// 笑顔の表情(`happy`)の weight。0.2〜0.6 をゆっくり往復する。
pub fn happy_weight(t: f32) -> f32 {
    0.4 + 0.2 * (t * 2.0 * PI / 9.0).sin()
}

/// モデルが持つ表情のうち、口パク・まばたき・笑顔に使うもの。
#[derive(Clone, Copy, Debug)]
pub struct FaceSet {
    pub vowels: [bool; 5],
    pub blink: bool,
    pub happy: bool,
}

impl FaceSet {
    pub fn of(vrm: &Vrm) -> Self {
        let has = |n: &str| vrm.expression(n).is_some();
        Self {
            vowels: VOWELS.map(has),
            blink: has("blink"),
            happy: has("happy"),
        }
    }

    /// 口パクに使える母音が 1 つもない
    pub fn no_vowels(&self) -> bool {
        !self.vowels.iter().any(|&v| v)
    }
}

/// 時刻 `t` の表情を `out` に入れる(口の母音 1 つ + まばたき + 笑顔を同時に)。モデルにない表情は入れない。
pub fn face(t: f32, set: &FaceSet, out: &mut Vec<(&'static str, f32)>) {
    out.clear();
    if let Some((v, w)) = mouth(t)
        && set.vowels[v]
    {
        out.push((VOWELS[v], w));
    }
    if set.blink {
        out.push(("blink", expression_weight("blink", t)));
    }
    if set.happy {
        out.push(("happy", happy_weight(t)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blink_is_mostly_open_and_closes_briefly() {
        assert_eq!(expression_weight("blink", 0.0), 0.0);
        assert_eq!(expression_weight("blink", 3.0), 0.0);
        let peak = expression_weight("blink", 1.0 + BLINK_LEN * 0.5);
        assert!((peak - 1.0).abs() < 1e-5);
        // 周期で繰り返す
        let again = expression_weight("blink", BLINK_PERIOD + 1.0 + BLINK_LEN * 0.5);
        assert!((again - 1.0).abs() < 1e-4);
        // 全時刻で 0..=1
        for i in 0..2000 {
            let w = expression_weight("blink", i as f32 * 0.01);
            assert!((0.0..=1.0).contains(&w));
        }
    }

    #[test]
    fn other_expressions_swell_between_zero_and_one() {
        assert!(expression_weight("happy", 0.0).abs() < 1e-6);
        assert!((expression_weight("happy", SWELL_PERIOD * 0.5) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn yaw_stays_within_limit_and_starts_at_front() {
        assert_eq!(camera_yaw(0.0), 0.0);
        let max = YAW_DEG.to_radians() + 1e-5;
        for i in 0..2000 {
            assert!(camera_yaw(i as f32 * 0.02).abs() <= max);
        }
        assert!((camera_yaw(YAW_PERIOD * 0.25) - YAW_DEG.to_radians()).abs() < 1e-5);
    }

    #[test]
    fn syllables_are_speech_paced_with_short_silences() {
        for k in 0..50 {
            let list: Vec<_> = syllables(k).collect();
            assert!(list.len() >= 8, "フレーズ {k}: {} 音", list.len());
            for s in &list {
                assert!(
                    (SYLLABLE_MIN..=SYLLABLE_MAX).contains(&s.dur),
                    "長さ {}",
                    s.dur
                );
                assert!(s.vowel < 5 && (0.55..=1.0).contains(&s.peak));
                assert!(s.start + s.dur < PHRASE_LEN);
            }
            for w in list.windows(2) {
                let gap = w[1].start - (w[0].start + w[0].dur);
                assert!((0.019..0.21).contains(&gap), "間 {gap}");
            }
            // 1 フレーズの後半は黙る
            let last = list.last().unwrap();
            assert!(last.start + last.dur < 3.6);
        }
    }

    #[test]
    fn mouth_opens_inside_syllables_and_closes_between() {
        let s = syllables(3).nth(2).unwrap();
        let base = 3.0 * PHRASE_LEN;
        let mid = mouth(base + s.start + s.dur * 0.5).expect("音の中");
        assert_eq!(mid.0, s.vowel);
        assert!((mid.1 - s.peak).abs() < 1e-4);
        // 端では閉じている
        assert!(mouth(base + s.start + 1e-4).is_none_or(|(_, w)| w < 0.05));
        // フレーズの終わりは黙っている
        assert!(mouth(base + PHRASE_LEN - 0.1).is_none());
        for i in 0..4000 {
            if let Some((v, w)) = mouth(i as f32 * 0.005) {
                assert!(v < 5 && (0.0..=1.0).contains(&w));
            }
        }
    }

    #[test]
    fn face_combines_mouth_blink_and_happy() {
        let set = FaceSet {
            vowels: [true; 5],
            blink: true,
            happy: true,
        };
        let mut out = Vec::new();
        // 口が動いている最中でも blink と happy が同時に入る(blink は 0 でも入れる)
        let s = syllables(0).nth(1).unwrap();
        let t = s.start + s.dur * 0.5;
        face(t, &set, &mut out);
        assert_eq!(out.len(), 3, "t={t}: {out:?}");
        assert!(VOWELS.contains(&out[0].0) && out[1].0 == "blink" && out[2].0 == "happy");
        // まばたきの最中に口が動いている瞬間がある(3 つとも 0 より大きい)
        let found = (0..8000).map(|i| i as f32 * 0.005).find(|&t| {
            face(t, &set, &mut out);
            out.len() == 3 && out.iter().all(|(_, w)| *w > 0.05)
        });
        assert!(found.is_some(), "口・まばたき・笑顔が同時に出る瞬間がある");
        // モデルにない表情は入れない
        let none = FaceSet {
            vowels: [false; 5],
            blink: false,
            happy: false,
        };
        face(t, &none, &mut out);
        assert!(out.is_empty());
        assert!(none.no_vowels());
    }

    #[test]
    fn happy_stays_in_range() {
        for i in 0..2000 {
            assert!((0.19..=0.61).contains(&happy_weight(i as f32 * 0.05)));
        }
    }
}
