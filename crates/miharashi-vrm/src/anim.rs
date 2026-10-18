//! 実行時のポーズ: ノード階層(ローカル TRS)を毎フレーム評価して関節行列を作る。
//! 自然なポーズ(T ポーズから腕を下ろす)と待機モーション(呼吸・頭の揺れ)は、humanoid の
//! 骨に「ワールド空間の回転」を掛ける形で与える(骨のローカル軸は並びが決まっていないので、
//! ローカルで「70 度」とは言えない)。時間 [秒] の関数で、GL には触れない。
use crate::gesture;
use crate::math::{self, Mat4, QUAT_IDENTITY, Quat, Trs};
use crate::model::Model;
use std::f32::consts::TAU;

/// 腕を下ろしたときの、真下からの開き [度]
pub const ARM_LEAN_DEG: f32 = 20.0;
/// 前腕を前に曲げる角度 [度]
pub const ELBOW_BEND_DEG: f32 = 18.0;

/// ローカル変換 `local` を、親のワールド回転 `parent_rot` のもとで、ワールド空間の回転 `delta`
/// だけ回す(ノードの位置は動かさない)。`local.r = conj(P) * D * P * r`。
pub fn apply_world_rotation(local: &mut Trs, parent_rot: Quat, delta: Quat) {
    let p = parent_rot;
    local.r = math::quat_normalize(math::quat_mul(
        math::quat_conj(p),
        math::quat_mul(delta, math::quat_mul(p, local.r)),
    ));
}

/// `Rig::update` の途中(spring 前の評価のあと)に呼ばれる処理。
pub type SpringFn<'a> = &'a mut dyn FnMut(&mut Rig, &Model);

/// 時間で揺らす回転(ワールド空間の軸まわり)。
#[derive(Clone, Copy, Debug)]
struct Idle {
    node: usize,
    axis: [f32; 3],
    amp_rad: f32,
    period: f32,
    phase: f32,
}

/// 指の 1 関節。曲げる軸は、手のローカル空間で持つ(手の向きが変わっても追従する)。
struct FingerBone {
    node: usize,
    /// 0 = 親指、1 = 人差し指、2 = 中指、3 = 薬指、4 = 小指
    finger: usize,
    max_rad: f32,
    axis_local: [f32; 3],
}

/// 身振りで動かす片腕。
struct ArmRig {
    /// 腕が伸びる側(T ポーズでの向きの x の符号。+1 = +X 側)
    out: f32,
    upper: usize,
    lower: usize,
    hand: usize,
    fingers: Vec<FingerBone>,
}

/// 身振りで動かす体と頭。`(ノード, 配分)`。配分は、体の向き・前傾を各関節に分ける割合。
#[derive(Default)]
struct BodyRig {
    yaw: Vec<(usize, f32)>,
    lean: Vec<(usize, f32)>,
    head: Option<usize>,
    neck: Option<usize>,
}

pub struct Rig {
    rest: Vec<Trs>,
    /// 自然なポーズ(静的)のワールド回転
    natural: Vec<(usize, Quat)>,
    idle: Vec<Idle>,
    hips: Option<usize>,
    arms: Vec<ArmRig>,
    body: BodyRig,
    /// 身振り(`gesture::pose_at`)を重ねるか
    gesture: bool,
    /// 毎フレームの、ノードごとのワールド回転(自然なポーズ + 待機モーション)
    delta: Vec<Quat>,
    /// 腰の上下 [m](ワールド Y)
    hip_bob: f32,
    /// spring などがローカル回転を直接決めるノード
    pub override_rot: Vec<Option<Quat>>,
    pub locals: Vec<Trs>,
    pub world: Vec<Mat4>,
    pub world_rot: Vec<Quat>,
    pub joints: Vec<Mat4>,
}

