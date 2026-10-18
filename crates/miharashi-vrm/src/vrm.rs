//! VRM 1.0 の拡張の解釈。`VRMC_vrm`(バージョン、expressions のプリセット名 → morph target bind、
//! humanoid の humanBones 名 → ノード)、`VRMC_materials_mtoon`(マテリアルごと)、
//! `VRMC_springBone`(髪・服の揺れ)を読む。lookAt などは読まない。
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub struct MorphBind {
    /// glTF のノード番号(そのノードのメッシュの morph target を動かす)
    pub node: usize,
    /// メッシュ内の morph target の番号
    pub index: usize,
    pub weight: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Expression {
    pub name: String,
    pub binds: Vec<MorphBind>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Vrm {
    pub spec_version: String,
    pub expressions: Vec<Expression>,
    /// humanoid の humanBones(`leftUpperArm` など → glTF のノード番号)
    pub human_bones: BTreeMap<String, usize>,
    pub spring: SpringBone,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutlineMode {
    None,
    World,
    Screen,
}

/// `VRMC_materials_mtoon` の、描画に使う値。テクスチャは glTF の texture 番号。
#[derive(Debug, Clone, PartialEq)]
pub struct Mtoon {
    pub shade_color: [f32; 3],
    pub shade_texture: Option<usize>,
    pub shading_toony: f32,
    pub shading_shift: f32,
    pub rim_color: [f32; 3],
    pub rim_fresnel_power: f32,
    pub rim_lift: f32,
    pub rim_lighting_mix: f32,
    /// emissiveFactor × KHR_materials_emissive_strength
    pub emissive: [f32; 3],
    pub outline_mode: OutlineMode,
    /// worldCoordinates はメートル、screenCoordinates は画面の高さに対する割合
    pub outline_width: f32,
    pub outline_color: [f32; 3],
    pub outline_lighting_mix: f32,
    pub outline_width_texture: Option<usize>,
    pub render_queue_offset: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ColliderShape {
    Sphere {
        offset: [f32; 3],
        radius: f32,
    },
    Capsule {
        offset: [f32; 3],
        radius: f32,
        tail: [f32; 3],
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Collider {
    pub node: usize,
    pub shape: ColliderShape,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpringJoint {
    pub node: usize,
    pub hit_radius: f32,
    pub stiffness: f32,
    pub gravity_power: f32,
    pub gravity_dir: [f32; 3],
    pub drag_force: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Spring {
    pub name: String,
    pub joints: Vec<SpringJoint>,
    /// `SpringBone::collider_groups` の番号
    pub collider_groups: Vec<usize>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SpringBone {
    pub colliders: Vec<Collider>,
    /// コライダーの番号の並び
    pub collider_groups: Vec<Vec<usize>>,
    pub springs: Vec<Spring>,
}

impl Vrm {
    pub fn expression(&self, name: &str) -> Option<&Expression> {
        self.expressions.iter().find(|e| e.name == name)
    }

    /// 動かせる(morph bind がある)expression の中から、試験に向く名前を 1 つ選ぶ。
    pub fn default_expression(&self) -> Option<&str> {
        ["blink", "happy", "aa"]
            .iter()
            .find_map(|n| self.expression(n).filter(|e| !e.binds.is_empty()))
            .or_else(|| self.expressions.iter().find(|e| !e.binds.is_empty()))
            .map(|e| e.name.as_str())
    }
}

/// glb の JSON チャンクを取り出す。
pub fn glb_json(bytes: &[u8]) -> Result<Value, String> {
    let u32_at = |o: usize| -> Result<usize, String> {
        bytes
            .get(o..o + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
            .ok_or_else(|| "glb が短すぎる".to_string())
    };
    if bytes.get(..4) != Some(b"glTF") {
        return Err("glb ではない(先頭が glTF でない)".into());
    }
    let len = u32_at(12)?;
    if bytes.get(16..20) != Some(b"JSON") {
        return Err("glb の最初のチャンクが JSON でない".into());
    }
    let json = bytes
        .get(20..20 + len)
        .ok_or("glb の JSON チャンクが途中で切れている")?;
    serde_json::from_slice(json).map_err(|e| format!("glb の JSON を読めない: {e}"))
}

/// glTF の JSON から VRM 1.0 の情報を読む。VRM 0.x(拡張 `VRM`)は未対応としてエラーにする。
pub fn parse(gltf: &Value) -> Result<Vrm, String> {
    let ext = gltf.get("extensions");
    let Some(vrm) = ext.and_then(|e| e.get("VRMC_vrm")) else {
        return Err(if ext.and_then(|e| e.get("VRM")).is_some() {
            "VRM 0.x(拡張 VRM)は未対応。VRM 1.0(VRMC_vrm)のモデルを使う".into()
        } else {
            "VRM ではない(拡張 VRMC_vrm がない)".into()
        });
    };
    let spec_version = vrm
        .get("specVersion")
        .and_then(Value::as_str)
        .ok_or("VRMC_vrm.specVersion がない")?
        .to_string();
    if !spec_version.starts_with("1.") {
        return Err(format!("VRM の specVersion {spec_version} は未対応"));
    }
    let mut expressions = Vec::new();
    let groups = vrm.get("expressions");
    for key in ["preset", "custom"] {
        let Some(map) = groups.and_then(|g| g.get(key)).and_then(Value::as_object) else {
            continue;
        };
        for (name, e) in map {
            let binds = e
                .get("morphTargetBinds")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(parse_bind).collect())
                .unwrap_or_default();
            expressions.push(Expression {
                name: name.clone(),
                binds,
            });
        }
    }
    let mut human_bones = BTreeMap::new();
    if let Some(m) = vrm
        .get("humanoid")
        .and_then(|h| h.get("humanBones"))
        .and_then(Value::as_object)
    {
        for (name, b) in m {
            if let Some(n) = b.get("node").and_then(Value::as_u64) {
                human_bones.insert(name.clone(), n as usize);
            }
        }
    }
    Ok(Vrm {
        spec_version,
        expressions,
        human_bones,
        spring: parse_spring(gltf),
    })
}

fn f3(v: Option<&Value>, default: [f32; 3]) -> [f32; 3] {
    let Some(a) = v.and_then(Value::as_array).filter(|a| a.len() >= 3) else {
        return default;
    };
    std::array::from_fn(|i| a[i].as_f64().unwrap_or(default[i] as f64) as f32)
}

fn num(v: Option<&Value>, default: f32) -> f32 {
    v.and_then(Value::as_f64).map_or(default, |x| x as f32)
}

/// `VRMC_springBone`。拡張がなければ空。壊れた項目は読み飛ばす。
pub fn parse_spring(gltf: &Value) -> SpringBone {
    let Some(sb) = gltf
        .get("extensions")
        .and_then(|e| e.get("VRMC_springBone"))
    else {
        return SpringBone::default();
    };
    let mut out = SpringBone::default();
    for c in sb
        .get("colliders")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(node) = c.get("node").and_then(Value::as_u64) else {
            continue;
        };
        let shape = c.get("shape");
        let sh = if let Some(s) = shape.and_then(|s| s.get("sphere")) {
            ColliderShape::Sphere {
                offset: f3(s.get("offset"), [0.0; 3]),
                radius: num(s.get("radius"), 0.0),
            }
        } else if let Some(s) = shape.and_then(|s| s.get("capsule")) {
            ColliderShape::Capsule {
                offset: f3(s.get("offset"), [0.0; 3]),
                radius: num(s.get("radius"), 0.0),
                tail: f3(s.get("tail"), [0.0; 3]),
            }
        } else {
            // 番号がずれないよう、読めない形は半径 0 の球にする
            ColliderShape::Sphere {
                offset: [0.0; 3],
                radius: 0.0,
            }
        };
        out.colliders.push(Collider {
            node: node as usize,
            shape: sh,
        });
    }
    for g in sb
        .get("colliderGroups")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        out.collider_groups.push(
            g.get("colliders")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_u64)
                        .map(|i| i as usize)
                        .filter(|&i| i < out.colliders.len())
                        .collect()
                })
                .unwrap_or_default(),
        );
    }
    for sp in sb
        .get("springs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let joints: Vec<SpringJoint> = sp
            .get("joints")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|j| {
                Some(SpringJoint {
                    node: j.get("node")?.as_u64()? as usize,
                    hit_radius: num(j.get("hitRadius"), 0.0),
                    stiffness: num(j.get("stiffness"), 1.0),
                    gravity_power: num(j.get("gravityPower"), 0.0),
                    gravity_dir: f3(j.get("gravityDir"), [0.0, -1.0, 0.0]),
                    drag_force: num(j.get("dragForce"), 0.5),
                })
            })
            .collect();
        if joints.is_empty() {
            continue;
        }
        out.springs.push(Spring {
            name: sp
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            joints,
            collider_groups: sp
                .get("colliderGroups")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_u64)
                        .map(|i| i as usize)
                        .filter(|&i| i < out.collider_groups.len())
                        .collect()
                })
                .unwrap_or_default(),
        });
    }
    out
}

