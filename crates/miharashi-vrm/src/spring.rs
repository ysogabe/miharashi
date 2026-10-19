//! SpringBone(VRMC_springBone)。three-vrm と同じ Verlet 積分を CPU で計算し、揺れる骨のローカル回転を
//! `Rig::override_rot` に入れる。モデル空間(= ワールド)で計算し、center は使わない。
use crate::anim::Rig;
use crate::math::{self, Quat};
use crate::model::Model;
use crate::vrm::{ColliderShape, NO_NODE, SPRING_LIMIT};

type V3 = [f32; 3];

/// 1 ステップの Verlet(three-vrm の VRMSpringBoneJoint.update 相当)。
/// `rest_axis` は剛性が戻そうとする向き(ワールド、正規化済み)。戻り値は次の尾の位置(骨の長さに揃える前)。
#[allow(clippy::too_many_arguments)]
pub fn verlet(
    prev_tail: V3,
    curr_tail: V3,
    rest_axis: V3,
    stiffness: f32,
    drag: f32,
    gravity: V3,
    dt: f32,
) -> V3 {
    let inertia = math::vec_scale(math::vec_sub(curr_tail, prev_tail), 1.0 - drag);
    let stiff = math::vec_scale(rest_axis, stiffness * dt);
    let ext = math::vec_scale(gravity, dt);
    math::vec_add(math::vec_add(curr_tail, inertia), math::vec_add(stiff, ext))
}

/// 頭(`head`)からの距離を `length` に揃える。
pub fn keep_length(head: V3, tail: V3, length: f32) -> V3 {
    let d = math::normalize(math::vec_sub(tail, head));
    math::vec_add(head, math::vec_scale(d, length))
}

/// 球(中心 `c`、半径 `r`)に入り込んだ点を、表面まで押し出す。
pub fn push_out_sphere(p: V3, c: V3, r: f32) -> V3 {
    let d = math::vec_sub(p, c);
    let l = math::vec_len(d);
    if l >= r {
        return p;
    }
    // 中心にちょうどあるときは上へ(向きが決まらない)
    let n = if l > 1e-9 {
        math::vec_scale(d, 1.0 / l)
    } else {
        [0.0, 1.0, 0.0]
    };
    math::vec_add(c, math::vec_scale(n, r))
}

