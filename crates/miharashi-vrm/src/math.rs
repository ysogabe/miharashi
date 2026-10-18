//! 4x4 行列(列優先: `m[列][行]`)と、モデルの読み込み・カメラで使う最小限のベクトル計算。
pub type Mat4 = [[f32; 4]; 4];

pub const IDENTITY: Mat4 = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

/// `a * b`(b を先に適用する)。
pub fn mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut r = [[0.0; 4]; 4];
    for (c, col) in r.iter_mut().enumerate() {
        for (row, v) in col.iter_mut().enumerate() {
            *v = (0..4).map(|k| a[k][row] * b[c][k]).sum();
        }
    }
    r
}

pub fn translate(x: f32, y: f32, z: f32) -> Mat4 {
    let mut m = IDENTITY;
    m[3] = [x, y, z, 1.0];
    m
}

/// Y 軸まわりの回転(ラジアン)。
pub fn rotate_y(a: f32) -> Mat4 {
    let (s, c) = a.sin_cos();
    let mut m = IDENTITY;
    m[0][0] = c;
    m[0][2] = -s;
    m[2][0] = s;
    m[2][2] = c;
    m
}

/// OpenGL 流の透視投影(NDC の z は -1..1)。
pub fn perspective(fovy: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
    let f = 1.0 / (fovy * 0.5).tan();
    let mut m = [[0.0; 4]; 4];
    m[0][0] = f / aspect;
    m[1][1] = f;
    m[2][2] = (far + near) / (near - far);
    m[2][3] = -1.0;
    m[3][2] = 2.0 * far * near / (near - far);
    m
}

pub fn add_scaled(acc: &mut Mat4, m: &Mat4, s: f32) {
    for c in 0..4 {
        for r in 0..4 {
            acc[c][r] += m[c][r] * s;
        }
    }
}

pub fn transform_point(m: &Mat4, p: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|r| m[0][r] * p[0] + m[1][r] * p[1] + m[2][r] * p[2] + m[3][r])
}

/// 線形部分だけを掛ける(位置の差分ベクトル用)。
pub fn transform_dir(m: &Mat4, v: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|r| m[0][r] * v[0] + m[1][r] * v[1] + m[2][r] * v[2])
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub fn normalize(v: [f32; 3]) -> [f32; 3] {
    let l = dot(v, v).sqrt();
    if l > 1e-12 {
        [v[0] / l, v[1] / l, v[2] / l]
    } else {
        [0.0, 1.0, 0.0]
    }
}

/// 法線用の行列(線形部分の逆転置、向きは行列式の符号で補う。長さは `normalize` で整える)。
pub fn normal_matrix(m: &Mat4) -> Mat4 {
    let c = [
        [m[0][0], m[0][1], m[0][2]],
        [m[1][0], m[1][1], m[1][2]],
        [m[2][0], m[2][1], m[2][2]],
    ];
    let s = if dot(c[0], cross(c[1], c[2])) < 0.0 {
        -1.0
    } else {
        1.0
    };
    let mut r = IDENTITY;
    for (i, col) in [cross(c[1], c[2]), cross(c[2], c[0]), cross(c[0], c[1])]
        .into_iter()
        .enumerate()
    {
        r[i] = [col[0] * s, col[1] * s, col[2] * s, 0.0];
    }
    r
}

/// クォータニオン `[x, y, z, w]`(glTF と同じ並び)。
pub type Quat = [f32; 4];
pub const QUAT_IDENTITY: Quat = [0.0, 0.0, 0.0, 1.0];

/// `a * b`(b を先に適用する)。
pub fn quat_mul(a: Quat, b: Quat) -> Quat {
    [
        a[3] * b[0] + a[0] * b[3] + a[1] * b[2] - a[2] * b[1],
        a[3] * b[1] - a[0] * b[2] + a[1] * b[3] + a[2] * b[0],
        a[3] * b[2] + a[0] * b[1] - a[1] * b[0] + a[2] * b[3],
        a[3] * b[3] - a[0] * b[0] - a[1] * b[1] - a[2] * b[2],
    ]
}

pub fn quat_conj(q: Quat) -> Quat {
    [-q[0], -q[1], -q[2], q[3]]
}

pub fn quat_normalize(q: Quat) -> Quat {
    let l = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    if l > 1e-12 {
        [q[0] / l, q[1] / l, q[2] / l, q[3] / l]
    } else {
        QUAT_IDENTITY
    }
}

/// 軸(正規化される)まわりに `angle` ラジアン回す。
pub fn quat_axis_angle(axis: [f32; 3], angle: f32) -> Quat {
    let a = normalize(axis);
    let (s, c) = (angle * 0.5).sin_cos();
    [a[0] * s, a[1] * s, a[2] * s, c]
}

/// ベクトル `v` を回す。
pub fn quat_rotate(q: Quat, v: [f32; 3]) -> [f32; 3] {
    let u = [q[0], q[1], q[2]];
    let t = cross(u, v).map(|x| x * 2.0);
    let c = cross(u, t);
    [
        v[0] + q[3] * t[0] + c[0],
        v[1] + q[3] * t[1] + c[1],
        v[2] + q[3] * t[2] + c[2],
    ]
}

