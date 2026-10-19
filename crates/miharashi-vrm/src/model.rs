//! VRM(glb)の読み込み。GL には触れない(CPU 側のデータだけを作る)。
//! 頂点はメッシュのローカル空間のまま持ち(スキンなしのプリミティブは 1 関節のスキンとして扱う)、
//! ノード階層(ローカル TRS)から毎フレーム関節行列を作って GPU で変形する。
use crate::math::{self, IDENTITY, Mat4, QUAT_IDENTITY, Quat, Trs};
use crate::vrm::{self, Mtoon, Vrm};
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
    /// `VRMC_materials_mtoon`(なければ Lambert で描く)
    pub mtoon: Option<Mtoon>,
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

/// 頂点は `[px, py, pz, nx, ny, nz, u, v]` の 8 float(インターリーブ、メッシュのローカル空間)。
pub const FLOATS_PER_VERTEX: usize = 8;

/// シーンに含まれるノード。`Model::order` は親が子より先の並び。
pub struct Node {
    pub parent: Option<usize>,
    /// ファイルにある初期姿勢(ローカル)
    pub rest: Trs,
}

pub struct Skin {
    /// 関節のノード番号
    pub joints: Vec<usize>,
    pub inverse_bind: Vec<Mat4>,
}

pub struct Primitive {
    /// このプリミティブを持つノードの番号(expression の bind はノード単位)
    pub node: usize,
    /// ローカル空間の頂点(morph の差分もローカル空間)
    pub vertices: Vec<f32>,
    /// 頂点ごとの関節番号(スキンの関節の何番目か)と重み。スキンなしは `[0;4]` / `[1,0,0,0]`
    pub joints: Vec<[u16; 4]>,
    pub weights: Vec<[f32; 4]>,
    /// 関節行列の配列(`Model::joint_nodes`)での、このプリミティブの先頭
    pub joint_base: usize,
    pub indices: Vec<u32>,
    pub targets: Vec<Target>,
    pub material: usize,
}

pub struct Model {
    pub vrm: Vrm,
    pub nodes: Vec<Node>,
    /// 親が先の並び(シーンから辿れるノードだけ)
    pub order: Vec<usize>,
    pub skins: Vec<Skin>,
    /// 関節行列の配列 `world[node] * inverse_bind` の元(スキンの関節を並べ、続けてスキンなしの
    /// プリミティブが持つノード(`inverse_bind` は単位))
    pub joint_nodes: Vec<(usize, Mat4)>,
    pub primitives: Vec<Primitive>,
    pub materials: Vec<Material>,
    pub textures: Vec<Texture>,
    pub images: Vec<Image>,
    pub mesh_count: usize,
    /// 初期姿勢での外接箱 `(min, max)`
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
        let mut vrm = vrm::parse(&json)?;
        let patched = zero_fill_empty_accessors(bytes, &json)?;
        let (doc, buffers, images) = gltf::import_slice(patched.as_deref().unwrap_or(bytes))
            .map_err(|e| format!("glb を読めない: {e}"))?;
        check_bufferless_accessors(&doc)?;
        let images = images
            .into_iter()
            .map(to_rgba)
            .collect::<Result<Vec<_>, _>>()?;
        let textures: Vec<Texture> = doc
            .textures()
            .map(|t| Texture {
                image: t.source().index(),
                wrap_s: wrap(t.sampler().wrap_s()),
                wrap_t: wrap(t.sampler().wrap_t()),
            })
            .collect();
        let tex_count = textures.len();
        let json_materials = json.get("materials").and_then(|m| m.as_array());
        let mut materials: Vec<Material> = doc
            .materials()
            .enumerate()
            .map(|(mi, m)| {
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
                    mtoon: json_materials
                        .and_then(|a| a.get(mi))
                        .and_then(|j| vrm::parse_mtoon(j, tex_count)),
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
            mtoon: None,
        });

        // ノード階層(シーンの根から辿る)
        let n_nodes = doc.nodes().len();
        // 範囲外のノード番号は、アニメーション側で添字に使う前に捨てる
        vrm.human_bones.retain(|_, n| *n < n_nodes);
        let mut nodes: Vec<Node> = doc
            .nodes()
            .map(|n| {
                let (t, r, sc) = n.transform().decomposed();
                Node {
                    parent: None,
                    rest: Trs { t, r, s: sc },
                }
            })
            .collect();
        let scene = doc
            .default_scene()
            .or_else(|| doc.scenes().next())
            .ok_or("glTF にシーンがない")?;
        let mut order = Vec::new();
        let mut visited = vec![false; n_nodes];
        let mut stack: Vec<(gltf::Node, Option<usize>)> =
            scene.nodes().map(|n| (n, None)).collect();
        while let Some((n, parent)) = stack.pop() {
            // 子の循環や、複数の親から同じノードに来る形(glTF では不正)は読まない
            if std::mem::replace(&mut visited[n.index()], true) {
                return Err(format!(
                    "ノード {} を複数回辿った(children が循環、または親が複数)",
                    n.index()
                ));
            }
            nodes[n.index()].parent = parent;
            order.push(n.index());
            stack.extend(n.children().map(|c| (c, Some(n.index()))));
        }
        // スタックで辿るので、親は必ず子より先に出る

