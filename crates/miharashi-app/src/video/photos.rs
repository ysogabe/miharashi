//! 動画の見本に使う写真。プログラムで決まった絵を作る合成の風景で、実在の写真・外部の画像は使わない。
//! 乱数は使わず(整数のハッシュだけ)、同じ番号なら毎回同じ画素になる。
use image::{DynamicImage, RgbImage};

/// 見本の写真 1 枚。撮影日は動画の「今日」(2026-10-16)から数えた経過の見本になる。
pub struct Scene {
    pub caption: &'static str,
    pub taken: &'static str,
    size: (u32, u32),
    spec: Spec,
}

type Rgb = [f32; 3];

/// 山並み・丘 1 層。`base` は尾根の平均の高さ(画像の上端 0、下端 1)、`amp` は起伏、`freq` は横方向の細かさ。
struct Layer {
    base: f32,
    amp: f32,
    freq: f32,
    color: Rgb,
    /// この高さ(0〜1)より上は雪の色にする
    snow: Option<f32>,
}

enum Ground {
    /// 水面(地平線で空と山並みを映す)
    Water(Rgb),
    /// 一番手前の層が下端まで続く(丘・森)
    Layers,
}

struct Spec {
    /// 空の色: 上、中ほど、地平線
    sky: [Rgb; 3],
    horizon: f32,
    /// 太陽 (x, y, 半径, 色)
    sun: Option<(f32, f32, f32, Rgb)>,
    clouds: f32,
    layers: &'static [Layer],
    ground: Ground,
    seed: u32,
}

const fn c(r: u8, g: u8, b: u8) -> Rgb {
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0]
}

pub const SCENES: [Scene; 4] = [
    Scene {
        caption: "夕暮れの湖",
        taken: "2026-10-04 17:42",
        size: (1600, 1200),
        spec: Spec {
            sky: [c(58, 52, 110), c(224, 120, 110), c(255, 196, 120)],
            horizon: 0.62,
            sun: Some((0.68, 0.56, 0.05, c(255, 236, 190))),
            clouds: 0.5,
            layers: &[
                Layer {
                    base: 0.50,
                    amp: 0.07,
                    freq: 2.2,
                    color: c(122, 96, 140),
                    snow: None,
                },
                Layer {
                    base: 0.56,
                    amp: 0.07,
                    freq: 3.4,
                    color: c(70, 58, 104),
                    snow: None,
                },
            ],
            ground: Ground::Water(c(40, 52, 96)),
            seed: 11,
        },
    },
    Scene {
        caption: "朝の高原",
        taken: "2026-09-27 07:15",
        size: (1800, 1200),
        spec: Spec {
            sky: [c(70, 130, 215), c(140, 192, 238), c(214, 232, 246)],
            horizon: 0.58,
            sun: Some((0.22, 0.2, 0.04, c(255, 250, 225))),
            clouds: 0.8,
            layers: &[
                Layer {
                    base: 0.52,
                    amp: 0.08,
                    freq: 1.6,
                    color: c(120, 150, 170),
                    snow: None,
                },
                Layer {
                    base: 0.64,
                    amp: 0.06,
                    freq: 2.4,
                    color: c(86, 150, 90),
                    snow: None,
                },
                Layer {
                    base: 0.78,
                    amp: 0.07,
                    freq: 1.8,
                    color: c(56, 128, 66),
                    snow: None,
                },
            ],
            ground: Ground::Layers,
            seed: 23,
        },
    },
    Scene {
        caption: "雪をいただく山",
        taken: "2026-10-11 10:30",
        size: (1200, 1600),
        spec: Spec {
            sky: [c(86, 138, 210), c(150, 190, 232), c(218, 232, 245)],
            horizon: 0.74,
            sun: None,
            clouds: 0.55,
            layers: &[
                Layer {
                    base: 0.34,
                    amp: 0.20,
                    freq: 1.1,
                    color: c(110, 120, 150),
                    snow: Some(0.30),
                },
                Layer {
                    base: 0.62,
                    amp: 0.07,
                    freq: 2.6,
                    color: c(58, 98, 82),
                    snow: None,
                },
                Layer {
                    base: 0.80,
                    amp: 0.05,
                    freq: 3.0,
                    color: c(30, 70, 56),
                    snow: None,
                },
            ],
            ground: Ground::Layers,
            seed: 37,
        },
    },
    Scene {
        caption: "海辺の夜明け",
        taken: "2026-10-09 05:48",
        size: (1600, 1200),
        spec: Spec {
            sky: [c(44, 70, 140), c(240, 150, 150), c(255, 214, 140)],
            horizon: 0.55,
            sun: Some((0.5, 0.55, 0.06, c(255, 240, 200))),
            clouds: 0.35,
            layers: &[Layer {
                base: 0.53,
                amp: 0.015,
                freq: 5.0,
                color: c(90, 80, 120),
                snow: None,
            }],
            ground: Ground::Water(c(52, 74, 130)),
            seed: 53,
        },
    },
];

fn hash(x: i32, y: i32, seed: u32) -> f32 {
    let mut h = (x as u32)
        .wrapping_mul(0x85eb_ca6b)
        .wrapping_add((y as u32).wrapping_mul(0xc2b2_ae35))
        .wrapping_add(seed.wrapping_mul(0x27d4_eb2f));
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297a_2d39);
    h ^= h >> 15;
    (h & 0xffff) as f32 / 65535.0
}

