//! 身振り(説明しながらの手の動き)の手続きモーション。時間 [秒] から、両腕・体・頭の目標を返すだけの
//! 純粋関数で、モデルにも GL にも触れない。腕の目標は向き(単位ベクトル)で持ち、骨の回転への変換は
//! `anim::Rig` が行う。
//!
//! 向きはキャラクターの体を基準にする: x = +X 側(VRM 1.0 では +Z が前なので、画面の右 = キャラクターの左)、
//! y = 上、z = 前。腕は +X 側の腕(`plus_x`)と -X 側の腕(`minus_x`)で別の目標を持つ。
//! 流れは「待機 → 片手で説明 → 両手を広げる → 手を振る → 指さし」を順に繰り返し、
//! 動きの間は滑らかな補間(なめらかな S 字、`MOVE_SECS`)でつなぐ。
use crate::math;
use std::f32::consts::TAU;

pub type V3 = [f32; 3];

/// 片腕の目標。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArmPose {
    /// 上腕(肩 → 肘)の向き
    pub upper: V3,
    /// 前腕(肘 → 手首)の向き
    pub fore: V3,
    /// 手首の折り曲げ [度](前腕の向きと前方 +Z に垂直な軸まわり。正で手のひらを上へ起こす向き)
    pub wrist_deg: f32,
    /// 指の曲げ(0 = 伸ばす、1 = 握る)。親指・人差し指・中指・薬指・小指
    pub curl: [f32; 5],
}

/// ある時刻の身振り全体。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GesturePose {
    pub plus_x: ArmPose,
    pub minus_x: ArmPose,
    /// 体の向き [度]。正で +X 側(画面の右)へ向く
    pub body_yaw_deg: f32,
    /// 体の前傾 [度]
    pub body_lean_deg: f32,
    /// 頭の向き [度](正で +X 側)
    pub head_yaw_deg: f32,
    /// 頭のうなずき [度](正でおじぎ)
    pub head_pitch_deg: f32,
    /// 頭の傾き [度]
    pub head_roll_deg: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gesture {
    /// 待機(腕を下ろす)
    Neutral,
    /// +X 側の片手を上げて説明する
    Explain,
    /// 両手を広げる
    Spread,
    /// -X 側の手を振る
    Wave,
    /// +X 側の腕で指さす
    Point,
}

/// 順序と、各身振りを保つ長さ [秒](次への移行 `MOVE_SECS` は別)
pub const SEQUENCE: [(Gesture, f32); 5] = [
    (Gesture::Neutral, 1.5),
    (Gesture::Explain, 4.5),
    (Gesture::Spread, 3.5),
    (Gesture::Wave, 3.5),
    (Gesture::Point, 4.0),
];
/// 次の身振りへ移るのにかける時間 [秒]
pub const MOVE_SECS: f32 = 0.9;

/// 全体の周期 [秒]
pub fn cycle_secs() -> f32 {
    SEQUENCE.iter().map(|(_, hold)| hold + MOVE_SECS).sum()
}

const fn arm(upper: V3, fore: V3, wrist_deg: f32, curl: [f32; 5]) -> ArmPose {
    ArmPose {
        upper,
        fore,
        wrist_deg,
        curl,
    }
}

/// 腕を下ろして軽く握った姿勢(`anim` の自然なポーズと同じ向き)
const REST_ARM: ArmPose = arm(
    [0.342, -0.940, 0.0],
    [0.325, -0.894, 0.309],
    0.0,
    [0.25, 0.2, 0.25, 0.3, 0.35],
);

