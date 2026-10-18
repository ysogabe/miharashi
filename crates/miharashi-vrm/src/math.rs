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
}