fn smooth(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

fn noise2(x: f32, y: f32, seed: u32) -> f32 {
    let (xi, yi) = (x.floor(), y.floor());
    let (fx, fy) = (smooth(x - xi), smooth(y - yi));
    let (xi, yi) = (xi as i32, yi as i32);
    let a = hash(xi, yi, seed) * (1.0 - fx) + hash(xi + 1, yi, seed) * fx;
    let b = hash(xi, yi + 1, seed) * (1.0 - fx) + hash(xi + 1, yi + 1, seed) * fx;
    a * (1.0 - fy) + b * fy
}

fn fbm(x: f32, y: f32, seed: u32) -> f32 {
    let mut sum = 0.0;
    let (mut amp, mut freq, mut norm) = (1.0, 1.0, 0.0);
    for o in 0..4 {
        sum += amp * noise2(x * freq, y * freq, seed.wrapping_add(o * 101));
        norm += amp;
        amp *= 0.5;
        freq *= 2.0;
    }
    sum / norm
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

/// 空と山並み・丘(水面や地面を除く)。`(x, y)` は 0〜1、`aspect` は幅 / 高さ。
fn above(spec: &Spec, x: f32, y: f32, aspect: f32) -> Rgb {
    let hz = spec.horizon;
    let t = (y / hz).clamp(0.0, 1.0);
    let mut col = if t < 0.55 {
        mix(spec.sky[0], spec.sky[1], t / 0.55)
    } else {
        mix(spec.sky[1], spec.sky[2], (t - 0.55) / 0.45)
    };
    if let Some((sx, sy, r, sc)) = spec.sun {
        let (dx, dy) = ((x - sx) * aspect, y - sy);
        let d = (dx * dx + dy * dy).sqrt();
        col = mix(col, sc, (-(d / (r * 6.0)).powi(2)).exp() * 0.55);
        if d < r {
            col = sc;
        }
    }
    // 雲: 横に長いノイズを閾値で切り、下側を少し暗くする
    let n = fbm(x * aspect * 2.4, y * 7.0, spec.seed);
    let cloud = ((n - (0.78 - spec.clouds * 0.3)) * 6.0).clamp(0.0, 1.0) * (1.0 - t).min(1.0);
    if cloud > 0.0 {
        let shade = mix([1.0, 1.0, 1.0], spec.sky[2], 0.35);
        col = mix(col, shade, cloud * 0.8);
    }
    for (i, l) in spec.layers.iter().enumerate() {
        let r = l.base
            - l.amp
                * fbm(
                    x * l.freq * aspect,
                    0.5,
                    spec.seed.wrapping_add(7 + i as u32 * 13),
                );
        if y > r {
            let mut lc = l.color;
            if let Some(line) = l.snow
                && y < line + 0.12 * fbm(x * 9.0 * aspect, y * 9.0, spec.seed + 5)
            {
                lc = mix(lc, [0.96, 0.97, 1.0], 0.9);
            }
            // 遠い層ほど空の色に溶けさせる
            let haze = 0.35 * (1.0 - i as f32 / spec.layers.len() as f32);
            col = mix(lc, spec.sky[2], haze);
        }
    }
    col
}

fn pixel(spec: &Spec, x: f32, y: f32, aspect: f32) -> Rgb {
    match &spec.ground {
        Ground::Water(tint) if y > spec.horizon => {
            // 地平線で折り返した絵を、横にゆらして暗くする
            let depth = (y - spec.horizon) / (1.0 - spec.horizon);
            let ripple =
                (noise2(x * aspect * 14.0, y * 90.0, spec.seed + 3) - 0.5) * 0.02 * (0.3 + depth);
            let mirrored = above(spec, x + ripple, 2.0 * spec.horizon - y, aspect);
            let streak = noise2(x * aspect * 5.0, y * 160.0, spec.seed + 9);
            let base = mix(mirrored, *tint, 0.35 + 0.4 * depth);
            mix(
                base,
                [1.0, 1.0, 1.0],
                (streak - 0.62).max(0.0) * 0.25 * (1.0 - depth),
            )
        }
        Ground::Water(_) => above(spec, x, y.min(spec.horizon), aspect),
        Ground::Layers => above(spec, x, y, aspect),
    }
}

/// 見本の写真 `i`(`SCENES` の番号)。
pub fn scene(i: usize) -> DynamicImage {
    let s = &SCENES[i % SCENES.len()];
    let (w, h) = s.size;
    let aspect = w as f32 / h as f32;
    let mut img = RgbImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = ((x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32);
            let col = pixel(&s.spec, fx, fy, aspect);
            // 粒子感を少し(ハッシュなので決まった値)
            let g = (hash(x as i32, y as i32, 99) - 0.5) * 0.02;
            img.put_pixel(
                x,
                y,
                image::Rgb([0, 1, 2].map(|k| ((col[k] + g).clamp(0.0, 1.0) * 255.0 + 0.5) as u8)),
            );
        }
    }
    DynamicImage::ImageRgb8(img)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 同じ番号なら同じ画素。写真ごとに絵が違う。
    #[test]
    fn scenes_are_deterministic_and_distinct() {
        let a: Vec<_> = (0..SCENES.len()).map(|i| scene(i).to_rgb8()).collect();
        let b: Vec<_> = (0..SCENES.len()).map(|i| scene(i).to_rgb8()).collect();
        assert!(a == b);
        for i in 0..a.len() {
            for j in i + 1..a.len() {
                assert!(a[i].dimensions() != a[j].dimensions() || a[i] != a[j]);
            }
        }
    }
}
