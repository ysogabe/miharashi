//! VRM 1.0 の拡張 `VRMC_vrm` の解釈。バージョンの確認と、expressions のプリセット名 →
//! morph target bind の対応だけを読む(humanoid・lookAt・springBone などは読まない)。
use serde_json::Value;

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
    Ok(Vrm {
        spec_version,
        expressions,
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
