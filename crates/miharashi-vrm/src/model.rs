//! VRM(glb)の読み込み。GL には触れない(CPU 側のデータだけを作る)。
//! 静止ポーズ: ノードの変換とスキニング(バインドポーズ)をここで頂点に焼き込むので、描画側は
//! 1 つの view-projection 行列だけで描ける。
use crate::math::{self, IDENTITY, Mat4};
use crate::vrm::{self, Vrm};
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
        let vrm = vrm::parse(&vrm::glb_json(bytes)?)?;
        let (doc, buffers, images) =
            gltf::import_slice(bytes).map_err(|e| format!("glb を読めない: {e}"))?;
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
        let mut stack: Vec<(gltf::Node, Mat4)> = scene.nodes().map(|n| (n, IDENTITY)).collect();
        while let Some((n, parent)) = stack.pop() {
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
) -> Vec<Mat4> {
    let (Some(skin), Some(joints), Some(weights)) = (skin, joints, weights) else {
        return vec![*node_world; n];
    };
    (0..n)
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
        .collect()
}

fn read_primitive(
    prim: &gltf::Primitive,
    buffers: &[gltf::buffer::Data],
    node: usize,
    material: usize,
    node_world: &Mat4,
    skin: Option<&[Mat4]>,
) -> Result<Primitive, String> {
    let reader = prim.reader(|b| Some(&buffers[b.index()]));
    let pos: Vec<[f32; 3]> = reader
        .read_positions()
        .ok_or("POSITION がないプリミティブ")?
        .collect();
    let n = pos.len();
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
    if nrm.len() != n || uv.len() != n {
        return Err("頂点属性の数が合わない".into());
    }
    let mats = vertex_matrices(
        n,
        node_world,
        skin,
        reader.read_joints(0).map(|j| j.into_u16().collect()),
        reader.read_weights(0).map(|w| w.into_f32().collect()),
    );
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