fn key(g: Gesture) -> GesturePose {
    let mut p = GesturePose {
        plus_x: REST_ARM,
        minus_x: mirror(REST_ARM),
        body_yaw_deg: 0.0,
        body_lean_deg: 0.0,
        head_yaw_deg: 0.0,
        head_pitch_deg: 0.0,
        head_roll_deg: 0.0,
    };
    match g {
        Gesture::Neutral => {}
        Gesture::Explain => {
            // 肘を前に出して、手を胸の前に。手のひらを少し上に向けて指をゆるく開く
            p.plus_x = arm(
                [0.35, -0.85, 0.40],
                [0.20, 0.55, 0.80],
                18.0,
                [0.2, 0.15, 0.25, 0.3, 0.35],
            );
            p.body_yaw_deg = -3.0;
            p.body_lean_deg = 2.0;
            p.head_yaw_deg = 3.0;
            p.head_roll_deg = 3.0;
        }
        Gesture::Spread => {
            // 両手を体の前の左右に開く(手のひらを上、指は開く)
            let a = arm(
                [0.30, -0.85, 0.40],
                [0.30, 0.35, 0.90],
                12.0,
                [0.05, 0.05, 0.08, 0.1, 0.12],
            );
            p.plus_x = a;
            p.minus_x = mirror(a);
            p.body_lean_deg = 3.0;
            p.head_pitch_deg = -2.0;
        }
        Gesture::Wave => {
            // 肘を横に出して、前腕を立てて振る
            p.minus_x = mirror(arm(
                [0.30, -0.65, 0.55],
                [0.20, 0.95, 0.25],
                0.0,
                [0.05, 0.05, 0.05, 0.08, 0.1],
            ));
            p.body_yaw_deg = -5.0;
            p.head_roll_deg = -4.0;
            p.head_yaw_deg = -4.0;
        }
        Gesture::Point => {
            // +X 側(写真のある側)を向いて、腕を伸ばして人差し指で指す
            p.plus_x = arm(
                [0.30, -0.55, 0.65],
                [0.50, 0.25, 0.80],
                0.0,
                [0.45, 0.0, 0.9, 0.95, 0.95],
            );
            p.body_yaw_deg = 10.0;
            p.head_yaw_deg = 14.0;
            p.body_lean_deg = 1.0;
        }
    }
    p
}

/// 「外側が正」で書いた目標を -X 側の腕の向きにするため、x 成分の符号を反転する。
fn mirror(a: ArmPose) -> ArmPose {
    let f = |v: V3| [-v[0], v[1], v[2]];
    ArmPose {
        upper: f(a.upper),
        fore: f(a.fore),
        ..a
    }
}

fn smoother(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * x * (x * (x * 6.0 - 15.0) + 10.0)
}

fn lerp(a: f32, b: f32, s: f32) -> f32 {
    a + (b - a) * s
}

fn blend_arm(a: &ArmPose, b: &ArmPose, s: f32) -> ArmPose {
    ArmPose {
        upper: math::vec_slerp(a.upper, b.upper, s),
        fore: math::vec_slerp(a.fore, b.fore, s),
        wrist_deg: lerp(a.wrist_deg, b.wrist_deg, s),
        curl: std::array::from_fn(|i| lerp(a.curl[i], b.curl[i], s).clamp(0.0, 1.0)),
    }
}

fn blend(a: &GesturePose, b: &GesturePose, s: f32) -> GesturePose {
    GesturePose {
        plus_x: blend_arm(&a.plus_x, &b.plus_x, s),
        minus_x: blend_arm(&a.minus_x, &b.minus_x, s),
        body_yaw_deg: lerp(a.body_yaw_deg, b.body_yaw_deg, s),
        body_lean_deg: lerp(a.body_lean_deg, b.body_lean_deg, s),
        head_yaw_deg: lerp(a.head_yaw_deg, b.head_yaw_deg, s),
        head_pitch_deg: lerp(a.head_pitch_deg, b.head_pitch_deg, s),
        head_roll_deg: lerp(a.head_roll_deg, b.head_roll_deg, s),
    }
}

/// 時刻 `t` で、いま `(from, to)` のあいだを `s`(0..=1、なめらか)進んでいる。保持中は `from == to`
/// ではなく `s = 0`。
fn locate(t: f32) -> (usize, usize, f32) {
    let total = cycle_secs();
    let mut x = t.rem_euclid(total);
    for (i, &(_, hold)) in SEQUENCE.iter().enumerate() {
        let next = (i + 1) % SEQUENCE.len();
        if x < hold {
            return (i, next, 0.0);
        }
        x -= hold;
        if x < MOVE_SECS {
            return (i, next, smoother(x / MOVE_SECS));
        }
        x -= MOVE_SECS;
    }
    (0, 1, 0.0)
}

