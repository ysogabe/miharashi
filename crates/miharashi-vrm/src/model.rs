//! VRM(glb)の読み込み。GL には触れない(CPU 側のデータだけを作る)。
//! 静止ポーズ: ノードの変換とスキニング(バインドポーズ)をここで頂点に焼き込むので、描画側は
//! 1 つの view-projection 行列だけで描ける。
use crate::math::{self, IDENTITY, Mat4};
use crate::vrm::{self, Vrm};
use gltf::accessor::{DataType, Dimensions};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlphaMode {
    Opaque,
    Mask,
    Blend,
}

#[derive(Clone, Debug)]
pub struct Material {
    pub base_color: [f32; 4],
    /// `Model::textures` の番号
    pub texture: Option<usize>,
    pub alpha_mode: AlphaMode,
    pub alpha_cutoff: f32,
    pub double_sided: bool,
}

pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// glTF の texture(画像 + サンプラーの wrap)。wrap は GL の定数。
pub struct Texture {
    pub image: usize,
    pub wrap_s: u32,
    pub wrap_t: u32,
}

/// 1 つの morph target の差分(ベース形状と同じ空間に変換済み)。
pub struct Target {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
}

/// 頂点は `[px, py, pz, nx, ny, nz, u, v]` の 8 float(インターリーブ)。
pub const FLOATS_PER_VERTEX: usize = 8;

pub struct Primitive {
    /// このプリミティブを持つノードの番号(expression の bind はノード単位)
    pub node: usize,
    pub vertices: Vec<f32>,
    pub indices: Vec<u32>,
    pub targets: Vec<Target>,
    pub material: usize,
}

pub struct Model {
    pub vrm: Vrm,
    pub primitives: Vec<Primitive>,
    pub materials: Vec<Material>,
    pub textures: Vec<Texture>,
    pub images: Vec<Image>,
    pub mesh_count: usize,
    /// バインドポーズでの外接箱 `(min, max)`
    pub bounds: ([f32; 3], [f32; 3]),
}

/// `vrm-bench` などが表示する、モデルの規模。
pub struct Stats {
    pub meshes: usize,
    pub primitives: usize,
    pub vertices: usize,
    pub triangles: usize,
    pub materials: usize,
    pub images: Vec<(u32, u32)>,
    pub morph_targets: usize,
}

impl Model {
    pub fn from_path(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::from_glb(&bytes)
    }