impl Rig {
    /// `pose` が true なら自然なポーズを求める(false は初期姿勢のまま)。
    pub fn new(model: &Model, pose: bool) -> Self {
        let n = model.nodes.len();
        let mut rig = Self {
            rest: model.rest_locals(),
            natural: Vec::new(),
            idle: Vec::new(),
            hips: model.vrm.human_bones.get("hips").copied(),
            arms: Vec::new(),
            body: BodyRig::default(),
            gesture: false,
            delta: vec![QUAT_IDENTITY; n],
            hip_bob: 0.0,
            override_rot: vec![None; n],
            locals: Vec::new(),
            world: Vec::new(),
            world_rot: Vec::new(),
            joints: Vec::new(),
        };
        rig.locals = rig.rest.clone();
        rig.eval(model);
        if pose {
            // 腕と指の軸は初期姿勢(T ポーズ)の形から決める。自然なポーズを掛ける前に
            rig.arms = build_arms(model, &rig.world, &rig.world_rot);
            rig.body = build_body(model);
            rig.natural = rig.natural_pose(model);
            rig.idle = idle_rules(model);
        }
        // animate を呼ぶ前でも描けるよう、初期姿勢の関節行列を持たせる
        model.joint_matrices(&rig.world, &mut rig.joints);
        rig
    }

    /// 身振りを重ねる。腕の骨がなければ false を返して何もしない。
    pub fn set_gesture(&mut self, on: bool) -> bool {
        self.gesture = on && !self.arms.is_empty();
        !on || self.gesture
    }

    /// 身振りで動かせる腕と、指の関節の数。
    pub fn gesture_bones(&self) -> (usize, usize) {
        (
            self.arms.len(),
            self.arms.iter().map(|a| a.fingers.len()).sum(),
        )
    }

    pub fn has_pose(&self) -> bool {
        !self.natural.is_empty() || !self.idle.is_empty()
    }

    /// `delta` / `override_rot` / `hip_bob` を使って、ローカル → ワールドを評価する。
    pub fn eval(&mut self, model: &Model) {
        let n = self.rest.len();
        self.locals.resize(n, self.rest[0]);
        self.world.resize(n, math::IDENTITY);
        self.world_rot.resize(n, QUAT_IDENTITY);
        for &i in &model.order {
            let parent = model.nodes[i].parent;
            let pr = parent.map_or(QUAT_IDENTITY, |p| self.world_rot[p]);
            let mut l = self.rest[i];
            if let Some(r) = self.override_rot[i] {
                l.r = r;
            } else if self.delta[i] != QUAT_IDENTITY {
                apply_world_rotation(&mut l, pr, self.delta[i]);
            }
            if Some(i) == self.hips && self.hip_bob != 0.0 {
                // ワールド Y の移動を親の空間に直す
                let d = math::quat_rotate(math::quat_conj(pr), [0.0, self.hip_bob, 0.0]);
                l.t = math::vec_add(l.t, d);
            }
            self.locals[i] = l;
            self.world_rot[i] = math::quat_mul(pr, l.r);
            let lm = l.to_mat4();
            self.world[i] = match parent {
                Some(p) => math::mul(&self.world[p], &lm),
                None => lm,
            };
        }
    }

    fn natural_pose(&mut self, model: &Model) -> Vec<(usize, Quat)> {
        let hb = &model.vrm.human_bones;
        let mut out = Vec::new();
        for side in ["left", "right"] {
            let get = |k: &str| hb.get(&format!("{side}{k}")).copied();
            let (Some(upper), Some(lower), Some(hand)) =
                (get("UpperArm"), get("LowerArm"), get("Hand"))
            else {
                continue;
            };
            let pos = |w: &[Mat4], i: usize| [w[i][3][0], w[i][3][1], w[i][3][2]];
            // 上腕: 腕の向き(上腕 → 前腕)を、真下から外側へ ARM_LEAN_DEG 開いた向きに
            self.delta.fill(QUAT_IDENTITY);
            self.eval(model);
            let dir = math::vec_sub(pos(&self.world, lower), pos(&self.world, upper));
            // 外側は、腕が今向いている側(T ポーズなら ±X)
            let side_x = if dir[0] < 0.0 { -1.0 } else { 1.0 };
            let a = ARM_LEAN_DEG.to_radians();
            let target = [side_x * a.sin(), -a.cos(), 0.0];
            let du = math::quat_from_to(dir, target);
            self.delta[upper] = du;
            // 前腕: 下がった腕の向きから、前(+Z)へ ELBOW_BEND_DEG 曲げる
            self.eval(model);
            let fdir = math::vec_sub(pos(&self.world, hand), pos(&self.world, lower));
            let axis = math::vec_cross(fdir, [0.0, 0.0, 1.0]);
            let dl = math::quat_axis_angle(axis, ELBOW_BEND_DEG.to_radians());
            self.delta[lower] = dl;
            out.push((upper, du));
            out.push((lower, dl));
        }
        // 後続(spring の初期化など)が見る world は、自然なポーズ前の初期姿勢に戻しておく
        self.delta.fill(QUAT_IDENTITY);
        self.eval(model);
        out
    }