/// `from` を `to` に重ねる最短の回転(どちらも正規化される)。
pub fn quat_from_to(from: [f32; 3], to: [f32; 3]) -> Quat {
    let (a, b) = (normalize(from), normalize(to));
    let d = dot(a, b);
    if d < -0.999_999 {
        // 逆向き: a に垂直な任意の軸で 180 度
        let axis = if a[0].abs() < 0.9 {
            cross(a, [1.0, 0.0, 0.0])
        } else {
            cross(a, [0.0, 1.0, 0.0])
        };
        return quat_axis_angle(axis, std::f32::consts::PI);
    }
    let c = cross(a, b);
    quat_normalize([c[0], c[1], c[2], 1.0 + d])
}

pub fn quat_to_mat4(q: Quat) -> Mat4 {
    let [x, y, z, w] = q;
    let mut m = IDENTITY;
    m[0] = [
        1.0 - 2.0 * (y * y + z * z),
        2.0 * (x * y + z * w),
        2.0 * (x * z - y * w),
        0.0,
    ];
    m[1] = [
        2.0 * (x * y - z * w),
        1.0 - 2.0 * (x * x + z * z),
        2.0 * (y * z + x * w),
        0.0,
    ];
    m[2] = [
        2.0 * (x * z + y * w),
        2.0 * (y * z - x * w),
        1.0 - 2.0 * (x * x + y * y),
        0.0,
    ];
    m
}

/// ノードのローカル変換 `T * R * S`。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Trs {
    pub t: [f32; 3],
    pub r: Quat,
    pub s: [f32; 3],
}

impl Trs {
    pub fn to_mat4(&self) -> Mat4 {
        let mut m = quat_to_mat4(self.r);
        for (c, s) in m.iter_mut().zip(self.s) {
            for v in c.iter_mut().take(3) {
                *v *= s;
            }
        }
        m[3] = [self.t[0], self.t[1], self.t[2], 1.0];
        m
    }
}

pub fn vec_add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

pub fn vec_sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub fn vec_scale(a: [f32; 3], s: f32) -> [f32; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}

pub fn vec_len(a: [f32; 3]) -> f32 {
    dot(a, a).sqrt()
}

pub fn vec_dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    dot(a, b)
}

pub fn vec_cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    cross(a, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translate_then_point() {
        let m = mul(&translate(1.0, 2.0, 3.0), &translate(0.5, 0.0, 0.0));
        assert_eq!(transform_point(&m, [0.0; 3]), [1.5, 2.0, 3.0]);
        assert_eq!(transform_dir(&m, [1.0, 0.0, 0.0]), [1.0, 0.0, 0.0]);
    }

    #[test]
    fn normal_matrix_of_nonuniform_scale() {
        let mut m = IDENTITY;
        m[0][0] = 2.0; // x を 2 倍に伸ばすと、斜めの面の法線は x 方向が小さくなる
        let n = normalize(transform_dir(&normal_matrix(&m), [1.0, 1.0, 0.0]));
        assert!(n[0] < n[1]);
    }

    #[test]
    fn quat_rotates_and_composes() {
        let q = quat_axis_angle([0.0, 0.0, 1.0], std::f32::consts::FRAC_PI_2);
        let v = quat_rotate(q, [1.0, 0.0, 0.0]);
        assert!((v[0]).abs() < 1e-6 && (v[1] - 1.0).abs() < 1e-6);
        // 行列にしても同じ向きに回る
        let m = transform_dir(&quat_to_mat4(q), [1.0, 0.0, 0.0]);
        assert!((m[1] - 1.0).abs() < 1e-6);
        // q * conj(q) = 単位
        let i = quat_mul(q, quat_conj(q));
        assert!(i[0].abs() < 1e-6 && (i[3] - 1.0).abs() < 1e-6);
        // from_to
        let r = quat_from_to([1.0, 0.0, 0.0], [0.0, -1.0, 0.0]);
        let v = quat_rotate(r, [1.0, 0.0, 0.0]);
        assert!((v[1] + 1.0).abs() < 1e-5, "{v:?}");
        let r = quat_from_to([0.0, 1.0, 0.0], [0.0, -1.0, 0.0]);
        let v = quat_rotate(r, [0.0, 1.0, 0.0]);
        assert!((v[1] + 1.0).abs() < 1e-5, "{v:?}");
    }

    #[test]
    fn trs_matrix_is_t_r_s() {
        let t = Trs {
            t: [1.0, 2.0, 3.0],
            r: quat_axis_angle([0.0, 0.0, 1.0], std::f32::consts::FRAC_PI_2),
            s: [2.0, 1.0, 1.0],
        };
        // x 方向の点: S で 2 倍 → R で y 方向 → T で平行移動
        let p = transform_point(&t.to_mat4(), [1.0, 0.0, 0.0]);
        assert!(
            (p[0] - 1.0).abs() < 1e-5 && (p[1] - 4.0).abs() < 1e-5 && (p[2] - 3.0).abs() < 1e-5
        );
    }
}