    pub fn from_glb(bytes: &[u8]) -> Result<Self, String> {
        let json = vrm::glb_json(bytes)?;
        let vrm = vrm::parse(&json)?;
        let patched = zero_fill_empty_accessors(bytes, &json);
        let (doc, buffers, images) = gltf::import_slice(patched.as_deref().unwrap_or(bytes))
            .map_err(|e| format!("glb を読めない: {e}"))?;
        let images = images
            .into_iter()
            .map(to_rgba)
            .collect::<Result<Vec<_>, _>>()?;
        let textures = doc
            .textures()
            .map(|t| Texture {
                image: t.source().index(),
                wrap_s: wrap(t.sampler().wrap_s()),
                wrap_t: wrap(t.sampler().wrap_t()),
            })
            .collect();
        let mut materials: Vec<Material> = doc
            .materials()
            .map(|m| {
                let pbr = m.pbr_metallic_roughness();
                Material {
                    base_color: pbr.base_color_factor(),
                    texture: pbr.base_color_texture().map(|i| i.texture().index()),
                    alpha_mode: match m.alpha_mode() {
                        gltf::material::AlphaMode::Opaque => AlphaMode::Opaque,
                        gltf::material::AlphaMode::Mask => AlphaMode::Mask,
                        gltf::material::AlphaMode::Blend => AlphaMode::Blend,
                    },
                    alpha_cutoff: m.alpha_cutoff().unwrap_or(0.5),
                    double_sided: m.double_sided(),
                }
            })
            .collect();
        // material のないプリミティブ用(glTF の既定: 白・不透明)
        let default_material = materials.len();
        materials.push(Material {
            base_color: [1.0; 4],
            texture: None,
            alpha_mode: AlphaMode::Opaque,
            alpha_cutoff: 0.5,
            double_sided: false,
        });

        // 各ノードのワールド変換(シーンの根から辿る)
        let mut world = vec![IDENTITY; doc.nodes().len()];
        let scene = doc
            .default_scene()
            .or_else(|| doc.scenes().next())
            .ok_or("glTF にシーンがない")?;
        let mut order = Vec::new();
        let mut visited = vec![false; world.len()];
        let mut stack: Vec<(gltf::Node, Mat4)> = scene.nodes().map(|n| (n, IDENTITY)).collect();
        while let Some((n, parent)) = stack.pop() {
            // 子の循環や、複数の親から同じノードに来る形(glTF では不正)は読まない
            if std::mem::replace(&mut visited[n.index()], true) {
                return Err(format!(
                    "ノード {} を複数回辿った(children が循環、または親が複数)",
                    n.index()
                ));
            }
            let w = math::mul(&parent, &n.transform().matrix());
            world[n.index()] = w;
            order.push(n.index());
            stack.extend(n.children().map(|c| (c, w)));
        }
        order.sort_unstable();

        let mut primitives = Vec::new();
        let mut meshes = std::collections::BTreeSet::new();
        for &ni in &order {
            let node = doc.nodes().nth(ni).ok_or("ノード番号が不正")?;
            let Some(mesh) = node.mesh() else { continue };
            meshes.insert(mesh.index());
            // スキンがあるメッシュはノードの変換を使わず、関節のワールド変換 × 逆バインド行列
            let skin = node.skin().map(|s| {
                let reader = s.reader(|b| Some(&buffers[b.index()]));
                let ibm: Vec<Mat4> = reader
                    .read_inverse_bind_matrices()
                    .map(|i| i.collect())
                    .unwrap_or_default();
                s.joints()
                    .enumerate()
                    .map(|(k, j)| math::mul(&world[j.index()], ibm.get(k).unwrap_or(&IDENTITY)))
                    .collect::<Vec<Mat4>>()
            });
            for prim in mesh.primitives() {
                if prim.mode() != gltf::mesh::Mode::Triangles {
                    continue;
                }
                let material = prim.material().index().unwrap_or(default_material);
                primitives.push(read_primitive(
                    &prim,
                    &buffers,
                    ni,
                    material,
                    &world[ni],
                    skin.as_deref(),
                )?);
            }
        }
        if primitives.is_empty() {
            return Err("描画できる三角形メッシュがない".into());
        }
        let mut min = [f32::MAX; 3];
        let mut max = [f32::MIN; 3];
        for p in &primitives {
            for v in p.vertices.as_chunks::<FLOATS_PER_VERTEX>().0 {
                for a in 0..3 {
                    min[a] = min[a].min(v[a]);
                    max[a] = max[a].max(v[a]);
                }
            }
        }
        Ok(Self {
            vrm,
            primitives,
            materials,
            textures,
            images,
            mesh_count: meshes.len(),
            bounds: (min, max),
        })
    }

    pub fn stats(&self) -> Stats {
        // 同じノードの複数プリミティブは同じ target 群を共有するので、最大のものを数える
        let mut targets = std::collections::BTreeMap::new();
        for p in &self.primitives {
            let e = targets.entry(p.node).or_insert(0);
            *e = (*e).max(p.targets.len());
        }
        Stats {
            meshes: self.mesh_count,
            primitives: self.primitives.len(),
            vertices: self
                .primitives
                .iter()
                .map(|p| p.vertices.len() / FLOATS_PER_VERTEX)
                .sum(),
            triangles: self.primitives.iter().map(|p| p.indices.len() / 3).sum(),
            materials: self.materials.len() - 1,
            images: self.images.iter().map(|i| (i.width, i.height)).collect(),
            morph_targets: targets.values().sum(),
        }
    }
}

