//! 試験・計測用に、コードから小さな VRM 1.0 の glb を組み立てる(実モデルは利用条件の都合で
//! リポジトリに置かない)。頭(テクスチャ + morph target `blink`)・胴・スキン付きの三角形を持つ。
use serde_json::{Value, json};

pub struct Opts {
    /// 球の経線数(三角形数は頭と胴で約 2 × segments × segments)
    pub segments: u32,
    /// 頭のテクスチャの 1 辺(画素)
    pub texture_size: u32,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            segments: 48,
            texture_size: 1024,
        }
    }
}

#[derive(Default)]
struct Bin {
    data: Vec<u8>,
    views: Vec<Value>,
    accessors: Vec<Value>,
}

impl Bin {
    fn view(&mut self, bytes: &[u8], target: Option<u32>) -> usize {
        while !self.data.len().is_multiple_of(4) {
            self.data.push(0);
        }
        let mut v = json!({"buffer": 0, "byteOffset": self.data.len(), "byteLength": bytes.len()});
        if let Some(t) = target {
            v["target"] = json!(t);
        }
        self.data.extend_from_slice(bytes);
        self.views.push(v);
        self.views.len() - 1
    }

    fn accessor(&mut self, bytes: &[u8], target: u32, ty: &str, ctype: u32, count: usize) -> usize {
        let view = self.view(bytes, Some(target));
        self.accessors
            .push(json!({"bufferView": view, "componentType": ctype, "count": count, "type": ty}));
        self.accessors.len() - 1
    }

    fn vec3(&mut self, v: &[[f32; 3]]) -> usize {
        let bytes: Vec<u8> = v.iter().flatten().flat_map(|f| f.to_le_bytes()).collect();
        let a = self.accessor(&bytes, 34962, "VEC3", 5126, v.len());
        let min: Vec<f32> = (0..3)
            .map(|k| v.iter().map(|p| p[k]).fold(f32::MAX, f32::min))
            .collect();
        let max: Vec<f32> = (0..3)
            .map(|k| v.iter().map(|p| p[k]).fold(f32::MIN, f32::max))
            .collect();
        self.accessors[a]["min"] = json!(min);
        self.accessors[a]["max"] = json!(max);
        a
    }

    fn vec2(&mut self, v: &[[f32; 2]]) -> usize {
        let bytes: Vec<u8> = v.iter().flatten().flat_map(|f| f.to_le_bytes()).collect();
        self.accessor(&bytes, 34962, "VEC2", 5126, v.len())
    }

    fn indices(&mut self, v: &[u32]) -> usize {
        let bytes: Vec<u8> = v.iter().flat_map(|f| f.to_le_bytes()).collect();
        self.accessor(&bytes, 34963, "SCALAR", 5125, v.len())
    }

    fn joints(&mut self, n: usize) -> usize {
        let bytes: Vec<u8> = (0..n)
            .flat_map(|_| [0u16; 4])
            .flat_map(u16::to_le_bytes)
            .collect();
        self.accessor(&bytes, 34962, "VEC4", 5123, n)
    }

    fn weights(&mut self, n: usize) -> usize {
        let bytes: Vec<u8> = (0..n)
            .flat_map(|_| [1.0f32, 0.0, 0.0, 0.0])
            .flat_map(f32::to_le_bytes)
            .collect();
        self.accessor(&bytes, 34962, "VEC4", 5126, n)
    }

    fn mat4(&mut self, m: &[f32; 16]) -> usize {
        let bytes: Vec<u8> = m.iter().flat_map(|f| f.to_le_bytes()).collect();
        let view = self.view(&bytes, None);
        self.accessors
            .push(json!({"bufferView": view, "componentType": 5126, "count": 1, "type": "MAT4"}));
        self.accessors.len() - 1
    }
}

struct Mesh {
    pos: Vec<[f32; 3]>,
    nrm: Vec<[f32; 3]>,
    uv: Vec<[f32; 2]>,
    idx: Vec<u32>,
}