    fn rotate_world(&mut self, node: usize, axis: [f32; 3], rad: f32) {
        self.delta[node] = math::quat_mul(math::quat_axis_angle(axis, rad), self.delta[node]);
    }

    /// 身振り(`gesture::pose_at`)を `delta` に重ねる。体と頭は世界の軸まわりの回転、腕は
    /// 「上腕 → 前腕」を目標の向きに合わせる回転、指は手のローカルの軸まわりの曲げ。
    /// 途中で何度か `eval` して、腕の現在の向きを求め直す。
    fn apply_gesture(&mut self, model: &Model, t: f32) {
        const X: [f32; 3] = [1.0, 0.0, 0.0];
        const Y: [f32; 3] = [0.0, 1.0, 0.0];
        const Z: [f32; 3] = [0.0, 0.0, 1.0];
        let g = gesture::pose_at(t);
        let yaw = g.body_yaw_deg.to_radians();
        let lean = g.body_lean_deg.to_radians();
        let mut applied_yaw = 0.0;
        for i in 0..self.body.yaw.len() {
            let (n, share) = self.body.yaw[i];
            self.rotate_world(n, Y, yaw * share);
            applied_yaw += yaw * share;
        }
        for i in 0..self.body.lean.len() {
            let (n, share) = self.body.lean[i];
            self.rotate_world(n, X, lean * share);
        }
        if let Some(n) = self.body.neck {
            self.rotate_world(n, Y, g.head_yaw_deg.to_radians() * 0.35);
        }
        if let Some(n) = self.body.head {
            self.rotate_world(n, Y, g.head_yaw_deg.to_radians() * 0.65);
            self.rotate_world(n, X, g.head_pitch_deg.to_radians());
            self.rotate_world(n, Z, g.head_roll_deg.to_radians());
        }
        self.eval(model);

        let body_q = math::quat_axis_angle(Y, applied_yaw);
        let target = |pose: &gesture::ArmPose, forearm: bool| {
            math::quat_rotate(body_q, if forearm { pose.fore } else { pose.upper })
        };
        let pos = |w: &[Mat4], i: usize| [w[i][3][0], w[i][3][1], w[i][3][2]];
        let pose_of = |out: f32| if out > 0.0 { g.plus_x } else { g.minus_x };
        // 上腕
        for k in 0..self.arms.len() {
            let (out, upper, lower) = (self.arms[k].out, self.arms[k].upper, self.arms[k].lower);
            let cur = math::vec_sub(pos(&self.world, lower), pos(&self.world, upper));
            let d = math::quat_from_to(cur, target(&pose_of(out), false));
            self.delta[upper] = math::quat_mul(d, self.delta[upper]);
        }
        self.eval(model);
        // 前腕と手首
        for k in 0..self.arms.len() {
            let (out, lower, hand) = (self.arms[k].out, self.arms[k].lower, self.arms[k].hand);
            let pose = pose_of(out);
            let want = target(&pose, true);
            let cur = math::vec_sub(pos(&self.world, hand), pos(&self.world, lower));
            let d = math::quat_from_to(cur, want);
            self.delta[lower] = math::quat_mul(d, self.delta[lower]);
            let axis = math::vec_cross(want, Z);
            let axis = if math::vec_len(axis) > 1e-4 { axis } else { X };
            self.delta[hand] = math::quat_axis_angle(axis, pose.wrist_deg.to_radians());
        }
        self.eval(model);
        // 指(手の向きが決まったあとに、手のローカルの軸を世界へ戻して曲げる)
        for k in 0..self.arms.len() {
            let pose = pose_of(self.arms[k].out);
            let hand_rot = self.world_rot[self.arms[k].hand];
            for i in 0..self.arms[k].fingers.len() {
                let f = &self.arms[k].fingers[i];
                let axis = math::quat_rotate(hand_rot, f.axis_local);
                let q = math::quat_axis_angle(axis, pose.curl[f.finger] * f.max_rad);
                let node = f.node;
                self.delta[node] = q;
            }
        }
    }