/// bufferView も sparse もない accessor は、glTF の仕様では「すべて 0」の意味(UniVRM が
/// 変化のない morph target に出す)。gltf クレートの検査はこれを弾くので、0 を詰めた
/// bufferView を BIN チャンクの末尾に足して、そこを指すように glb を組み直す。
/// 該当する accessor がなければ `None`(元のバイト列をそのまま読む)。
fn zero_fill_empty_accessors(bytes: &[u8], json: &serde_json::Value) -> Option<Vec<u8>> {
    use serde_json::Value;
    let accessors = json.get("accessors")?.as_array()?;
    let empty = |a: &Value| a.get("bufferView").is_none() && a.get("sparse").is_none();
    let size_of = |a: &Value| -> Option<u64> {
        let comps = match a.get("type")?.as_str()? {
            "SCALAR" => 1,
            "VEC2" => 2,
            "VEC3" => 3,
            "VEC4" | "MAT2" => 4,
            "MAT3" => 9,
            "MAT4" => 16,
            _ => return None,
        };
        let bytes = match a.get("componentType")?.as_u64()? {
            5120 | 5121 => 1,
            5122 | 5123 => 2,
            _ => 4,
        };
        a.get("count")?.as_u64()?.checked_mul(comps * bytes)
    };
    let zeros = accessors
        .iter()
        .filter(|a| empty(a))
        .map(size_of)
        .collect::<Option<Vec<_>>>()?
        .into_iter()
        .max()?;
    if zeros == 0 || zeros > 1 << 30 {
        return None;
    }
    let u32_at = |o: usize| -> Option<usize> {
        let b = bytes.get(o..o + 4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
    };
    // JSON チャンクの次が BIN チャンク
    let bin_at = 20 + u32_at(12)?;
    if bytes.get(bin_at + 4..bin_at + 8)? != b"BIN\0" {
        return None;
    }
    let bin_len = u32_at(bin_at)?;
    let bin = bytes.get(bin_at + 8..bin_at + 8 + bin_len)?;
    let pad = bin.len().next_multiple_of(4) - bin.len();
    let offset = bin.len() + pad;

    let mut json = json.clone();
    let views = json.get_mut("bufferViews")?.as_array_mut()?;
    let view = views.len();
    views.push(serde_json::json!({"buffer": 0, "byteOffset": offset, "byteLength": zeros}));
    for a in json.get_mut("accessors")?.as_array_mut()? {
        if empty(a) {
            a["bufferView"] = serde_json::json!(view);
        }
    }
    json["buffers"][0]["byteLength"] = serde_json::json!(offset as u64 + zeros);
    let mut new_bin = bin.to_vec();
    new_bin.resize(offset + zeros as usize, 0);
    Some(crate::synth::pack_glb(&json, &new_bin))
}

fn wrap(m: gltf::texture::WrappingMode) -> u32 {
    match m {
        gltf::texture::WrappingMode::ClampToEdge => glow::CLAMP_TO_EDGE,
        gltf::texture::WrappingMode::MirroredRepeat => glow::MIRRORED_REPEAT,
        gltf::texture::WrappingMode::Repeat => glow::REPEAT,
    }
}

fn to_rgba(d: gltf::image::Data) -> Result<Image, String> {
    use gltf::image::Format as F;
    let n = d.width as usize * d.height as usize;
    let p = &d.pixels;
    let rgba = match d.format {
        F::R8G8B8A8 => d.pixels.clone(),
        F::R8G8B8 => p
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|c| [c[0], c[1], c[2], 255])
            .collect(),
        F::R8G8 => p
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|c| [c[0], c[0], c[0], c[1]])
            .collect(),
        F::R8 => p.iter().flat_map(|&v| [v, v, v, 255]).collect(),
        // 16 ビット(PNG)は上位バイトだけ使う(リトルエンディアン)
        F::R16G16B16A16 | F::R16G16B16 | F::R16G16 | F::R16 => {
            let hi: Vec<u8> = p.as_chunks::<2>().0.iter().map(|c| c[1]).collect();
            let ch = match d.format {
                F::R16G16B16A16 => 4,
                F::R16G16B16 => 3,
                F::R16G16 => 2,
                _ => 1,
            };
            hi.chunks_exact(ch)
                .flat_map(|c| match ch {
                    4 => [c[0], c[1], c[2], c[3]],
                    3 => [c[0], c[1], c[2], 255],
                    2 => [c[0], c[0], c[0], c[1]],
                    _ => [c[0], c[0], c[0], 255],
                })
                .collect()
        }
        f => return Err(format!("未対応のテクスチャ形式 {f:?}")),
    };
    if rgba.len() != n * 4 {
        return Err("テクスチャの大きさが合わない".into());
    }
    Ok(Image {
        width: d.width,
        height: d.height,
        rgba,
    })
}

/// 頂点ごとの変換行列(スキンなら関節の重み付き和、なければノードのワールド変換)。
fn vertex_matrices(
    n: usize,
    node_world: &Mat4,
    skin: Option<&[Mat4]>,
    joints: Option<Vec<[u16; 4]>>,
    weights: Option<Vec<[f32; 4]>>,
) -> Result<Vec<Mat4>, String> {
    let (Some(skin), Some(joints), Some(weights)) = (skin, joints, weights) else {
        return Ok(vec![*node_world; n]);
    };
    if joints.len() != n || weights.len() != n {
        return Err("JOINTS_0 / WEIGHTS_0 の数が POSITION と合わない".into());
    }
    Ok((0..n)
        .map(|i| {
            let mut m = [[0.0; 4]; 4];
            let mut sum = 0.0;
            for k in 0..4 {
                let w = weights[i][k];
                if w > 0.0
                    && let Some(j) = skin.get(joints[i][k] as usize)
                {
                    math::add_scaled(&mut m, j, w);
                    sum += w;
                }
            }
            if sum <= 0.0 {
                return *node_world;
            }
            for c in &mut m {
                for v in c.iter_mut() {
                    *v /= sum;
                }
            }
            m
        })
        .collect())
}