/// 話しながらの小さな動き(説明の拍、手を振る往復など)。その身振りの重み `w` だけ足す。
fn wiggle(g: Gesture, t: f32, w: f32, p: &mut GesturePose) {
    if w <= 0.0 {
        return;
    }
    let add = |v: &mut V3, d: V3| {
        *v = math::normalize([v[0] + d[0] * w, v[1] + d[1] * w, v[2] + d[2] * w]);
    };
    match g {
        Gesture::Neutral => {
            // 待機でも、頭と指がほんの少し動く
            p.head_pitch_deg += 1.0 * w * (TAU * t / 3.1).sin();
        }
        Gesture::Explain => {
            // 話の拍に合わせて前腕が上下し、頭がうなずく
            let beat = (TAU * 1.3 * t).sin();
            add(&mut p.plus_x.fore, [0.0, 0.12 * beat, -0.04 * beat]);
            add(&mut p.plus_x.upper, [0.0, 0.0, 0.04 * beat]);
            p.plus_x.wrist_deg += 6.0 * w * beat;
            p.head_pitch_deg += 3.0 * w * (TAU * 0.9 * t).sin();
            p.head_yaw_deg += 2.0 * w * (TAU * 0.35 * t).sin();
        }
        Gesture::Spread => {
            let beat = (TAU * 0.8 * t).sin();
            // 外側が正の向き。-X 側は x 成分を反転して足す
            add(&mut p.plus_x.fore, [0.05 * beat, 0.10 * beat, 0.0]);
            add(&mut p.minus_x.fore, [-0.05 * beat, 0.10 * beat, 0.0]);
            p.head_pitch_deg += 2.0 * w * (TAU * 0.6 * t).sin();
        }
        Gesture::Wave => {
            // 前腕を左右(肘を軸に)に振る。手首も少しついてくる
            let swing = (TAU * 2.2 * t).sin();
            add(&mut p.minus_x.fore, [-0.55 * swing, 0.0, 0.0]);
            p.minus_x.wrist_deg += 8.0 * w * swing;
            p.head_roll_deg += 2.0 * w * (TAU * 1.1 * t).sin();
        }
        Gesture::Point => {
            let beat = (TAU * 1.0 * t).sin();
            add(&mut p.plus_x.fore, [0.0, 0.04 * beat, 0.0]);
            p.head_pitch_deg += 2.0 * w * (TAU * 0.7 * t).sin();
        }
    }
}

/// 時刻 `t` [秒] の身振り。周期は `cycle_secs()`(待機から始まり、周期でつながる)。
pub fn pose_at(t: f32) -> GesturePose {
    let (i, j, s) = locate(t);
    let (ga, gb) = (SEQUENCE[i].0, SEQUENCE[j].0);
    let mut p = blend(&key(ga), &key(gb), s);
    wiggle(ga, t, 1.0 - s, &mut p);
    wiggle(gb, t, s, &mut p);
    p
}

/// 視線と指さしの目標(`--follow-cards`)。`pose_at` の身振りに重ねる。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aim {
    /// 目標の向き(世界で、キャラクターの正面を +Z とした単位ベクトル。x = +X 側 = 画面の右、y = 上)
    pub dir: V3,
    /// 頭と体を目標へ向ける割合(0〜1)
    pub look: f32,
    /// 腕を伸ばして指さす割合(0〜1)
    pub point: f32,
}

/// 頭の向きの上限 [度]
const AIM_YAW_MAX: f32 = 35.0;

/// 指さす腕の目標。前腕は `v` の向きに伸ばし、上腕はそれより下に垂らす(`key(Point)` の形に近い)。
fn point_arm(v: V3) -> ArmPose {
    // 真横に近い目標でも、腕が体の中へ入らないよう前向きの成分を残す
    let fore = math::normalize([v[0], v[1], v[2].max(0.35)]);
    let upper = math::normalize([fore[0] * 0.6, fore[1] - 0.7, fore[2] * 0.9 + 0.2]);
    arm(upper, fore, 0.0, [0.45, 0.0, 0.9, 0.95, 0.95])
}