/// 楕円体(外向きが表の CCW)。
fn ellipsoid(segs: u32, c: [f32; 3], r: [f32; 3]) -> Mesh {
    let rings = (segs / 2).max(2);
    let mut m = Mesh {
        pos: vec![],
        nrm: vec![],
        uv: vec![],
        idx: vec![],
    };
    for i in 0..=rings {
        let th = std::f32::consts::PI * i as f32 / rings as f32;
        for j in 0..=segs {
            let ph = std::f32::consts::TAU * j as f32 / segs as f32;
            let u = [th.sin() * ph.cos(), th.cos(), th.sin() * ph.sin()];
            m.pos
                .push([c[0] + r[0] * u[0], c[1] + r[1] * u[1], c[2] + r[2] * u[2]]);
            // 楕円体の法線は (u / r) の向き
            let n = [u[0] / r[0], u[1] / r[1], u[2] / r[2]];
            let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            m.nrm.push([n[0] / l, n[1] / l, n[2] / l]);
            m.uv.push([j as f32 / segs as f32, i as f32 / rings as f32]);
        }
    }
    let at = |i: u32, j: u32| i * (segs + 1) + j;
    for i in 0..rings {
        for j in 0..segs {
            let (a, b, c2, d) = (at(i, j), at(i, j + 1), at(i + 1, j), at(i + 1, j + 1));
            m.idx.extend_from_slice(&[a, b, c2, b, d, c2]);
        }
    }
    m
}

/// 縞と格子の模様を PNG にしたもの(向きとミップマップの確認用)。
fn texture_png(size: u32) -> Vec<u8> {
    let img = image::RgbaImage::from_fn(size, size, |x, y| {
        let (fx, fy) = (x as f32 / size as f32, y as f32 / size as f32);
        let grid = ((x * 8 / size + y * 8 / size) % 2) as u8;
        image::Rgba([
            (fx * 255.0) as u8,
            (fy * 200.0) as u8 + 30,
            120 + grid * 100,
            255,
        ])
    });
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .expect("PNG に書ける");
    out
}

/// 頭(node 1)・胴(node 2)・スキン付き三角形(node 3、関節 node 4)を持つ VRM 1.0 の glb。
/// root(node 0)は y 方向に +0.5 動かしてある。スキンの関節は x 方向に +0.5。
pub fn build_vrm_glb(o: &Opts) -> Vec<u8> {
    let (doc, bin) = build_vrm_doc(o);
    pack_glb(&doc, &bin)
}