    /// 時刻 `t` のポーズを評価して、関節行列を作る。`spring` は評価の途中で呼ばれ、
    /// `override_rot` を設定して再評価の対象にする(spring を使わなければ `None`)。
    pub fn update(&mut self, model: &Model, t: f32, spring: Option<SpringFn<'_>>) {
        self.delta.fill(QUAT_IDENTITY);
        self.hip_bob = 0.0;
        for &(n, q) in &self.natural {
            self.delta[n] = q;
        }
        for r in &self.idle {
            let q = math::quat_axis_angle(r.axis, r.amp_rad * (TAU * t / r.period + r.phase).sin());
            self.delta[r.node] = math::quat_mul(q, self.delta[r.node]);
        }
        if !self.idle.is_empty() {
            self.hip_bob = HIP_BOB_M * (TAU * t / BREATH_PERIOD).sin();
        }
        if self.gesture {
            self.apply_gesture(model, t);
        }
        self.eval(model);
        if let Some(f) = spring {
            f(self, model);
            self.eval(model);
        }
        model.joint_matrices(&self.world, &mut self.joints);
    }
}

/// 指の関節ごとの、握り切ったときの曲げ角 [度](親指以外: 付け根・中・先)
const FINGER_CURL_DEG: [f32; 3] = [75.0, 85.0, 55.0];
/// 親指(付け根・先)
const THUMB_CURL_DEG: [f32; 2] = [30.0, 45.0];

fn build_arms(model: &Model, world: &[Mat4], world_rot: &[Quat]) -> Vec<ArmRig> {
    let hb = &model.vrm.human_bones;
    let pos = |i: usize| [world[i][3][0], world[i][3][1], world[i][3][2]];
    let mut arms = Vec::new();
    for side in ["left", "right"] {
        let get = |k: &str| hb.get(&format!("{side}{k}")).copied();
        let (Some(upper), Some(lower), Some(hand)) =
            (get("UpperArm"), get("LowerArm"), get("Hand"))
        else {
            continue;
        };
        let dir = math::vec_sub(pos(lower), pos(upper));
        // T ポーズなら腕は ±X に伸びている。向きが取れないときは VRM 1.0 の左 = +X
        let out = if dir[0].abs() > 0.2 * math::vec_len(dir) {
            dir[0].signum()
        } else if side == "left" {
            1.0
        } else {
            -1.0
        };
        let mut fingers = Vec::new();
        let hand_inv = math::quat_conj(world_rot[hand]);
        for (fi, name) in ["Thumb", "Index", "Middle", "Ring", "Little"]
            .into_iter()
            .enumerate()
        {
            // 親指は Metacarpal を動かさず、Proximal(と、あれば Intermediate)・Distal を曲げる
            let joints: Vec<(usize, f32)> = if fi == 0 {
                [
                    ("Proximal", THUMB_CURL_DEG[0]),
                    ("Distal", THUMB_CURL_DEG[1]),
                ]
                .into_iter()
                .filter_map(|(j, d)| get(&format!("{name}{j}")).map(|n| (n, d)))
                .collect()
            } else {
                ["Proximal", "Intermediate", "Distal"]
                    .into_iter()
                    .zip(FINGER_CURL_DEG)
                    .filter_map(|(j, d)| get(&format!("{name}{j}")).map(|n| (n, d)))
                    .collect()
            };
            for (k, &(node, deg)) in joints.iter().enumerate() {
                // 指の向き: 次の関節への向き(先端は 1 つ手前からの向き)。手のひらは T ポーズで下向きとみなし、
                // 曲げる軸は (指の向き × 下) = 指先が下へ向く回転
                let d = match joints.get(k + 1) {
                    Some(&(next, _)) => math::vec_sub(pos(next), pos(node)),
                    None if k > 0 => math::vec_sub(pos(node), pos(joints[k - 1].0)),
                    None => continue,
                };
                let axis = math::vec_cross(math::normalize(d), [0.0, -1.0, 0.0]);
                if math::vec_len(axis) < 0.1 {
                    continue;
                }
                fingers.push(FingerBone {
                    node,
                    finger: fi,
                    max_rad: deg.to_radians(),
                    axis_local: math::quat_rotate(hand_inv, math::normalize(axis)),
                });
            }
        }
        arms.push(ArmRig {
            out,
            upper,
            lower,
            hand,
            fingers,
        });
    }
    arms
}