/// accessor が属性の期待する型(要素数・成分の型)かを、読む前に確かめる。
/// gltf クレートの reader は型が違うと assert で panic するので、ここで Err にする。
fn check_accessor(
    what: &str,
    a: &gltf::Accessor,
    dims: Dimensions,
    types: &[(DataType, bool)],
    count: Option<usize>,
) -> Result<(), String> {
    // types は (成分の型, 正規化が必要か)
    let ok = a.dimensions() == dims
        && types
            .iter()
            .any(|&(t, norm)| a.data_type() == t && (!norm || a.normalized()));
    if !ok {
        return Err(format!(
            "{what} の accessor の型が合わない({:?} / {:?}{})",
            a.dimensions(),
            a.data_type(),
            if a.normalized() { "、正規化" } else { "" }
        ));
    }
    if let Some(c) = count
        && a.count() != c
    {
        return Err(format!(
            "{what} の数({})が POSITION({c})と合わない",
            a.count()
        ));
    }
    Ok(())
}

/// プリミティブの属性・インデックス・morph target の accessor を検査し、頂点数を返す。
fn check_attributes(prim: &gltf::Primitive) -> Result<usize, String> {
    use gltf::Semantic as S;
    const F32: (DataType, bool) = (DataType::F32, false);
    let pos = prim
        .get(&S::Positions)
        .ok_or("POSITION がないプリミティブ")?;
    check_accessor("POSITION", &pos, Dimensions::Vec3, &[F32], None)?;
    let n = pos.count();
    if let Some(a) = prim.get(&S::Normals) {
        check_accessor("NORMAL", &a, Dimensions::Vec3, &[F32], Some(n))?;
    }
    if let Some(a) = prim.get(&S::TexCoords(0)) {
        let types = [F32, (DataType::U8, true), (DataType::U16, true)];
        check_accessor("TEXCOORD_0", &a, Dimensions::Vec2, &types, Some(n))?;
    }
    if let Some(a) = prim.get(&S::Joints(0)) {
        let types = [(DataType::U8, false), (DataType::U16, false)];
        check_accessor("JOINTS_0", &a, Dimensions::Vec4, &types, Some(n))?;
    }
    if let Some(a) = prim.get(&S::Weights(0)) {
        let types = [F32, (DataType::U8, true), (DataType::U16, true)];
        check_accessor("WEIGHTS_0", &a, Dimensions::Vec4, &types, Some(n))?;
    }
    if let Some(a) = prim.indices() {
        let types = [
            (DataType::U8, false),
            (DataType::U16, false),
            (DataType::U32, false),
        ];
        check_accessor("indices", &a, Dimensions::Scalar, &types, None)?;
    }
    for t in prim.morph_targets() {
        if let Some(a) = t.positions() {
            check_accessor(
                "morph target の POSITION",
                &a,
                Dimensions::Vec3,
                &[F32],
                Some(n),
            )?;
        }
        if let Some(a) = t.normals() {
            check_accessor(
                "morph target の NORMAL",
                &a,
                Dimensions::Vec3,
                &[F32],
                Some(n),
            )?;
        }
    }
    Ok(n)
}