/// マテリアル(glTF の JSON の 1 要素)の `VRMC_materials_mtoon`。なければ `None`。
/// `texture_count` を超えるテクスチャ番号は使わない。
pub fn parse_mtoon(material: &Value, texture_count: usize) -> Option<Mtoon> {
    let ext = material.get("extensions")?;
    let m = ext.get("VRMC_materials_mtoon")?;
    let tex = |key: &str| {
        m.get(key)
            .and_then(|t| t.get("index"))
            .and_then(Value::as_u64)
            .map(|i| i as usize)
            .filter(|&i| i < texture_count)
    };
    let strength = num(
        ext.get("KHR_materials_emissive_strength")
            .and_then(|e| e.get("emissiveStrength")),
        1.0,
    );
    let emissive = f3(material.get("emissiveFactor"), [0.0; 3]).map(|v| v * strength);
    let outline_mode = match m.get("outlineWidthMode").and_then(Value::as_str) {
        Some("worldCoordinates") => OutlineMode::World,
        Some("screenCoordinates") => OutlineMode::Screen,
        _ => OutlineMode::None,
    };
    Some(Mtoon {
        shade_color: f3(m.get("shadeColorFactor"), [1.0; 3]),
        shade_texture: tex("shadeMultiplyTexture"),
        shading_toony: num(m.get("shadingToonyFactor"), 0.9).clamp(0.0, 1.0),
        shading_shift: num(m.get("shadingShiftFactor"), 0.0),
        rim_color: f3(m.get("parametricRimColorFactor"), [0.0; 3]),
        rim_fresnel_power: num(m.get("parametricRimFresnelPowerFactor"), 5.0).max(0.0),
        rim_lift: num(m.get("parametricRimLiftFactor"), 0.0),
        rim_lighting_mix: num(m.get("rimLightingMixFactor"), 1.0).clamp(0.0, 1.0),
        emissive,
        outline_mode,
        outline_width: num(m.get("outlineWidthFactor"), 0.0).max(0.0),
        outline_color: f3(m.get("outlineColorFactor"), [0.0; 3]),
        outline_lighting_mix: num(m.get("outlineLightingMixFactor"), 1.0).clamp(0.0, 1.0),
        outline_width_texture: tex("outlineWidthMultiplyTexture"),
        render_queue_offset: m
            .get("renderQueueOffsetNumber")
            .and_then(Value::as_i64)
            .unwrap_or(0) as i32,
    })
}

