//! 実行時のポーズ: ノード階層(ローカル TRS)を毎フレーム評価して関節行列を作る。
//! 自然なポーズ(T ポーズから腕を下ろす)と待機モーション(呼吸・頭の揺れ)は、humanoid の
//! 骨に「ワールド空間の回転」を掛ける形で与える(骨のローカル軸は並びが決まっていないので、
//! ローカルで「70 度」とは言えない)。時間 [秒] の関数で、GL には触れない。
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

pub struct Rig {
    rest: Vec<Trs>,
    /// 自然なポーズ(静的)のワールド回転
    natural: Vec<(usize, Quat)>,
    idle: Vec<Idle>,
    hips: Option<usize>,
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
            rig.natural = rig.natural_pose(model);
            rig.idle = idle_rules(model);
        }
        // animate を呼ぶ前でも描けるよう、初期姿勢の関節行列を持たせる
        model.joint_matrices(&rig.world, &mut rig.joints);
        rig
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
        self.eval(model);
        if let Some(f) = spring {
            f(self, model);
            self.eval(model);
        }
        model.joint_matrices(&self.world, &mut self.joints);
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