fn read_primitive(
    prim: &gltf::Primitive,
    buffers: &[gltf::buffer::Data],
    node: usize,
    material: usize,
    node_world: &Mat4,
    skin: Option<&[Mat4]>,
) -> Result<Primitive, String> {
    let n = check_attributes(prim)?;
    let reader = prim.reader(|b| Some(&buffers[b.index()]));
    let pos: Vec<[f32; 3]> = reader
        .read_positions()
        .ok_or("POSITION がないプリミティブ")?
        .collect();
    let indices: Vec<u32> = match reader.read_indices() {
        Some(i) => i.into_u32().collect(),
        None => (0..n as u32).collect(),
    };
    if indices.iter().any(|&i| i as usize >= n) {
        return Err("インデックスが頂点数を超えている".into());
    }
    let nrm: Vec<[f32; 3]> = match reader.read_normals() {
        Some(i) => i.collect(),
        None => smooth_normals(&pos, &indices),
    };
    let uv: Vec<[f32; 2]> = match reader.read_tex_coords(0) {
        Some(i) => i.into_f32().collect(),
        None => vec![[0.0; 2]; n],
    };
    if pos.len() != n || nrm.len() != n || uv.len() != n {
        return Err("頂点属性の数が合わない".into());
    }
    let mats = vertex_matrices(
        n,
        node_world,
        skin,
        reader.read_joints(0).map(|j| j.into_u16().collect()),
        reader.read_weights(0).map(|w| w.into_f32().collect()),
    )?;
    let mut vertices = Vec::with_capacity(n * FLOATS_PER_VERTEX);
    let normal_mats: Vec<Mat4> = mats.iter().map(math::normal_matrix).collect();
    for i in 0..n {
        let p = math::transform_point(&mats[i], pos[i]);
        let nn = math::normalize(math::transform_dir(&normal_mats[i], nrm[i]));
        vertices.extend_from_slice(&[p[0], p[1], p[2], nn[0], nn[1], nn[2], uv[i][0], uv[i][1]]);
    }
    let mut targets = Vec::new();
    for (tp, tn, _) in reader.read_morph_targets() {
        let positions: Vec<[f32; 3]> = match tp {
            Some(i) => i
                .zip(&mats)
                .map(|(d, m)| math::transform_dir(m, d))
                .collect(),
            None => vec![[0.0; 3]; n],
        };
        let normals: Vec<[f32; 3]> = match tn {
            Some(i) => i
                .zip(&normal_mats)
                .map(|(d, m)| math::transform_dir(m, d))
                .collect(),
            None => vec![[0.0; 3]; n],
        };
        if positions.len() != n || normals.len() != n {
            return Err("morph target の頂点数が合わない".into());
        }
        targets.push(Target { positions, normals });
    }
    Ok(Primitive {
        node,
        vertices,
        indices,
        targets,
        material,
    })
}