fn apply_aim(p: &mut GesturePose, aim: &Aim) {
    let look = aim.look.clamp(0.0, 1.0);
    let point = aim.point.clamp(0.0, 1.0);
    let v = math::normalize(aim.dir);
    let yaw = v[0]
        .atan2(v[2])
        .to_degrees()
        .clamp(-AIM_YAW_MAX, AIM_YAW_MAX);
    // 目標が上なら頭を上げる(正がおじぎ)
    let pitch = (-v[1])
        .clamp(-1.0, 1.0)
        .asin()
        .to_degrees()
        .clamp(-15.0, 15.0);
    // 向きは頭に 3/4、体に 1/4
    p.head_yaw_deg = lerp(p.head_yaw_deg, yaw * 0.75, look);
    p.body_yaw_deg = lerp(p.body_yaw_deg, yaw * 0.25, look);
    p.head_pitch_deg = lerp(p.head_pitch_deg, pitch, look);
    p.head_roll_deg = lerp(p.head_roll_deg, 0.0, look);
    if point > 0.0 {
        // 腕の目標は体を基準にするので、体の向きの分だけ戻す
        let body = math::quat_axis_angle([0.0, 1.0, 0.0], (-p.body_yaw_deg).to_radians());
        let local = math::quat_rotate(body, v);
        let target = point_arm(local);
        let side = if v[0] >= 0.0 {
            &mut p.plus_x
        } else {
            &mut p.minus_x
        };
        *side = blend_arm(side, &target, point);
    }
}

