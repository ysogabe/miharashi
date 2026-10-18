//! 実モニター表示(`vrm-view`)の演出。時間 [秒] から表情の weight とカメラの角度を決めるだけで、GL には触れない。
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
}