/// カプセル(線分 a–b、半径 `r`)。線分上の最近点の球として押し出す。
pub fn push_out_capsule(p: V3, a: V3, b: V3, r: f32) -> V3 {
    let ab = math::vec_sub(b, a);
    let len2 = math::vec_dot(ab, ab);
    let t = if len2 > 1e-12 {
        (math::vec_dot(math::vec_sub(p, a), ab) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    push_out_sphere(p, math::vec_add(a, math::vec_scale(ab, t)), r)
}

/// 1 フレームに計算する「関節 × コライダー」の組の数の上限(全 spring の合計)。
/// 読み込みの上限(関節 1024 × コライダー 1024)のままだと最悪 100 万組を毎フレーム計算し、
/// 30fps の予算を食いつぶす。組 1 つは数十 ns なので、16384 組なら 1ms 前後に収まる。
/// ニケちゃんは髪 55 本・計 110 関節・コライダー 31 で、最悪(全関節が全コライダーに当たる)でも
/// 3410 組(上限の約 1/5)なので、実モデルは引っかからない。
/// 超えた spring は、その spring のコライダーを外して(衝突なしで揺らして)落ちないようにする。
pub const MAX_COLLISION_PAIRS: usize = 16384;

struct SJoint {
    node: usize,
    /// 子ノード(なければ仮の尾)
    child: Option<usize>,
    /// 初期のローカルな尾の位置(子ノードのローカル位置。なければ骨の向きに 0.07)
    child_pos: V3,
    bone_axis: V3,
    bone_length: f32,
    rest_rot: Quat,
    stiffness: f32,
    drag: f32,
    gravity: V3,
    hit_radius: f32,
    prev_tail: V3,
    curr_tail: V3,
}

pub struct SpringSim {
    /// (joints の範囲, コライダーの番号)
    springs: Vec<(std::ops::Range<usize>, Vec<usize>)>,
    joints: Vec<SJoint>,
    initialized: bool,
    /// `MAX_COLLISION_PAIRS` を超えて、コライダーを外した spring の数
    collision_disabled: usize,
}

impl SpringSim {
    pub fn new(model: &Model, _rig: &Rig) -> Self {
        let sb = &model.vrm.spring;
        let (mut springs, mut joints) = (Vec::new(), Vec::new());
        let (mut pairs, mut collision_disabled) = (0usize, 0usize);
        for sp in &sb.springs {
            let start = joints.len();
            for (k, j) in sp.joints.iter().enumerate() {
                if j.node >= model.nodes.len() {
                    continue;
                }
                let rest = model.nodes[j.node].rest;
                let child =
                    sp.joints.get(k + 1).map(|c| c.node).filter(|&c| {
                        c < model.nodes.len() && model.nodes[c].parent == Some(j.node)
                    });
                let child_pos = match child {
                    Some(c) => model.nodes[c].rest.t,
                    None => math::vec_scale(math::normalize(rest.t), 0.07),
                };
                joints.push(SJoint {
                    node: j.node,
                    child,
                    child_pos,
                    bone_axis: math::normalize(child_pos),
                    bone_length: math::vec_len(child_pos),
                    rest_rot: rest.r,
                    stiffness: j.stiffness,
                    drag: j.drag_force.clamp(0.0, 1.0),
                    gravity: math::vec_scale(math::normalize(j.gravity_dir), j.gravity_power),
                    hit_radius: j.hit_radius,
                    prev_tail: [0.0; 3],
                    curr_tail: [0.0; 3],
                });
            }
            if joints.len() > start {
                // グループの展開で同じコライダーが重なっても、1 つにまとめる
                let mut seen = std::collections::BTreeSet::new();
                let mut cols = Vec::new();
                for &g in sp
                    .collider_groups
                    .iter()
                    .filter(|&&g| g < sb.collider_groups.len())
                {
                    for &c in &sb.collider_groups[g] {
                        if cols.len() < SPRING_LIMIT && seen.insert(c) {
                            cols.push(c);
                        }
                    }
                }
                let n = (joints.len() - start).saturating_mul(cols.len());
                if pairs.saturating_add(n) > MAX_COLLISION_PAIRS {
                    cols.clear();
                    collision_disabled += 1;
                } else {
                    pairs += n;
                }
                springs.push((start..joints.len(), cols));
            }
        }
        if collision_disabled > 0 {
            eprintln!(
                "SpringBone: 関節 × コライダーが上限({MAX_COLLISION_PAIRS})を超えるので、\
                 {collision_disabled} 本の spring の衝突を無効にする"
            );
        }
        Self {
            springs,
            joints,
            initialized: false,
            collision_disabled,
        }
    }

    pub fn collision_disabled_springs(&self) -> usize {
        self.collision_disabled
    }

    pub fn joint_count(&self) -> usize {
        self.joints.len()
    }

    /// 1 フレーム進める。`rig.world` は spring 前の評価(ポーズ・待機モーション込み)。
    /// 揺れる骨の `override_rot` を設定し、その骨と子のワールドを更新する。
    pub fn step(&mut self, model: &Model, rig: &mut Rig, dt: f32) {
        let dt = dt.clamp(0.0, 1.0 / 20.0);
        let sb = &model.vrm.spring;
        let pos = |w: &math::Mat4| [w[3][0], w[3][1], w[3][2]];
        if !self.initialized {
            // 最初のフレームは、今の姿勢の尾の位置に止める
            for j in &mut self.joints {
                let tail = math::transform_point(&rig.world[j.node], j.child_pos);
                j.prev_tail = tail;
                j.curr_tail = tail;
            }
            self.initialized = true;
        }
        for (range, cols) in &self.springs {
            for jx in range.clone() {
                let j = &mut self.joints[jx];
                let parent = model.nodes[j.node].parent;
                let parent_rot = parent.map_or(math::QUAT_IDENTITY, |p| rig.world_rot[p]);
                // 親の変形(前の骨の回転)を反映して、この骨の頭の位置を求める
                if let Some(p) = parent {
                    rig.world[j.node] =
                        math::mul(&rig.world[p], &model.nodes[j.node].rest.to_mat4());
                }
                let head = pos(&rig.world[j.node]);
                let rest_world_rot = math::quat_mul(parent_rot, j.rest_rot);
                let axis_w = math::quat_rotate(rest_world_rot, j.bone_axis);
                let mut next = verlet(
                    j.prev_tail,
                    j.curr_tail,
                    axis_w,
                    j.stiffness,
                    j.drag,
                    j.gravity,
                    dt,
                );
                next = keep_length(head, next, j.bone_length);
                for &ci in cols {
                    let c = &sb.colliders[ci];
                    // node のないコライダー(NO_NODE)や範囲外は無視する
                    if c.node == NO_NODE || c.node >= rig.world.len() {
                        continue;
                    }
                    let m = &rig.world[c.node];
                    next = match c.shape {
                        ColliderShape::Sphere { offset, radius } => push_out_sphere(
                            next,
                            math::transform_point(m, offset),
                            radius + j.hit_radius,
                        ),
                        ColliderShape::Capsule {
                            offset,
                            radius,
                            tail,
                        } => push_out_capsule(
                            next,
                            math::transform_point(m, offset),
                            math::transform_point(m, tail),
                            radius + j.hit_radius,
                        ),
                    };
                    next = keep_length(head, next, j.bone_length);
                }
                j.prev_tail = j.curr_tail;
                j.curr_tail = next;
                // 回転: 初期の骨の向き → 次の尾の向き(骨の初期ローカル系で見たもの)
                let to_local =
                    math::quat_rotate(math::quat_conj(rest_world_rot), math::vec_sub(next, head));
                let rot = math::quat_mul(j.rest_rot, math::quat_from_to(j.bone_axis, to_local));
                let rot = math::quat_normalize(rot);
                rig.override_rot[j.node] = Some(rot);
                // この骨と子のワールドを更新する(次の骨の頭の位置に効く)
                let mut l = rig.locals[j.node];
                l.r = rot;
                l.t = model.nodes[j.node].rest.t;
                rig.locals[j.node] = l;
                rig.world_rot[j.node] = math::quat_mul(parent_rot, rot);
                rig.world[j.node] = match parent {
                    Some(p) => math::mul(&rig.world[p], &l.to_mat4()),
                    None => l.to_mat4(),
                };
                if let Some(c) = j.child {
                    let cl = model.nodes[c].rest;
                    rig.world_rot[c] = math::quat_mul(rig.world_rot[j.node], cl.r);
                    rig.world[c] = math::mul(&rig.world[j.node], &cl.to_mat4());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verlet_with_gravity_pulls_tail_down_and_keeps_length() {
        let head = [0.0, 0.0, 0.0];
        let tail = [0.0, 0.0, 0.1];
        // 剛性・ドラッグなし、重力だけ。尾は -Y 側へ寄り、骨の長さ(0.1)は保たれる
        let n = verlet(
            tail,
            tail,
            [0.0, 0.0, 1.0],
            0.0,
            0.0,
            [0.0, -10.0, 0.0],
            1.0 / 30.0,
        );
        assert!(n[1] < tail[1]);
        let k = keep_length(head, n, 0.1);
        assert!((math::vec_len(k) - 0.1).abs() < 1e-6);
        assert!(k[1] < 0.0 && k[2] > 0.0);
    }

    #[test]
    fn verlet_stiffness_restores_toward_rest_axis_and_drag_damps_inertia() {
        // 尾が +X に外れていて、戻る向きは +Z。剛性で Z が増える
        let tail = [0.1, 0.0, 0.0];
        let n = verlet(tail, tail, [0.0, 0.0, 1.0], 2.0, 0.5, [0.0; 3], 1.0 / 30.0);
        assert!(n[2] > 0.0);
        // 慣性: 前フレームから +X に動いていた分の (1 - drag) だけ進む
        let n = verlet(
            [0.0, 0.0, 0.0],
            [0.1, 0.0, 0.0],
            [0.0; 3],
            0.0,
            0.7,
            [0.0; 3],
            1.0 / 30.0,
        );
        assert!((n[0] - (0.1 + 0.1 * 0.3)).abs() < 1e-6, "{n:?}");
    }

    #[test]
    fn collider_pushes_tail_out() {
        // 球の内側の点は表面へ
        let p = push_out_sphere([0.05, 0.0, 0.0], [0.0; 3], 0.1);
        assert!((math::vec_len(p) - 0.1).abs() < 1e-6 && p[0] > 0.0);
        // 外側の点は動かない
        assert_eq!(
            push_out_sphere([0.2, 0.0, 0.0], [0.0; 3], 0.1),
            [0.2, 0.0, 0.0]
        );
        // カプセルは線分の最近点から
        let p = push_out_capsule([0.05, 0.5, 0.0], [0.0, 0.0, 0.0], [0.0, 1.0, 0.0], 0.1);
        assert!((p[0] - 0.1).abs() < 1e-6 && (p[1] - 0.5).abs() < 1e-6);
    }
}