/// `pose_at` に、視線と指さしの目標(あれば)を重ねた身振り。
pub fn pose_aimed(t: f32, aim: Option<&Aim>) -> GesturePose {
    let mut p = pose_at(t);
    if let Some(a) = aim {
        apply_aim(&mut p, a);
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(v: V3) -> bool {
        (math::vec_len(v) - 1.0).abs() < 1e-4
    }

    fn angle_deg(a: V3, b: V3) -> f32 {
        math::vec_dot(math::normalize(a), math::normalize(b))
            .clamp(-1.0, 1.0)
            .acos()
            .to_degrees()
    }

    #[test]
    fn starts_neutral_and_repeats_each_cycle() {
        let p = pose_at(0.0);
        assert!(angle_deg(p.plus_x.upper, REST_ARM.upper) < 0.5);
        assert!(angle_deg(p.minus_x.fore, mirror(REST_ARM).fore) < 0.5);
        assert!(p.minus_x.upper[0] < 0.0 && p.plus_x.upper[0] > 0.0);
        let c = cycle_secs();
        // 周期でつながる(wiggle の位相は周期と合わないので、腕の向きの差だけを見る)
        let (a, b) = (pose_at(0.2), pose_at(c + 0.2));
        assert!(angle_deg(a.plus_x.upper, b.plus_x.upper) < 1.0);
    }

    #[test]
    fn directions_are_unit_and_values_bounded() {
        let mut t = 0.0;
        while t < cycle_secs() * 2.0 {
            let p = pose_at(t);
            for a in [&p.plus_x, &p.minus_x] {
                assert!(unit(a.upper) && unit(a.fore), "t={t}");
                assert!(a.wrist_deg.abs() < 40.0, "t={t}: {}", a.wrist_deg);
                assert!(a.curl.iter().all(|c| (0.0..=1.0).contains(c)), "t={t}");
            }
            for v in [
                p.body_yaw_deg,
                p.head_yaw_deg,
                p.head_pitch_deg,
                p.head_roll_deg,
                p.body_lean_deg,
            ] {
                assert!(v.abs() < 30.0, "t={t}: {v}");
            }
            t += 0.01;
        }
    }

    #[test]
    fn motion_is_continuous() {
        // 1/60 秒で腕の向きが飛ばない(手を振る往復を含めても 1 フレーム 12 度以内)
        let dt = 1.0 / 60.0;
        let mut prev = pose_at(0.0);
        let mut t = dt;
        while t < cycle_secs() * 2.0 {
            let p = pose_at(t);
            for (a, b) in [(&prev.plus_x, &p.plus_x), (&prev.minus_x, &p.minus_x)] {
                assert!(angle_deg(a.upper, b.upper) < 12.0, "upper t={t}");
                assert!(angle_deg(a.fore, b.fore) < 12.0, "fore t={t}");
            }
            assert!((prev.body_yaw_deg - p.body_yaw_deg).abs() < 1.0, "t={t}");
            assert!((prev.head_yaw_deg - p.head_yaw_deg).abs() < 1.0, "t={t}");
            prev = p;
            t += dt;
        }
    }

    #[test]
    fn each_gesture_is_reached_while_holding() {
        // 各身振りの保持の中ほどで、その身振りの特徴が出ている
        let at = |g: Gesture| {
            let mut s = 0.0;
            for &(gg, hold) in SEQUENCE.iter() {
                if gg == g {
                    return pose_at(s + hold * 0.5);
                }
                s += hold + MOVE_SECS;
            }
            unreachable!()
        };
        let explain = at(Gesture::Explain);
        assert!(explain.plus_x.fore[1] > 0.3, "前腕が上を向く");
        assert!(angle_deg(explain.minus_x.upper, mirror(REST_ARM).upper) < 3.0);
        let spread = at(Gesture::Spread);
        assert!(spread.plus_x.fore[0] > 0.2 && spread.minus_x.fore[0] < -0.2);
        let wave = at(Gesture::Wave);
        assert!(wave.minus_x.fore[1] > 0.5, "手を振る腕の前腕が立つ");
        let point = at(Gesture::Point);
        assert!(point.plus_x.fore[0] > 0.4, "指さしは +X 側へ伸びる");
        assert!(point.plus_x.curl[1] < 0.1 && point.plus_x.curl[2] > 0.8);
        assert!(point.body_yaw_deg > 5.0);
    }

    #[test]
    fn aim_turns_head_and_points_the_nearer_arm() {
        let t = 0.3;
        let none = pose_aimed(t, None);
        assert_eq!(none, pose_at(t));
        // 重みが 0 なら変わらない
        let zero = Aim {
            dir: [0.6, 0.0, 0.8],
            look: 0.0,
            point: 0.0,
        };
        assert!(angle_deg(pose_aimed(t, Some(&zero)).plus_x.fore, none.plus_x.fore) < 1e-3);
        // 右の目標: 頭が +X 側へ向き、+X 側の腕が目標の向きに伸びる
        let right = Aim {
            dir: math::normalize([0.6, 0.1, 0.8]),
            look: 1.0,
            point: 1.0,
        };
        let p = pose_aimed(t, Some(&right));
        assert!(p.head_yaw_deg > 10.0 && p.head_yaw_deg <= AIM_YAW_MAX);
        assert!(p.plus_x.fore[0] > 0.4 && p.plus_x.curl[1] < 0.1);
        assert!(unit(p.plus_x.upper) && unit(p.plus_x.fore));
        assert!(
            angle_deg(p.minus_x.fore, none.minus_x.fore) < 1e-3,
            "反対の腕はそのまま"
        );
        // 左の目標は -X 側の腕
        let left = Aim {
            dir: math::normalize([-0.6, -0.1, 0.8]),
            ..right
        };
        let q = pose_aimed(t, Some(&left));
        assert!(q.head_yaw_deg < -10.0);
        assert!(q.minus_x.fore[0] < -0.4 && q.minus_x.curl[1] < 0.1);
        assert!(angle_deg(q.plus_x.fore, none.plus_x.fore) < 1e-3);
        // 真横に近い目標でも腕は前向きの成分を残す
        let side = Aim {
            dir: [1.0, 0.0, 0.0],
            ..right
        };
        assert!(pose_aimed(t, Some(&side)).plus_x.fore[2] > 0.3);
    }
}