fn parse_bind(b: &Value) -> Option<MorphBind> {
    Some(MorphBind {
        node: b.get("node")?.as_u64()? as usize,
        index: b.get("index")?.as_u64()? as usize,
        weight: b.get("weight").and_then(Value::as_f64).unwrap_or(1.0) as f32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_presets_and_binds() {
        let j = json!({"extensions": {"VRMC_vrm": {"specVersion": "1.0", "expressions": {
            "preset": {"blink": {"morphTargetBinds": [{"node": 3, "index": 1, "weight": 0.5}]},
                       "happy": {}},
            "custom": {"wink": {"morphTargetBinds": [{"node": 3, "index": 2}]}}}}}});
        let v = parse(&j).unwrap();
        assert_eq!(v.spec_version, "1.0");
        let blink = v.expression("blink").unwrap();
        assert_eq!(
            blink.binds,
            [MorphBind {
                node: 3,
                index: 1,
                weight: 0.5
            }]
        );
        assert!(v.expression("happy").unwrap().binds.is_empty());
        assert_eq!(v.expression("wink").unwrap().binds[0].weight, 1.0);
        assert_eq!(v.default_expression(), Some("blink"));
    }

    #[test]
    fn rejects_vrm0_and_plain_gltf() {
        let e = parse(&json!({"extensions": {"VRM": {"specVersion": "0.0"}}})).unwrap_err();
        assert!(e.contains("0.x") && e.contains("未対応"), "{e}");
        assert!(parse(&json!({})).unwrap_err().contains("VRM ではない"));
        let e = parse(&json!({"extensions": {"VRMC_vrm": {"specVersion": "2.0"}}})).unwrap_err();
        assert!(e.contains("2.0"));
    }
}