        let mut skins = Vec::new();
        for s in doc.skins() {
            if let Some(a) = s.inverse_bind_matrices() {
                check_accessor("inverseBindMatrices", &a, Dimensions::Mat4, &[F32], None)?;
            }
            let reader = s.reader(|b| Some(&buffers[b.index()]));
            let ibm: Vec<Mat4> = reader
                .read_inverse_bind_matrices()
                .map(|i| i.collect())
                .unwrap_or_default();
            let joints: Vec<usize> = s.joints().map(|j| j.index()).collect();
            let inverse_bind = (0..joints.len())
                .map(|k| ibm.get(k).copied().unwrap_or(IDENTITY))
                .collect();
            skins.push(Skin {
                joints,
                inverse_bind,
            });
        }
        // 関節行列の配列: スキンごとに並べる
        let mut joint_nodes: Vec<(usize, Mat4)> = Vec::new();
        let mut skin_base = Vec::new();
        for sk in &skins {
            skin_base.push(joint_nodes.len());
            joint_nodes.extend(
                sk.joints
                    .iter()
                    .copied()
                    .zip(sk.inverse_bind.iter().copied()),
            );
        }
        let mut node_slot = std::collections::BTreeMap::new();

        let mut primitives = Vec::new();
        let mut meshes = std::collections::BTreeSet::new();
        let mut sorted = order.clone();
        sorted.sort_unstable();
        for &ni in &sorted {
            let node = doc.nodes().nth(ni).ok_or("ノード番号が不正")?;
            let Some(mesh) = node.mesh() else { continue };
            meshes.insert(mesh.index());
            let skin = node.skin().map(|s| s.index());
            for prim in mesh.primitives() {
                if prim.mode() != gltf::mesh::Mode::Triangles {
                    continue;
                }
                let material = prim.material().index().unwrap_or(default_material);
                let mut p = read_primitive(
                    &prim,
                    &buffers,
                    ni,
                    material,
                    skin.map(|k| skins[k].joints.len()),
                )?;
                // JOINTS/WEIGHTS を持つスキンのプリミティブだけが、スキンの関節を使う
                p.0.joint_base = match (skin, p.1) {
                    (Some(k), true) => skin_base[k],
                    _ => *node_slot.entry(ni).or_insert_with(|| {
                        joint_nodes.push((ni, IDENTITY));
                        joint_nodes.len() - 1
                    }),
                };
                primitives.push(p.0);
            }
        }
        if primitives.is_empty() {
            return Err("描画できる三角形メッシュがない".into());
        }
        let mut model = Self {
            vrm,
            nodes,
            order,
            skins,
            joint_nodes,
            primitives,
            materials,
            textures,
            images,
            mesh_count: meshes.len(),
            bounds: ([0.0; 3], [0.0; 3]),
        };
        // 初期姿勢の外接箱
        let joints = model.rest_joint_matrices();
        let mut min = [f32::MAX; 3];
        let mut max = [f32::MIN; 3];
        for i in 0..model.primitives.len() {
            for v in model
                .posed_vertices(i, &joints)
                .as_chunks::<FLOATS_PER_VERTEX>()
                .0
            {
                for a in 0..3 {
                    min[a] = min[a].min(v[a]);
                    max[a] = max[a].max(v[a]);
                }
            }
        }
        model.bounds = (min, max);
        Ok(model)
    }

    /// ローカル変換から、ノードのワールド行列とワールド回転を求める(`order` の順)。
    /// シーンから辿れないノードは単位のまま。
    pub fn eval_world(&self, locals: &[Trs], world: &mut Vec<Mat4>, world_rot: &mut Vec<Quat>) {
        world.resize(self.nodes.len(), IDENTITY);
        world_rot.resize(self.nodes.len(), QUAT_IDENTITY);
        for &i in &self.order {
            let l = locals[i].to_mat4();
            match self.nodes[i].parent {
                Some(p) => {
                    world[i] = math::mul(&world[p], &l);
                    world_rot[i] = math::quat_mul(world_rot[p], locals[i].r);
                }
                None => {
                    world[i] = l;
                    world_rot[i] = locals[i].r;
                }
            }
        }
    }

    pub fn rest_locals(&self) -> Vec<Trs> {
        self.nodes.iter().map(|n| n.rest).collect()
    }

    /// 関節行列(`world[node] * inverse_bind`)を `joint_nodes` の順に作る。
    pub fn joint_matrices(&self, world: &[Mat4], out: &mut Vec<Mat4>) {
        out.clear();
        out.extend(
            self.joint_nodes
                .iter()
                .map(|(n, ibm)| math::mul(&world[*n], ibm)),
        );
    }

    /// 初期姿勢(ファイルにあるローカル変換)の関節行列。
    pub fn rest_joint_matrices(&self) -> Vec<Mat4> {
        let (mut w, mut r) = (Vec::new(), Vec::new());
        self.eval_world(&self.rest_locals(), &mut w, &mut r);
        let mut out = Vec::new();
        self.joint_matrices(&w, &mut out);
        out
    }

    /// 関節行列で変形した頂点(`[px,py,pz,nx,ny,nz,u,v]`)。GPU のスキニングと同じ計算を CPU で行う
    /// (外接箱・試験・bake 用)。重みの合計が 0 の頂点は変換しない。
    pub fn posed_vertices(&self, prim: usize, joints: &[Mat4]) -> Vec<f32> {
        let p = &self.primitives[prim];
        let mut out = p.vertices.clone();
        for (i, v) in out
            .as_chunks_mut::<FLOATS_PER_VERTEX>()
            .0
            .iter_mut()
            .enumerate()
        {
            let mut m = [[0.0; 4]; 4];
            let mut sum = 0.0;
            for k in 0..4 {
                let w = p.weights[i][k];
                if w > 0.0
                    && let Some(j) = joints.get(p.joint_base + p.joints[i][k] as usize)
                {
                    math::add_scaled(&mut m, j, w);
                    sum += w;
                }
            }
            if sum <= 0.0 {
                continue;
            }
            for c in &mut m {
                for x in c.iter_mut() {
                    *x /= sum;
                }
            }
            let pos = math::transform_point(&m, [v[0], v[1], v[2]]);
            let n = math::normalize(math::transform_dir(
                &math::normal_matrix(&m),
                [v[3], v[4], v[5]],
            ));
            v[..6].copy_from_slice(&[pos[0], pos[1], pos[2], n[0], n[1], n[2]]);
        }
        out
    }

    /// 初期姿勢で頂点に焼き込んだもの(以前の描き方。`bake` の機能段階が使う)。
    pub fn baked_vertices(&self, prim: usize) -> Vec<f32> {
        self.posed_vertices(prim, &self.rest_joint_matrices())
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

/// 0 詰めの大きさの下限(BIN チャンクが短くても、これだけは許す)。
const MIN_ZERO_LIMIT: u64 = 1 << 20;

/// bufferView も sparse もない accessor は、glTF の仕様では「すべて 0」の意味(UniVRM が
/// 変化のない morph target に出す)。gltf クレートの検査はこれを弾くので、0 を詰めた
/// bufferView を BIN チャンクの末尾に足して、そこを指すように glb を組み直す。
/// 対象は morph target が参照する accessor だけで、0 の大きさは BIN チャンクの長さ
/// (下限 1 MiB)までに限る(小さな glb で大きなメモリを確保させない)。
/// 該当する accessor がなければ `Ok(None)`(元のバイト列をそのまま読む)。
fn zero_fill_empty_accessors(
    bytes: &[u8],
    json: &serde_json::Value,
) -> Result<Option<Vec<u8>>, String> {
    use serde_json::Value;
    let Some(accessors) = json.get("accessors").and_then(Value::as_array) else {
        return Ok(None);
    };
    // morph target の属性が指す accessor の番号
    let mut targeted = std::collections::BTreeSet::new();
    let meshes = json.get("meshes").and_then(Value::as_array);
    for prim in meshes
        .into_iter()
        .flatten()
        .filter_map(|m| m.get("primitives")?.as_array())
        .flatten()
    {
        let targets = prim.get("targets").and_then(Value::as_array);
        for attrs in targets.into_iter().flatten().filter_map(Value::as_object) {
            targeted.extend(attrs.values().filter_map(Value::as_u64));
        }
    }
    let empty = |i: usize, a: &Value| {
        targeted.contains(&(i as u64)) && a.get("bufferView").is_none() && a.get("sparse").is_none()
    };
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
    let sizes: Option<Vec<u64>> = accessors
        .iter()
        .enumerate()
        .filter(|&(i, a)| empty(i, a))
        .map(|(_, a)| size_of(a))
        .collect();
    // 大きさが読めない accessor は、そのまま gltf クレートの検査に任せる
    let Some(zeros) = sizes.and_then(|v| v.into_iter().max()).filter(|&z| z > 0) else {
        return Ok(None);
    };
    let u32_at = |o: usize| -> Option<usize> {
        let b = bytes.get(o..o.checked_add(4)?)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
    };
    // JSON チャンクの次が BIN チャンク
    let bin = (|| {
        let bin_at = 20usize.checked_add(u32_at(12)?)?;
        if bytes.get(bin_at.checked_add(4)?..bin_at.checked_add(8)?)? != b"BIN\0" {
            return None;
        }
        let bin_len = u32_at(bin_at)?;
        bytes.get(bin_at + 8..bin_at.checked_add(8)?.checked_add(bin_len)?)
    })();
    let Some(bin) = bin else { return Ok(None) };
    let limit = (bin.len() as u64).max(MIN_ZERO_LIMIT);
    if zeros > limit {
        return Err(format!(
            "bufferView のない accessor が大きすぎる({zeros} バイト、上限 {limit})"
        ));
    }
    let pad = bin.len().next_multiple_of(4) - bin.len();
    let offset = bin.len() + pad;

    let mut json = json.clone();
    let Some(views) = json.get_mut("bufferViews").and_then(Value::as_array_mut) else {
        return Ok(None);
    };
    let view = views.len();
    views.push(serde_json::json!({"buffer": 0, "byteOffset": offset, "byteLength": zeros}));
    if let Some(accessors) = json.get_mut("accessors").and_then(Value::as_array_mut) {
        for (i, a) in accessors.iter_mut().enumerate() {
            if empty(i, a)
                && let Some(o) = a.as_object_mut()
            {
                o.insert("bufferView".into(), serde_json::json!(view));
            }
        }
    }
    let buffer = json
        .get_mut("buffers")
        .and_then(|b| b.get_mut(0))
        .and_then(Value::as_object_mut)
        .ok_or("buffers[0] がない")?;
    buffer.insert(
        "byteLength".into(),
        serde_json::json!(offset as u64 + zeros),
    );
    let mut new_bin = bin.to_vec();
    new_bin.resize(offset + zeros as usize, 0);
    Ok(Some(crate::synth::pack_glb(&json, &new_bin)))
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

/// bufferView のない accessor(sparse だけで「残りは 0」の形)は、reader が `count` 個の 0 を
/// 出して `collect()` が先に確保するので、`count` が巨大だと確保の失敗で abort する。
/// 大きさは `zero_fill_empty_accessors` と同じく、buffer の長さ(下限 1 MiB)までに限る。
/// 頂点数・index 数は bufferView のある accessor では bufferView(= buffer)の長さで決まる。
fn check_bufferless_accessors(doc: &gltf::Document) -> Result<(), String> {
    let limit = doc
        .buffers()
        .map(|b| b.length() as u64)
        .max()
        .unwrap_or(0)
        .max(MIN_ZERO_LIMIT);
    for a in doc.accessors().filter(|a| a.view().is_none()) {
        let bytes = (a.count() as u64).saturating_mul(a.size() as u64);
        if bytes > limit {
            return Err(format!(
                "accessor {} は bufferView がないのに大きすぎる({bytes} バイト、上限 {limit})",
                a.index()
            ));
        }
    }
    Ok(())
}

/// bufferView 自身が buffer に収まるか。reader は `offset + length` をそのまま足すので、
/// 収まらない値だと debug ビルドで桁あふれの panic、release では回り込む。
fn check_view(what: &str, view: &gltf::buffer::View) -> Result<(), String> {
    let end = view.offset().checked_add(view.length());
    if end.is_none_or(|e| e > view.buffer().length()) {
        return Err(format!("{what} の bufferView が buffer の範囲を超える"));
    }
    Ok(())
}

/// accessor が属性の期待する型(要素数・成分の型)かを、読む前に確かめる。
/// gltf クレートの reader は型が違うと assert で panic するので、ここで Err にする。
const F32: (DataType, bool) = (DataType::F32, false);

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
    if let Some(v) = a.view() {
        check_view(what, &v)?;
        // byteStride が要素より小さいと reader が panic する
        let stride = v.stride().unwrap_or(a.size());
        if stride < a.size() {
            return Err(format!(
                "{what} の byteStride({stride})が要素の大きさ({})より小さい",
                a.size()
            ));
        }
        // 最後の要素の終わりが bufferView に収まること(sparse の base もこの範囲を読む)
        let end = stride
            .checked_mul(a.count().saturating_sub(1))
            .and_then(|e| e.checked_add(a.offset()))
            .and_then(|e| e.checked_add(a.size()));
        if end.is_none_or(|e| e > v.length()) {
            return Err(format!("{what} が bufferView の範囲を超える"));
        }
    }
    if let Some(c) = count
        && a.count() != c
    {
        return Err(format!(
            "{what} の数({})が POSITION({c})と合わない",
            a.count()
        ));
    }
    check_sparse(what, a)
}

/// sparse accessor の indices / values の bufferView を検査する。
/// gltf クレートの reader は、これらの stride や範囲が壊れていると panic するか、読み出しが
/// 範囲外にならないことを保証しないので、読む前にここで Err にする。
fn check_sparse(what: &str, a: &gltf::Accessor) -> Result<(), String> {
    let Some(sp) = a.sparse() else {
        return Ok(());
    };
    let n = sp.count();
    if n == 0 || n > a.count() {
        return Err(format!(
            "{what} の sparse の数({n})が 1〜accessor の数({})の範囲にない",
            a.count()
        ));
    }
    // (名前, bufferView, byteOffset, 要素の大きさ)
    let idx = sp.indices();
    let val = sp.values();
    let parts = [
        ("indices", idx.view(), idx.offset(), idx.index_type().size()),
        ("values", val.view(), val.offset(), a.size()),
    ];
    for (name, view, offset, size) in parts {
        check_view(&format!("{what} の sparse {name}"), &view)?;
        let stride = view.stride().unwrap_or(size);
        if stride < size {
            return Err(format!(
                "{what} の sparse {name} の byteStride({stride})が要素の大きさ({size})より小さい"
            ));
        }
        // 最後の要素の終わりが bufferView に収まること(usize の桁あふれも弾く)
        let end = stride
            .checked_mul(n - 1)
            .and_then(|v| v.checked_add(offset))
            .and_then(|v| v.checked_add(size));
        if end.is_none_or(|e| e > view.length()) {
            return Err(format!(
                "{what} の sparse {name} が bufferView の範囲を超える"
            ));
        }
    }
    Ok(())
}

/// プリミティブの属性・インデックス・morph target の accessor を検査し、頂点数を返す。
fn check_attributes(prim: &gltf::Primitive) -> Result<usize, String> {
    use gltf::Semantic as S;
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

/// プリミティブを読む。戻り値の `bool` は、スキンの関節を使うか(JOINTS_0 / WEIGHTS_0 がある)。
/// `skin_joints` はノードのスキンの関節数(関節番号の範囲検査用)。
fn read_primitive(
    prim: &gltf::Primitive,
    buffers: &[gltf::buffer::Data],
    node: usize,
    material: usize,
    skin_joints: Option<usize>,
) -> Result<(Primitive, bool), String> {
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
    let joints_in: Option<Vec<[u16; 4]>> = reader.read_joints(0).map(|j| j.into_u16().collect());
    let weights_in: Option<Vec<[f32; 4]>> = reader.read_weights(0).map(|w| w.into_f32().collect());
    let (joints, weights, skinned) = match (skin_joints, joints_in, weights_in) {
        (Some(count), Some(j), Some(mut w)) => {
            if j.len() != n || w.len() != n {
                return Err("JOINTS_0 / WEIGHTS_0 の数が POSITION と合わない".into());
            }
            // 関節数を超える番号は、重みを 0 にし番号も 0 にする(シェーダが UBO の範囲外を読まないように)
            let mut j = j;
            for (jv, wv) in j.iter_mut().zip(w.iter_mut()) {
                for k in 0..4 {
                    if jv[k] as usize >= count {
                        wv[k] = 0.0;
                        jv[k] = 0;
                    }
                }
            }
            (j, w, true)
        }
        _ => (vec![[0; 4]; n], vec![[1.0, 0.0, 0.0, 0.0]; n], false),
    };
    let mut vertices = Vec::with_capacity(n * FLOATS_PER_VERTEX);
    for i in 0..n {
        let nn = math::normalize(nrm[i]);
        vertices.extend_from_slice(&[
            pos[i][0], pos[i][1], pos[i][2], nn[0], nn[1], nn[2], uv[i][0], uv[i][1],
        ]);
    }
    let mut targets = Vec::new();
    for (tp, tn, _) in reader.read_morph_targets() {
        let positions: Vec<[f32; 3]> = match tp {
            Some(i) => i.collect(),
            None => vec![[0.0; 3]; n],
        };
        let normals: Vec<[f32; 3]> = match tn {
            Some(i) => i.collect(),
            None => vec![[0.0; 3]; n],
        };
        if positions.len() != n || normals.len() != n {
            return Err("morph target の頂点数が合わない".into());
        }
        targets.push(Target { positions, normals });
    }
    Ok((
        Primitive {
            node,
            vertices,
            joints,
            weights,
            joint_base: 0,
            indices,
            targets,
            material,
        },
        skinned,
    ))
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
    use crate::anim::Rig;
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
    fn out_of_range_human_bone_nodes_are_dropped() {
        let (mut d, b) = doc();
        let hb = &mut d["extensions"]["VRMC_vrm"]["humanoid"]["humanBones"];
        hb["leftUpperArm"] = json!({"node": 9999});
        hb["head"] = json!({"node": 9999});
        let m = load(&d, &b).unwrap();
        assert!(!m.vrm.human_bones.contains_key("leftUpperArm"));
        assert!(!m.vrm.human_bones.contains_key("head"));
        assert!(m.vrm.human_bones.contains_key("leftLowerArm"));
        let mut rig = Rig::new(&m, true);
        rig.update(&m, 0.0, None);
    }

    #[test]
    fn spring_bone_duplicates_and_counts_stay_small() {
        use crate::spring::SpringSim;
        use crate::vrm::SPRING_LIMIT;
        let (mut d, b) = doc();
        // コライダー 1 個を指すグループ番号・コライダー番号を大量に重ねる
        let dup: Vec<u32> = vec![0; 20_000];
        let groups: Vec<Value> = (0..3000).map(|_| json!({"colliders": dup})).collect();
        let joints: Vec<Value> = (0..3000).map(|_| json!({"node": 9})).collect();
        let sb = &mut d["extensions"]["VRMC_springBone"];
        sb["colliderGroups"] = json!(groups);
        sb["springs"] = json!([{"colliderGroups": vec![0u32; 20_000], "joints": joints}]);
        let m = load(&d, &b).unwrap();
        let sb = &m.vrm.spring;
        assert!(sb.collider_groups.len() <= SPRING_LIMIT);
        assert_eq!(sb.collider_groups[0], [0]);
        assert_eq!(sb.springs[0].collider_groups, [0]);
        assert_eq!(sb.springs[0].joints.len(), SPRING_LIMIT);
        let rig = Rig::new(&m, false);
        let sim = SpringSim::new(&m, &rig);
        assert_eq!(sim.joint_count(), SPRING_LIMIT);
    }

    /// 形態ターゲットの POSITION accessor に、sparse(indices と values の bufferView)を付ける。
    /// bufferView は BIN の先頭を指す。`idx` / `val` は (byteOffset, byteLength, byteStride)。
    fn add_sparse(
        doc: &mut Value,
        count: u64,
        idx: (u64, u64, Option<u64>),
        val: (u64, u64, Option<u64>),
    ) {
        let t = doc["meshes"][0]["primitives"][0]["targets"][0]["POSITION"]
            .as_u64()
            .unwrap() as usize;
        let views = doc["bufferViews"].as_array_mut().unwrap();
        let mut push = |(off, len, stride): (u64, u64, Option<u64>)| {
            let mut v = json!({"buffer": 0, "byteOffset": off, "byteLength": len});
            if let Some(s) = stride {
                v["byteStride"] = json!(s);
            }
            views.push(v);
            views.len() - 1
        };
        let (iv, vv) = (push(idx), push(val));
        doc["accessors"][t]["sparse"] = json!({
            "count": count,
            "indices": {"bufferView": iv, "componentType": 5123},
            "values": {"bufferView": vv},
        });
    }

    #[test]
    fn sparse_accessor_with_valid_views_loads() {
        let (mut d, b) = doc();
        add_sparse(&mut d, 2, (0, 4, None), (0, 24, None));
        load(&d, &b).unwrap();
    }

    /// sparse の indices / values 自身の byteOffset を書き換える。
    fn set_sparse_offset(doc: &mut Value, which: &str, offset: u64) {
        let t = doc["meshes"][0]["primitives"][0]["targets"][0]["POSITION"]
            .as_u64()
            .unwrap() as usize;
        doc["accessors"][t]["sparse"][which]["byteOffset"] = json!(offset);
    }

    #[test]
    fn rejects_broken_sparse_views_without_panic() {
        // (indices, values, エラーに含まれる文言)。gltf の検査を通り、check_sparse で弾く形だけ
        type View = (u64, u64, Option<u64>);
        let cases: [(View, View, &str); 3] = [
            // indices が bufferView からはみ出す(u16 × 2 に 2 バイトしかない)
            (
                (0, 2, None),
                (0, 24, None),
                "sparse indices が bufferView の範囲を超える",
            ),
            // values が bufferView からはみ出す
            (
                (0, 4, None),
                (0, 12, None),
                "sparse values が bufferView の範囲を超える",
            ),
            // values の byteStride が要素(12)より小さい
            (
                (0, 4, None),
                (0, 24, Some(4)),
                "sparse values の byteStride(4)",
            ),
        ];
        for (idx, val, word) in cases {
            let (mut d, b) = doc();
            add_sparse(&mut d, 2, idx, val);
            let e = load(&d, &b)
                .err()
                .unwrap_or_else(|| panic!("{idx:?} {val:?}"));
            assert!(e.contains(word), "{word}: {e}");
        }
        // sparse の数が 0、または accessor の数を超える
        for n in [0, 1 << 20] {
            let (mut d, b) = doc();
            add_sparse(&mut d, n, (0, 4, None), (0, 24, None));
            let e = load(&d, &b).err().unwrap();
            assert!(e.contains("sparse の数"), "{e}");
        }
        // indices / values 自身の byteOffset が大きく、足し算や掛け算が桁あふれする
        for which in ["indices", "values"] {
            for offset in [u64::MAX - 1, u32::MAX as u64, 1 << 40] {
                let (mut d, b) = doc();
                add_sparse(&mut d, 2, (0, 4, None), (0, 24, None));
                set_sparse_offset(&mut d, which, offset);
                let e = load(&d, &b).err().unwrap();
                assert!(
                    e.contains(&format!("sparse {which} が bufferView の範囲を超える")),
                    "{which} {offset}: {e}"
                );
            }
        }
    }

    #[test]
    fn rejects_sparse_views_that_gltf_refuses() {
        // byteStride は 4〜252 に限られるので、gltf の検査が先に弾く
        for (idx, val) in [
            ((0, 4, Some(1)), (0, 24, None)),
            ((0, 4, Some(u32::MAX as u64)), (0, 24, None)),
        ] {
            let (mut d, b) = doc();
            add_sparse(&mut d, 2, idx, val);
            let e = load(&d, &b).err().unwrap();
            assert!(e.contains("glb を読めない"), "{e}");
        }
    }

    #[test]
    fn rejects_buffer_view_outside_buffer() {
        // bufferView の offset + length が buffer を超える(桁あふれを含む)
        for (offset, len) in [(u64::MAX - 1, 8u64), (1 << 40, 8), (0, 1 << 40)] {
            let (mut d, b) = doc();
            add_sparse(&mut d, 2, (0, 4, None), (0, 24, None));
            let views = d["bufferViews"].as_array_mut().unwrap();
            let iv = views.len() - 2;
            views[iv]["byteOffset"] = json!(offset);
            views[iv]["byteLength"] = json!(len);
            let e = load(&d, &b)
                .err()
                .unwrap_or_else(|| panic!("{offset} {len}"));
            assert!(e.contains("bufferView が buffer の範囲を超える"), "{e}");
        }
        // sparse でない accessor の bufferView
        let (mut d, b) = doc();
        let acc = d["meshes"][0]["primitives"][0]["attributes"]["POSITION"]
            .as_u64()
            .unwrap() as usize;
        let view = d["accessors"][acc]["bufferView"].as_u64().unwrap() as usize;
        d["bufferViews"][view]["byteOffset"] = json!(u64::MAX - 8);
        let e = load(&d, &b).err().unwrap();
        assert!(e.contains("bufferView が buffer の範囲を超える"), "{e}");
    }

    #[test]
    fn sparse_only_accessor_with_huge_count_is_rejected() {
        // bufferView なし・sparse だけ・count が巨大でも、確保せずに Err にする
        for count in [2_000_000_000u64, 1 << 33, u64::MAX >> 1] {
            for morph in [false, true] {
                let (mut d, b) = doc();
                let p = &d["meshes"][0]["primitives"][0];
                let acc = if morph {
                    p["targets"][0]["POSITION"].as_u64()
                } else {
                    p["attributes"]["POSITION"].as_u64()
                }
                .unwrap() as usize;
                let a = d["accessors"][acc].as_object_mut().unwrap();
                a.remove("bufferView");
                a.insert("count".into(), json!(count));
                a.insert(
                    "sparse".into(),
                    json!({
                        "count": 1,
                        "indices": {"bufferView": 0, "componentType": 5123},
                        "values": {"bufferView": 0},
                    }),
                );
                let e = load(&d, &b)
                    .err()
                    .unwrap_or_else(|| panic!("{count} {morph}"));
                assert!(e.contains("bufferView がないのに大きすぎる"), "{e}");
            }
        }
    }

    #[test]
    fn collider_without_node_keeps_later_indices() {
        use crate::spring::SpringSim;
        use crate::vrm::{ColliderShape, NO_NODE};
        let (mut d, b) = doc();
        let sphere = |r: f64| json!({"sphere": {"offset": [0, 0, 0], "radius": r}});
        let sb = &mut d["extensions"]["VRMC_springBone"];
        sb["colliders"] = json!([
            {"node": 9, "shape": sphere(0.1)},
            {"shape": sphere(5.0)},
            {"node": 9, "shape": {"capsule": {"offset": [0, 0, 0], "radius": 0.2, "tail": [0, 1, 0]}}},
        ]);
        sb["colliderGroups"] = json!([{"colliders": [0, 1, 2]}]);
        sb["springs"][0]["colliderGroups"] = json!([0]);
        let m = load(&d, &b).unwrap();
        let cs = &m.vrm.spring.colliders;
        assert_eq!(cs.len(), 3);
        assert_eq!(cs[0].node, 9);
        // node のないものは印の node と半径 0 の球(元の半径 5.0 は捨てる)
        assert_eq!(cs[1].node, NO_NODE);
        assert_eq!(
            cs[1].shape,
            ColliderShape::Sphere {
                offset: [0.0; 3],
                radius: 0.0
            }
        );
        // 後ろのコライダーの番号はずれない
        assert_eq!(cs[2].node, 9);
        assert!(matches!(cs[2].shape, ColliderShape::Capsule { .. }));
        assert_eq!(m.vrm.spring.collider_groups[0], [0, 1, 2]);
        let mut rig = Rig::new(&m, true);
        let mut sim = SpringSim::new(&m, &rig);
        assert_eq!(sim.collision_disabled_springs(), 0);
        for _ in 0..3 {
            rig.update(&m, 0.0, None);
            sim.step(&m, &mut rig, 1.0 / 30.0);
        }
    }

    #[test]
    fn joint_times_collider_over_limit_disables_collision() {
        use crate::spring::{MAX_COLLISION_PAIRS, SpringSim};
        let (mut d, b) = doc();
        let colliders: Vec<Value> = (0..1000)
            .map(|_| json!({"node": 9, "shape": {"sphere": {"offset": [0, 0, 0], "radius": 0.1}}}))
            .collect();
        let ids: Vec<u32> = (0..1000).collect();
        let joints: Vec<Value> = (0..100).map(|_| json!({"node": 9})).collect();
        const { assert!(100 * 1000 > MAX_COLLISION_PAIRS) };
        let sb = &mut d["extensions"]["VRMC_springBone"];
        sb["colliders"] = json!(colliders);
        sb["colliderGroups"] = json!([{"colliders": ids}]);
        sb["springs"] = json!([{"colliderGroups": [0], "joints": joints}]);
        let m = load(&d, &b).unwrap();
        let mut rig = Rig::new(&m, false);
        let mut sim = SpringSim::new(&m, &rig);
        assert_eq!(sim.joint_count(), 100);
        assert_eq!(sim.collision_disabled_springs(), 1);
        rig.update(&m, 0.0, None);
        sim.step(&m, &mut rig, 1.0 / 30.0);
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
    fn rejects_huge_empty_accessor_without_allocating() {
        let (mut d, b) = doc();
        let t = d["meshes"][0]["primitives"][0]["targets"][0]["NORMAL"]
            .as_u64()
            .unwrap() as usize;
        let a = d["accessors"][t].as_object_mut().unwrap();
        a.remove("bufferView");
        a.insert("count".into(), json!(1u64 << 28));
        let e = load(&d, &b).err().unwrap();
        assert!(e.contains("大きすぎる"), "{e}");
    }

    #[test]
    fn empty_accessor_outside_morph_target_is_left_alone() {
        // morph target 以外の bufferView なし accessor は 0 詰めせず、gltf の検査で弾く
        let (mut d, b) = doc();
        let n = attr(&d, 0, "NORMAL");
        d["accessors"][n]
            .as_object_mut()
            .unwrap()
            .remove("bufferView");
        assert!(load(&d, &b).is_err());
    }

    #[test]
    fn missing_buffers_is_error_not_panic() {
        let (mut d, b) = doc();
        let t = d["meshes"][0]["primitives"][0]["targets"][0]["NORMAL"]
            .as_u64()
            .unwrap() as usize;
        d["accessors"][t]
            .as_object_mut()
            .unwrap()
            .remove("bufferView");
        d.as_object_mut().unwrap().remove("buffers");
        assert!(load(&d, &b).is_err());
        d["buffers"] = json!([]);
        assert!(load(&d, &b).is_err());
    }

    #[test]
    fn rejects_stride_smaller_than_element() {
        let (mut d, b) = doc();
        let n = attr(&d, 0, "NORMAL");
        let view = d["accessors"][n]["bufferView"].as_u64().unwrap() as usize;
        d["bufferViews"][view]["byteStride"] = json!(4);
        let e = load(&d, &b).err().unwrap();
        assert!(e.contains("byteStride"), "{e}");
    }

    #[test]
    fn rejects_wrong_inverse_bind_matrices() {
        let (mut d, b) = doc();
        let ibm = d["skins"][0]["inverseBindMatrices"].as_u64().unwrap() as usize;
        retype(&mut d, ibm, "VEC4", 4);
        let e = load(&d, &b).err().unwrap();
        assert!(e.contains("inverseBindMatrices"), "{e}");
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