/// `build_vrm_glb` の中身(glTF の JSON と BIN)。壊れた glb を作る試験が JSON を書き換えるのに使う。
pub(crate) fn build_vrm_doc(o: &Opts) -> (Value, Vec<u8>) {
    let head = ellipsoid(o.segments, [0.0, 1.0, 0.0], [0.15, 0.17, 0.15]);
    let body = ellipsoid(o.segments, [0.0, 0.5, 0.0], [0.2, 0.4, 0.12]);
    // blink: 頭を縦につぶす(morph target の差分)
    let squash: Vec<[f32; 3]> = head
        .pos
        .iter()
        .map(|p| [0.0, -0.6 * (p[1] - 1.0), 0.0])
        .collect();
    let zero = vec![[0.0f32; 3]; head.pos.len()];
    let tri = (
        [[-0.1f32, 1.0, 0.0], [0.1, 1.0, 0.0], [0.0, 1.2, 0.0]],
        [0.0f32, 0.0, 1.0],
    );

    let mut b = Bin::default();
    let (hp, hn, hu, hi) = (
        b.vec3(&head.pos),
        b.vec3(&head.nrm),
        b.vec2(&head.uv),
        b.indices(&head.idx),
    );
    let (ht, htn) = (b.vec3(&squash), b.vec3(&zero));
    let (bp, bn, bi) = (b.vec3(&body.pos), b.vec3(&body.nrm), b.indices(&body.idx));
    let (tp, tn) = (b.vec3(&tri.0), b.vec3(&[tri.1; 3]));
    let (tj, tw, ti) = (b.joints(3), b.weights(3), b.indices(&[0, 1, 2]));
    let ibm = b.mat4(&[
        1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ]);
    let png = texture_png(o.texture_size);
    let png_view = b.view(&png, None);

    let doc = json!({
        "asset": {"version": "2.0"},
        "extensionsUsed": ["VRMC_vrm"],
        "extensions": {"VRMC_vrm": {
            "specVersion": "1.0",
            "meta": {"name": "miharashi synthetic", "licenseUrl": "https://vrm.dev/licenses/1.0/", "authors": ["miharashi"]},
            "humanoid": {"humanBones": {}},
            "expressions": {"preset": {
                "blink": {"morphTargetBinds": [{"node": 1, "index": 0, "weight": 1.0}]},
                "happy": {}
            }}
        }},
        "scene": 0,
        "scenes": [{"nodes": [0, 3, 4]}],
        "nodes": [
            {"name": "root", "translation": [0.0, 0.5, 0.0], "children": [1, 2]},
            {"name": "Head", "mesh": 0},
            {"name": "Body", "mesh": 1},
            {"name": "Skinned", "mesh": 2, "skin": 0},
            {"name": "Joint", "translation": [0.5, 0.0, 0.0]}
        ],
        "skins": [{"joints": [4], "inverseBindMatrices": ibm}],
        "meshes": [
            {"primitives": [{"attributes": {"POSITION": hp, "NORMAL": hn, "TEXCOORD_0": hu},
                             "indices": hi, "material": 0,
                             "targets": [{"POSITION": ht, "NORMAL": htn}]}],
             "extras": {"targetNames": ["blink"]}},
            {"primitives": [{"attributes": {"POSITION": bp, "NORMAL": bn}, "indices": bi, "material": 1}]},
            {"primitives": [{"attributes": {"POSITION": tp, "NORMAL": tn, "JOINTS_0": tj, "WEIGHTS_0": tw},
                             "indices": ti, "material": 2}]}
        ],
        "materials": [
            {"name": "face", "pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}},
            {"name": "cloth", "pbrMetallicRoughness": {"baseColorFactor": [0.8, 0.3, 0.35, 1.0]}},
            {"name": "mask", "alphaMode": "MASK", "alphaCutoff": 0.4, "doubleSided": true,
             "pbrMetallicRoughness": {"baseColorFactor": [0.3, 0.8, 0.4, 1.0]}}
        ],
        "textures": [{"source": 0, "sampler": 0}],
        "samplers": [{"wrapS": 33071, "wrapT": 33071}],
        "images": [{"bufferView": png_view, "mimeType": "image/png"}],
        "accessors": b.accessors,
        "bufferViews": b.views,
        "buffers": [{"byteLength": b.data.len()}]
    });
    (doc, b.data)
}

pub(crate) fn pack_glb(doc: &Value, bin: &[u8]) -> Vec<u8> {
    let mut json = serde_json::to_vec(doc).expect("JSON にできる");
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    let mut bin = bin.to_vec();
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    let total = 12 + 8 + json.len() + 8 + bin.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&json);
    out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(&bin);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AlphaMode, FLOATS_PER_VERTEX, Model};
    use crate::render::{Renderer, attach_depth, detach_depth};

    fn small() -> Model {
        Model::from_glb(&build_vrm_glb(&Opts {
            segments: 12,
            texture_size: 64,
        }))
        .unwrap()
    }

    #[test]
    fn loads_synthetic_vrm() {
        let m = small();
        let s = m.stats();
        assert_eq!((s.meshes, s.primitives, s.morph_targets), (3, 3, 1));
        assert_eq!(s.images, [(64, 64)]);
        assert_eq!(s.materials, 3);
        // 球 2 つ(rings = 6、経線 12 → 6 × 12 × 2 面)+ 三角形 1
        assert_eq!(s.triangles, 2 * 6 * 12 * 2 + 1);
        assert_eq!(m.vrm.spec_version, "1.0");
        assert_eq!(m.vrm.default_expression(), Some("blink"));
        assert_eq!(m.materials[2].alpha_mode, AlphaMode::Mask);
        assert_eq!(m.materials[2].alpha_cutoff, 0.4);
        assert!(m.materials[2].double_sided);
        assert_eq!(m.textures[0].wrap_s, glow::CLAMP_TO_EDGE);
    }

    #[test]
    fn bakes_node_and_skin_transforms() {
        let m = small();
        // root の +0.5 が頭に効いて、頭の中心は y = 1.5
        let head = &m.primitives[0];
        let ys: Vec<f32> = head
            .vertices
            .as_chunks::<FLOATS_PER_VERTEX>()
            .0
            .iter()
            .map(|v| v[1])
            .collect();
        let (lo, hi) = (
            ys.iter().cloned().fold(f32::MAX, f32::min),
            ys.iter().cloned().fold(f32::MIN, f32::max),
        );
        assert!(
            (lo - 1.33).abs() < 1e-4 && (hi - 1.67).abs() < 1e-4,
            "{lo} {hi}"
        );
        // スキンの三角形は関節の +0.5(x)だけ動く(自身のノードの変換は使わない)
        let tri = &m.primitives[2];
        assert!((tri.vertices[0] - 0.4).abs() < 1e-5 && (tri.vertices[1] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn expression_resolves_to_morph_weights() {
        let mut m = small();
        // GL なしで weight の解決と合成を確かめるため、Renderer を介さず同じ計算をする
        let e = m.vrm.expression("blink").unwrap().clone();
        assert_eq!((e.binds[0].node, e.binds[0].index), (1, 0));
        let head = &mut m.primitives[0];
        let top = (0..head.vertices.len() / FLOATS_PER_VERTEX)
            .max_by(|&a, &b| head.vertices[a * 8 + 1].total_cmp(&head.vertices[b * 8 + 1]))
            .unwrap();
        // blink = 頭の頂点が 0.6 × (中心からの y) だけ下がる
        let d = head.targets[0].positions[top];
        assert!((d[1] + 0.6 * 0.17).abs() < 1e-5, "{d:?}");
    }

    #[test]
    fn rejects_vrm0_glb() {
        let mut doc = serde_json::json!({"asset": {"version": "2.0"}, "extensions": {"VRM": {"specVersion": "0.0"}}});
        doc["scenes"] = serde_json::json!([]);
        let e = Model::from_glb(&pack_glb(&doc, &[])).err().unwrap();
        assert!(e.contains("0.x") && e.contains("未対応"), "{e}");
    }

    /// render node と Mesa がある環境だけで動く。ない環境(CI など)では skip する。
    #[test]
    fn renders_and_morphs_on_gpu() {
        let node = miharashi_gpu::render_node_for_pci("0000:10:00.0");
        let off = match miharashi_gpu::Offscreen::open(&node) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("GLES が使えないので skip: {e}");
                return;
            }
        };
        let size = (320u32, 240u32);
        let (result, img) = off
            .with_fbo(size, |gl| -> Result<(), String> {
                let depth = attach_depth(gl, size)?;
                let mut r = Renderer::new(gl, small())?;
                r.update(gl);
                r.render(gl, size);
                // blink の前後で morph 後の頂点が変わること
                let before = r.morphed_vertices(0);
                r.set_expression("blink", 1.0)?;
                r.update(gl);
                let after = r.morphed_vertices(0);
                assert_ne!(before, after, "blink で頂点が動く");
                assert!(r.set_expression("nothing", 1.0).is_err());
                r.render(gl, size);
                unsafe {
                    use glow::HasContext;
                    assert_eq!(gl.get_error(), glow::NO_ERROR);
                }
                r.destroy(gl);
                detach_depth(gl, depth);
                unsafe {
                    use glow::HasContext;
                    assert_eq!(gl.get_error(), glow::NO_ERROR, "解放後に GL エラー");
                }
                Ok(())
            })
            .unwrap();
        result.unwrap();
        let nonbg = img
            .as_chunks::<4>()
            .0
            .iter()
            // 背景は (20, 23, 31)
            .filter(|p| p[0].abs_diff(20) > 10 || p[1].abs_diff(23) > 10 || p[2].abs_diff(31) > 10)
            .count();
        assert!(nonbg > 1000, "描けた画素が少ない: {nonbg}");
    }
}