fn build_body(model: &Model) -> BodyRig {
    let hb = &model.vrm.human_bones;
    let get = |k: &str| hb.get(k).copied();
    let (upper_chest, chest) = (get("upperChest"), get("chest"));
    let mut yaw = Vec::new();
    let mut lean = Vec::new();
    let add = |v: &mut Vec<(usize, f32)>, n: Option<usize>, share: f32| {
        if let Some(n) = n {
            v.push((n, share));
        }
    };
    add(&mut yaw, get("hips"), 0.15);
    add(&mut yaw, get("spine"), 0.25);
    // 上胸があれば胸と半分ずつ
    let (c, u) = if upper_chest.is_some() {
        (0.2, 0.15)
    } else {
        (0.35, 0.0)
    };
    add(&mut yaw, chest, c);
    add(&mut yaw, upper_chest, u);
    add(&mut yaw, get("neck"), 0.1);
    add(&mut lean, get("spine"), 0.5);
    add(
        &mut lean,
        chest,
        if upper_chest.is_some() { 0.3 } else { 0.5 },
    );
    add(&mut lean, upper_chest, 0.2);
    BodyRig {
        yaw,
        lean,
        head: get("head"),
        neck: get("neck"),
    }
}

/// 呼吸の周期 [秒]
pub const BREATH_PERIOD: f32 = 4.0;
/// 呼吸に合わせた腰の上下 [m]
pub const HIP_BOB_M: f32 = 0.002;

fn idle_rules(model: &Model) -> Vec<Idle> {
    let hb = &model.vrm.human_bones;
    let mut v = Vec::new();
    let mut add = |names: &[&str], axis, deg: f32, period, phase| {
        if let Some(&node) = names.iter().find_map(|n| hb.get(*n)) {
            v.push(Idle {
                node,
                axis,
                amp_rad: deg.to_radians(),
                period,
                phase,
            });
        }
    };
    let x = [1.0, 0.0, 0.0];
    let y = [0.0, 1.0, 0.0];
    // 呼吸: 胸と背骨が少し反る
    add(&["upperChest", "chest"], x, -1.0, BREATH_PERIOD, 0.0);
    add(&["spine"], x, -0.6, BREATH_PERIOD, 0.3);
    // 頭: 首と頭でゆっくり。周期を変えて機械的にしない
    add(&["neck"], y, 1.0, 7.0, 0.0);
    add(&["head"], y, 1.5, 7.0, 0.4);
    add(&["neck"], x, 0.5, 5.3, 1.0);
    add(&["head"], x, 1.0, 5.3, 1.3);
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::{quat_axis_angle, quat_rotate};

    #[test]
    fn world_rotation_is_independent_of_parent_orientation() {
        // 親が Z まわりに 90 度回っていても、ワールドの Y まわり 30 度は Y まわり 30 度のまま
        let parent = quat_axis_angle([0.0, 0.0, 1.0], std::f32::consts::FRAC_PI_2);
        let mut local = Trs {
            t: [0.0; 3],
            r: QUAT_IDENTITY,
            s: [1.0; 3],
        };
        let d = quat_axis_angle([0.0, 1.0, 0.0], 30f32.to_radians());
        apply_world_rotation(&mut local, parent, d);
        let world = math::quat_mul(parent, local.r);
        // ワールド回転 = d * parent
        let expect = math::quat_mul(d, parent);
        for v in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] {
            let (a, b) = (quat_rotate(world, v), quat_rotate(expect, v));
            for k in 0..3 {
                assert!((a[k] - b[k]).abs() < 1e-5, "{a:?} {b:?}");
            }
        }
    }
}