/// 法線がないプリミティブ向けの面積重み付き法線。
fn smooth_normals(pos: &[[f32; 3]], indices: &[u32]) -> Vec<[f32; 3]> {
    let mut acc = vec![[0.0f32; 3]; pos.len()];
    for t in indices.as_chunks::<3>().0 {
        let [a, b, c] = [t[0] as usize, t[1] as usize, t[2] as usize];
        let e1: [f32; 3] = std::array::from_fn(|k| pos[b][k] - pos[a][k]);
        let e2: [f32; 3] = std::array::from_fn(|k| pos[c][k] - pos[a][k]);
        let f = [
            e1[1] * e2[2] - e1[2] * e2[1],
            e1[2] * e2[0] - e1[0] * e2[2],
            e1[0] * e2[1] - e1[1] * e2[0],
        ];
        for i in [a, b, c] {
            for k in 0..3 {
                acc[i][k] += f[k];
            }
        }
    }
    acc.into_iter().map(math::normalize).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::{Opts, build_vrm_doc, pack_glb};
    use serde_json::{Value, json};

    fn doc() -> (Value, Vec<u8>) {
        build_vrm_doc(&Opts {
            segments: 6,
            texture_size: 8,
        })
    }

    fn load(doc: &Value, bin: &[u8]) -> Result<Model, String> {
        Model::from_glb(&pack_glb(doc, bin))
    }

    /// 名前の属性が指す accessor の番号(メッシュ `mesh` の最初のプリミティブ)。
    fn attr(doc: &Value, mesh: usize, name: &str) -> usize {
        doc["meshes"][mesh]["primitives"][0]["attributes"][name]
            .as_u64()
            .unwrap() as usize
    }

    /// accessor の型を変える。buffer view からはみ出して glTF の検査で弾かれないよう、view も広げる。
    fn retype(doc: &mut Value, acc: usize, ty: &str, comps: u64) {
        let count = doc["accessors"][acc]["count"].as_u64().unwrap();
        let ctype = doc["accessors"][acc]["componentType"].as_u64().unwrap();
        let size = match ctype {
            5120 | 5121 => 1,
            5122 | 5123 => 2,
            _ => 4,
        };
        let view = doc["accessors"][acc]["bufferView"].as_u64().unwrap() as usize;
        doc["accessors"][acc]["type"] = json!(ty);
        doc["bufferViews"][view]["byteLength"] = json!(count * comps * size);
    }

    #[test]
    fn accessor_without_buffer_view_reads_as_zeros() {
        // UniVRM が変化のない morph target に出す「bufferView なし = すべて 0」の accessor
        let (mut d, b) = doc();
        let t = d["meshes"][0]["primitives"][0]["targets"][0]["NORMAL"]
            .as_u64()
            .unwrap() as usize;
        d["accessors"][t]
            .as_object_mut()
            .unwrap()
            .remove("bufferView");
        let m = load(&d, &b).unwrap();
        assert!(
            m.primitives[0].targets[0]
                .normals
                .iter()
                .all(|n| *n == [0.0; 3])
        );
    }

    #[test]
    fn synthetic_doc_loads() {
        let (d, b) = doc();
        load(&d, &b).unwrap();
    }

    #[test]
    fn rejects_mismatched_skin_attribute_counts() {
        let (mut d, b) = doc();
        let joints = attr(&d, 2, "JOINTS_0");
        d["accessors"][joints]["count"] = json!(2);
        let e = load(&d, &b).err().unwrap();
        assert!(e.contains("JOINTS_0") && e.contains("合わない"), "{e}");

        let (mut d, b) = doc();
        let weights = attr(&d, 2, "WEIGHTS_0");
        d["accessors"][weights]["count"] = json!(2);
        let e = load(&d, &b).err().unwrap();
        assert!(e.contains("WEIGHTS_0") && e.contains("合わない"), "{e}");
    }

    #[test]
    fn rejects_mismatched_normal_and_uv_counts() {
        let (mut d, b) = doc();
        let n = attr(&d, 0, "NORMAL");
        d["accessors"][n]["count"] = json!(3);
        let e = load(&d, &b).err().unwrap();
        assert!(e.contains("NORMAL"), "{e}");

        let (mut d, b) = doc();
        let uv = attr(&d, 0, "TEXCOORD_0");
        d["accessors"][uv]["count"] = json!(3);
        let e = load(&d, &b).err().unwrap();
        assert!(e.contains("TEXCOORD_0"), "{e}");
    }

    #[test]
    fn rejects_cyclic_or_shared_children() {
        // 循環: root → Head → root
        let (mut d, b) = doc();
        d["nodes"][1]["children"] = json!([0]);
        let e = load(&d, &b).err().unwrap();
        assert!(e.contains("複数回"), "{e}");

        // 親が複数: Body(2) が root と Head の両方の子
        let (mut d, b) = doc();
        d["nodes"][1]["children"] = json!([2]);
        let e = load(&d, &b).err().unwrap();
        assert!(e.contains("複数回"), "{e}");
    }

    #[test]
    fn rejects_wrong_accessor_types() {
        // NORMAL が VEC4
        let (mut d, b) = doc();
        let n = attr(&d, 0, "NORMAL");
        retype(&mut d, n, "VEC4", 4);
        let e = load(&d, &b).err().unwrap();
        assert!(e.contains("NORMAL") && e.contains("型"), "{e}");

        // POSITION が VEC2
        let (mut d, b) = doc();
        let p = attr(&d, 1, "POSITION");
        retype(&mut d, p, "VEC2", 2);
        let e = load(&d, &b).err().unwrap();
        assert!(e.contains("POSITION"), "{e}");

        // TEXCOORD_0 が VEC3
        let (mut d, b) = doc();
        let uv = attr(&d, 0, "TEXCOORD_0");
        retype(&mut d, uv, "VEC3", 3);
        let e = load(&d, &b).err().unwrap();
        assert!(e.contains("TEXCOORD_0"), "{e}");

        // JOINTS_0 が SCALAR
        let (mut d, b) = doc();
        let j = attr(&d, 2, "JOINTS_0");
        retype(&mut d, j, "SCALAR", 1);
        let e = load(&d, &b).err().unwrap();
        assert!(e.contains("JOINTS_0"), "{e}");

        // indices が VEC3
        let (mut d, b) = doc();
        let i = d["meshes"][1]["primitives"][0]["indices"].as_u64().unwrap() as usize;
        retype(&mut d, i, "VEC3", 3);
        let e = load(&d, &b).err().unwrap();
        assert!(e.contains("indices"), "{e}");

        // morph target の POSITION が VEC4
        let (mut d, b) = doc();
        let t = d["meshes"][0]["primitives"][0]["targets"][0]["POSITION"]
            .as_u64()
            .unwrap() as usize;
        retype(&mut d, t, "VEC4", 4);
        let e = load(&d, &b).err().unwrap();
        assert!(e.contains("morph target"), "{e}");
    }
}
