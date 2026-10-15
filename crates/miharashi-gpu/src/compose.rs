//! GPU での画面の合成(PoC-005 PR3)。
//! - `Frame::Image`: CPU で描いた完成画面を 1 枚転送して描く(状況画面など。PR2 と同じ)。
//! - `Frame::Photo`: 写真のフレームは 1 枚ごとに 1 回だけテクスチャにし、クロスフェードは
//!   シェーダーで混ぜる。文字などの重ね絵(透明背景の RGBA)は変わったときだけ転送して上に重ねる。
//!   写真のフレームは、CPU で合成した完成フレームを転送するか、デコードしただけの写真から
//!   GPU で作る(PR3b、`photo.rs`)。
//! - `Frame::Radar`: 航空機の画面。CPU で描いた背景(地図と文字)は変わったときだけ転送し、
//!   回る線、残光、点はシェーダーで毎フレーム描く(PR4a)。
use crate::photo::{PhotoBuilder, SourceImage, program as link, texture as tex2d};
use glow::HasContext;
use std::time::Instant;

const VERT: &str = r"#version 300 es
out highp vec2 uv;
void main() {
    // 頂点 3 つで画面全体を覆う三角形。画像の 1 行目(uv.y = 0)が画面の上に来る
    vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    uv = vec2(p.x, 1.0 - p.y);
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
";

// 下地: 旧フレーム a と新フレーム b を t で混ぜる(フェードなしは a = b、t = 1)。
// CPU 経路の「旧を描いてから新を不透明度 t で重ねる」と同じ式。
// 2560 px 幅でも画素がずれないよう highp、拡大縮小しないので NEAREST
const FRAG_BASE: &str = r"#version 300 es
precision highp float;
uniform sampler2D a;
uniform sampler2D b;
uniform float t;
in highp vec2 uv;
out vec4 color;
void main() {
    color = vec4(mix(texture(a, uv).rgb, texture(b, uv).rgb, t), 1.0);
}
";

// 重ね絵: premultiplied の RGBA をそのまま出し、ブレンド(ONE, ONE_MINUS_SRC_ALPHA)で重ねる
const FRAG_OVERLAY: &str = r"#version 300 es
precision highp float;
uniform sampler2D tex;
in highp vec2 uv;
out vec4 color;
void main() {
    color = texture(tex, uv);
}
";

const VERT_CARD: &str = r"#version 300 es
uniform vec2 viewport;
uniform vec4 rect;
out highp vec2 uv;
void main() {
    vec2 p = gl_VertexID == 0 ? vec2(0.0, 0.0) : gl_VertexID == 1 ? vec2(1.0, 0.0) :
             gl_VertexID == 2 ? vec2(0.0, 1.0) : gl_VertexID == 3 ? vec2(0.0, 1.0) :
             gl_VertexID == 4 ? vec2(1.0, 0.0) : vec2(1.0, 1.0);
    // Card vertices use screen-space coordinates (p.y = 0 is the top), and
    // uploaded row zero is the source image's top row. Unlike the fullscreen
    // triangle, this geometry does not need a vertical UV flip.
    uv = p;
    vec2 px = rect.xy + p * rect.zw;
    gl_Position = vec4(2.0 * px.x / viewport.x - 1.0, 1.0 - 2.0 * px.y / viewport.y, 0.0, 1.0);
}
";

const FRAG_CARD: &str = r"#version 300 es
precision highp float;
uniform sampler2D tex;
uniform float alpha;
in highp vec2 uv;
out vec4 color;
void main() { color = texture(tex, uv) * alpha; }
";

const FRAG_SCRIM: &str = r"#version 300 es
precision highp float;
uniform float alpha;
out vec4 color;
void main() { color = vec4(0.0, 0.0, 0.0, alpha); }
";

/// Borrowed, premultiplied card pixels; this module intentionally has no render-crate dependency.
#[derive(Clone, Copy)]
pub struct AgentGpuCard<'a> {
    pub id: u64,
    pub revision: u64,
    pub pixels: &'a [u8],
    pub size: (u32, u32),
    /// (x, y, width, height), in screen pixels with top-left origin.
    pub rect: (f32, f32, f32, f32),
    pub alpha: f32,
}

const AGENT_CACHE: usize = 64;
struct AgentTexture {
    id: u64,
    revision: u64,
    size: (u32, u32),
    texture: glow::Texture,
}

// レーダー: 回る線と残光、線が通ると光って次の 1 周でゆっくり暗くなる点。加算で背景に重ねる。
// 座標は画面の px(左上が原点、y は下向き)。角度は北 0、時計回り
const FRAG_RADAR: &str = r"#version 300 es
precision highp float;
uniform vec2 size;
uniform vec2 center;
uniform float radius;
uniform float sweep;
uniform float trail;
uniform float scale;
uniform int n;
uniform vec3 blips[32];
out vec4 color;
const float TAU = 6.28318530718;
const vec3 GREEN = vec3(0.16, 1.0, 0.43);
float behind(vec2 d) {
    return mod(sweep - atan(d.x, -d.y), TAU);
}
void main() {
    vec2 p = vec2(gl_FragCoord.x, size.y - gl_FragCoord.y);
    vec2 d = p - center;
    float r = length(d);
    if (r > radius) discard;
    float b = behind(d);
    // 残光(CPU の sweep_cpu と同じく、線の直後で 0.45、TRAIL の角度で 1/e)
    vec3 c = GREEN * 0.45 * exp(-b / trail);
    // 線: 線からの距離 [px] で細く明るく。線の手前側もなめらかにする
    float perp = r * sin(min(b, TAU - b));
    float w = 1.6 * scale;
    c += vec3(0.6, 1.0, 0.7) * exp(-perp * perp / (w * w)) * step(min(b, TAU - b), 1.0);
    for (int i = 0; i < 32; i++) {
        if (i >= n) break;
        vec3 q = blips[i];
        float life = exp(-behind(q.xy - center) / (TAU * 0.55));
        int kind = int(q.z + 0.5);
        vec3 col = kind == 1 ? vec3(1.0, 0.75, 0.24) : (kind == 2 ? vec3(0.35, 0.59, 0.45) : vec3(0.43, 1.0, 0.59));
        float rr = (kind == 1 ? 11.0 : (kind == 2 ? 7.0 : 9.0)) * scale;
        vec2 e = p - q.xy;
        float d2 = dot(e, e);
        float glow = exp(-d2 / (rr * rr)) + 0.3 * exp(-d2 / (6.0 * rr * rr));
        c += col * glow * (0.3 + 1.0 * life);
    }
    color = vec4(c, 0.0);
}
";

/// レーダーの点の上限(シェーダーの配列の長さ)。
pub const RADAR_BLIPS: usize = 32;

/// 航空機の画面のレーダー。座標は画面の px(左上が原点)、角度は北 0、時計回り [rad]。
#[derive(Clone, Copy)]
pub struct Radar<'a> {
    pub center: (f32, f32),
    pub radius: f32,
    /// 回る線の角度
    pub sweep: f32,
    /// 残光の長さ(明るさが 1/e になる角度)
    pub trail: f32,
    /// 線の太さと点の大きさの倍率(1080 px の高さで 1)
    pub scale: f32,
    /// 点 (x, y, 種類)。種類は 0 = ほか、1 = いちばん近い機体、2 = 地上。`RADAR_BLIPS` を超えた分は描かない
    pub blips: &'a [[f32; 3]],
}

/// 写真のテクスチャを残しておく枚数(表示中とフェード元、と先読みの余裕)。
const PHOTO_CACHE: usize = 3;

/// 写真のフレームの中身。
#[derive(Clone, Copy)]
pub enum PhotoSource<'a> {
    /// CPU で合成した完成フレーム。premultiplied RGBA8、上から下、stride = 幅 × 4(画面と同じ大きさ、不透明)
    Frame(&'a [u8]),
    /// デコードしただけの写真。GPU で完成フレームを作る
    Image(SourceImage<'a>),
    /// 前回までに GPU に渡した(呼び出し側はもう画素を持っていない)
    Resident,
}

/// 写真のフレーム。`id` が同じなら同じ画像とみなし、転送し直さない。
#[derive(Clone, Copy)]
pub struct PhotoLayer<'a> {
    pub id: u64,
    pub src: PhotoSource<'a>,
}

/// 1 回の表示の内容。
pub enum Frame<'a> {
    /// CPU で描いた完成画面(不透明)
    Image(&'a [u8]),
    Photo {
        photo: PhotoLayer<'a>,
        /// フェード中の (旧フレーム, 新フレームの不透明度 0〜1)
        old: Option<(PhotoLayer<'a>, f32)>,
        /// 重ね絵が変わったときだけ Some(premultiplied RGBA8、透明背景)。None なら前回のものを使う
        overlay: Option<&'a [u8]>,
    },
    /// 航空機の画面
    Radar {
        /// 背景(不透明)が変わったときだけ Some。None なら前回のものを使う
        background: Option<&'a [u8]>,
        radar: Radar<'a>,
    },
    Agents {
        background: Option<&'a [u8]>,
        cards: &'a [AgentGpuCard<'a>],
        scrim_alpha: f32,
        focus_card_id: Option<u64>,
    },
}

pub struct Compositor {
    base: glow::Program,
    overlay: glow::Program,
    radar: glow::Program,
    card: glow::Program,
    scrim: glow::Program,
    vao: glow::VertexArray,
    /// `Frame::Radar` の背景(初めて使うときに作る)
    radar_bg: Option<glow::Texture>,
    agent_bg: Option<glow::Texture>,
    agent_cards: Vec<AgentTexture>,
    /// `Frame::Image` 用
    image_tex: glow::Texture,
    overlay_tex: glow::Texture,
    /// (写真の id, テクスチャ)。古いものから捨てる
    photos: Vec<(u64, glow::Texture)>,
    /// 重ね絵を 1 度でも転送したか(まだなら重ねない)
    overlay_ready: bool,
    size: (u32, u32),
    builder: PhotoBuilder,
    /// 今の時点までの転送量 [バイト](ログ用)
    pub uploaded: u64,
    /// Agent background and card uploads, useful for verifying that pose-only frames transfer no pixels.
    pub agent_uploaded: u64,
    /// 写真から完成フレームを作るのにかかった CPU 時間 [ms](転送と命令の発行。状態ログ用、読んだら空にする)
    pub build_ms: Vec<f32>,
}

fn program(gl: &glow::Context, frag: &str) -> Result<glow::Program, String> {
    link(gl, VERT, frag)
}

fn texture(gl: &glow::Context, size: (u32, u32)) -> Result<glow::Texture, String> {
    tex2d(gl, glow::RGBA8, size, glow::NEAREST)
}

fn card_texture(gl: &glow::Context, size: (u32, u32)) -> Result<glow::Texture, String> {
    // Card faces are routinely enlarged for the center focus animation. Keep
    // other compositor textures on NEAREST; only these cached surfaces need smoothing.
    tex2d(gl, glow::RGBA8, size, glow::LINEAR)
}

impl Compositor {
    pub fn new(gl: &glow::Context, size: (u32, u32)) -> Result<Self, String> {
        let base = program(gl, FRAG_BASE)?;
        let overlay = program(gl, FRAG_OVERLAY)?;
        let radar = program(gl, FRAG_RADAR)?;
        let card = link(gl, VERT_CARD, FRAG_CARD)?;
        let scrim = program(gl, FRAG_SCRIM)?;
        unsafe {
            gl.use_program(Some(base));
            gl.uniform_1_i32(gl.get_uniform_location(base, "a").as_ref(), 0);
            gl.uniform_1_i32(gl.get_uniform_location(base, "b").as_ref(), 1);
            gl.use_program(Some(overlay));
            gl.uniform_1_i32(gl.get_uniform_location(overlay, "tex").as_ref(), 0);
            gl.use_program(Some(card));
            gl.uniform_1_i32(gl.get_uniform_location(card, "tex").as_ref(), 0);
            Ok(Self {
                base,
                overlay,
                radar,
                card,
                scrim,
                vao: gl.create_vertex_array()?,
                radar_bg: None,
                agent_bg: None,
                agent_cards: Vec::new(),
                image_tex: texture(gl, size)?,
                overlay_tex: texture(gl, size)?,
                photos: Vec::new(),
                overlay_ready: false,
                size,
                builder: PhotoBuilder::new(gl)?,
                uploaded: 0,
                agent_uploaded: 0,
                build_ms: Vec::new(),
            })
        }
    }

    fn upload(&mut self, gl: &glow::Context, tex: glow::Texture, rgba: &[u8]) {
        let (w, h) = self.size;
        assert_eq!(
            rgba.len(),
            w as usize * h as usize * 4,
            "画像の大きさが違う"
        );
        unsafe {
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
            gl.tex_sub_image_2d(
                glow::TEXTURE_2D,
                0,
                0,
                0,
                w as i32,
                h as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(rgba)),
            );
        }
        self.uploaded += rgba.len() as u64;
    }

    /// GPU で扱える写真の最大の幅と高さ。
    pub fn max_texture(&self) -> u32 {
        self.builder.max_texture
    }

    /// 写真のテクスチャを返す(初めての id なら転送するか、GPU で作る)。
    fn photo_tex(
        &mut self,
        gl: &glow::Context,
        layer: PhotoLayer<'_>,
    ) -> Result<glow::Texture, String> {
        if let Some(i) = self.photos.iter().position(|(id, _)| *id == layer.id) {
            // 最近使ったものを後ろへ
            let e = self.photos.remove(i);
            self.photos.push(e);
            return Ok(e.1);
        }
        let tex = match layer.src {
            PhotoSource::Frame(rgba) => {
                let tex = texture(gl, self.size)?;
                self.upload(gl, tex, rgba);
                tex
            }
            PhotoSource::Image(img) => {
                let t0 = Instant::now();
                let prev = unsafe { gl.get_parameter_framebuffer(glow::FRAMEBUFFER_BINDING) };
                let built = self.builder.build(gl, self.vao, &img, self.size);
                unsafe { gl.bind_framebuffer(glow::FRAMEBUFFER, prev) };
                let tex = built?;
                self.uploaded += img.pixels.len() as u64;
                self.build_ms.push(t0.elapsed().as_secs_f32() * 1000.0);
                tex
            }
            PhotoSource::Resident => {
                return Err(format!("写真 {} のテクスチャが残っていない", layer.id));
            }
        };
        if self.photos.len() >= PHOTO_CACHE {
            unsafe { gl.delete_texture(self.photos.remove(0).1) };
        }
        self.photos.push((layer.id, tex));
        Ok(tex)
    }

    fn draw_base(&self, gl: &glow::Context, a: glow::Texture, b: glow::Texture, t: f32) {
        unsafe {
            gl.disable(glow::BLEND);
            gl.use_program(Some(self.base));
            gl.uniform_1_f32(gl.get_uniform_location(self.base, "t").as_ref(), t);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(a));
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, Some(b));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_vertex_array(Some(self.vao));
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
        }
    }

    /// 今バインドされている framebuffer に `frame` を描く。
    pub fn draw(&mut self, gl: &glow::Context, frame: &Frame<'_>) -> Result<(), String> {
        let (w, h) = self.size;
        let viewport = || unsafe { gl.viewport(0, 0, w as i32, h as i32) };
        match frame {
            Frame::Image(rgba) => {
                viewport();
                self.upload(gl, self.image_tex, rgba);
                self.draw_base(gl, self.image_tex, self.image_tex, 1.0);
            }
            Frame::Photo {
                photo,
                old,
                overlay,
            } => {
                // 写真を GPU で作るときは別の framebuffer に描くので、表示先への描画より先に済ませる
                let new = self.photo_tex(gl, *photo)?;
                let old = match old {
                    Some((o, t)) => Some((self.photo_tex(gl, *o)?, t.clamp(0.0, 1.0))),
                    None => None,
                };
                viewport();
                match old {
                    Some((old, t)) => self.draw_base(gl, old, new, t),
                    None => self.draw_base(gl, new, new, 1.0),
                }
                if let Some(rgba) = overlay {
                    self.upload(gl, self.overlay_tex, rgba);
                    self.overlay_ready = true;
                }
                if !self.overlay_ready {
                    return Ok(());
                }
                unsafe {
                    gl.enable(glow::BLEND);
                    gl.blend_func(glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
                    gl.use_program(Some(self.overlay));
                    gl.bind_texture(glow::TEXTURE_2D, Some(self.overlay_tex));
                    gl.draw_arrays(glow::TRIANGLES, 0, 3);
                    gl.disable(glow::BLEND);
                }
            }
            Frame::Radar { background, radar } => {
                viewport();
                self.draw_radar(gl, *background, radar)?;
            }
            Frame::Agents {
                background,
                cards,
                scrim_alpha,
                focus_card_id,
            } => {
                viewport();
                self.draw_agents(gl, *background, cards, *scrim_alpha, *focus_card_id)?;
            }
        }
        Ok(())
    }

    fn draw_agents(
        &mut self,
        gl: &glow::Context,
        background: Option<&[u8]>,
        cards: &[AgentGpuCard<'_>],
        scrim_alpha: f32,
        focus_card_id: Option<u64>,
    ) -> Result<(), String> {
        let bg = match (self.agent_bg, background) {
            (Some(t), _) => t,
            (None, Some(_)) => {
                let t = texture(gl, self.size)?;
                self.agent_bg = Some(t);
                t
            }
            (None, None) => return Err("agent background is not initialized".into()),
        };
        if let Some(pixels) = background {
            let expected = self.size.0 as usize * self.size.1 as usize * 4;
            if pixels.len() != expected {
                return Err("agent background size does not match compositor".into());
            }
            self.upload(gl, bg, pixels);
            self.agent_uploaded += pixels.len() as u64;
        }
        self.draw_base(gl, bg, bg, 1.0);
        let shade = if scrim_alpha.is_finite() {
            scrim_alpha.clamp(0.0, 1.0)
        } else {
            0.0
        };
        if shade > 0.0 {
            unsafe {
                gl.enable(glow::BLEND);
                gl.blend_func(glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
                gl.use_program(Some(self.scrim));
                gl.uniform_1_f32(gl.get_uniform_location(self.scrim, "alpha").as_ref(), shade);
                gl.bind_vertex_array(Some(self.vao));
                gl.draw_arrays(glow::TRIANGLES, 0, 3);
                gl.disable(glow::BLEND);
            }
        }
        let live: std::collections::HashSet<u64> =
            cards.iter().take(AGENT_CACHE).map(|c| c.id).collect();
        let mut index = 0;
        while index < self.agent_cards.len() {
            if !live.contains(&self.agent_cards[index].id) {
                unsafe {
                    gl.delete_texture(self.agent_cards.remove(index).texture);
                }
            } else {
                index += 1;
            }
        }
        for card in cards.iter().take(AGENT_CACHE) {
            if card.size.0 == 0
                || card.size.1 == 0
                || card.pixels.len() != card.size.0 as usize * card.size.1 as usize * 4
            {
                return Err(format!("agent card {} has invalid surface size", card.id));
            }
            let found = self.agent_cards.iter().position(|c| c.id == card.id);
            if found.is_none_or(|i| self.agent_cards[i].size != card.size) {
                if let Some(i) = found {
                    unsafe {
                        gl.delete_texture(self.agent_cards.remove(i).texture);
                    }
                }
                let tex = card_texture(gl, card.size)?;
                self.agent_cards.push(AgentTexture {
                    id: card.id,
                    revision: u64::MAX,
                    size: card.size,
                    texture: tex,
                });
            }
            let i = self
                .agent_cards
                .iter()
                .position(|c| c.id == card.id)
                .ok_or("agent texture cache insertion failed")?;
            if self.agent_cards[i].revision != card.revision {
                unsafe {
                    gl.bind_texture(glow::TEXTURE_2D, Some(self.agent_cards[i].texture));
                    gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
                    gl.tex_sub_image_2d(
                        glow::TEXTURE_2D,
                        0,
                        0,
                        0,
                        card.size.0 as i32,
                        card.size.1 as i32,
                        glow::RGBA,
                        glow::UNSIGNED_BYTE,
                        glow::PixelUnpackData::Slice(Some(card.pixels)),
                    );
                }
                self.uploaded += card.pixels.len() as u64;
                self.agent_uploaded += card.pixels.len() as u64;
                self.agent_cards[i].revision = card.revision;
            }
            if !card.rect.0.is_finite()
                || !card.rect.1.is_finite()
                || !card.rect.2.is_finite()
                || !card.rect.3.is_finite()
                || !card.alpha.is_finite()
            {
                continue;
            }
            let alpha = card.alpha.clamp(0.0, 1.0)
                * if shade > 0.0 && focus_card_id != Some(card.id) {
                    1.0 - shade
                } else {
                    1.0
                };
            if card.rect.2 <= 0.0 || card.rect.3 <= 0.0 || alpha <= 0.0 {
                continue;
            }
            unsafe {
                gl.enable(glow::BLEND);
                gl.blend_func(glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
                gl.use_program(Some(self.card));
                gl.uniform_2_f32(
                    gl.get_uniform_location(self.card, "viewport").as_ref(),
                    self.size.0 as f32,
                    self.size.1 as f32,
                );
                gl.uniform_4_f32(
                    gl.get_uniform_location(self.card, "rect").as_ref(),
                    card.rect.0,
                    card.rect.1,
                    card.rect.2,
                    card.rect.3,
                );
                gl.uniform_1_f32(gl.get_uniform_location(self.card, "alpha").as_ref(), alpha);
                gl.active_texture(glow::TEXTURE0);
                gl.bind_texture(glow::TEXTURE_2D, Some(self.agent_cards[i].texture));
                gl.bind_vertex_array(Some(self.vao));
                gl.draw_arrays(glow::TRIANGLES, 0, 6);
                gl.disable(glow::BLEND);
            }
        }
        Ok(())
    }

    fn draw_radar(
        &mut self,
        gl: &glow::Context,
        background: Option<&[u8]>,
        radar: &Radar<'_>,
    ) -> Result<(), String> {
        let bg = match (self.radar_bg, background) {
            (Some(t), _) => t,
            (None, Some(_)) => {
                let t = texture(gl, self.size)?;
                self.radar_bg = Some(t);
                t
            }
            (None, None) => return Err("レーダーの背景がまだ無い".into()),
        };
        if let Some(rgba) = background {
            self.upload(gl, bg, rgba);
        }
        self.draw_base(gl, bg, bg, 1.0);
        let n = radar.blips.len().min(RADAR_BLIPS);
        let flat: Vec<f32> = radar.blips[..n].iter().flatten().copied().collect();
        let p = self.radar;
        unsafe {
            let loc = |name: &str| gl.get_uniform_location(p, name);
            gl.use_program(Some(p));
            gl.uniform_2_f32(loc("size").as_ref(), self.size.0 as f32, self.size.1 as f32);
            gl.uniform_2_f32(loc("center").as_ref(), radar.center.0, radar.center.1);
            gl.uniform_1_f32(loc("radius").as_ref(), radar.radius);
            gl.uniform_1_f32(loc("sweep").as_ref(), radar.sweep);
            gl.uniform_1_f32(loc("trail").as_ref(), radar.trail.max(0.01));
            gl.uniform_1_f32(loc("scale").as_ref(), radar.scale);
            gl.uniform_1_i32(loc("n").as_ref(), n as i32);
            if n > 0 {
                gl.uniform_3_f32_slice(loc("blips").as_ref(), &flat);
            }
            gl.enable(glow::BLEND);
            gl.blend_func(glow::ONE, glow::ONE);
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
            gl.disable(glow::BLEND);
        }
        Ok(())
    }

    pub fn destroy(&self, gl: &glow::Context) {
        unsafe {
            for (_, t) in &self.photos {
                gl.delete_texture(*t);
            }
            gl.delete_texture(self.image_tex);
            gl.delete_texture(self.overlay_tex);
            if let Some(t) = self.radar_bg {
                gl.delete_texture(t);
            }
            if let Some(t) = self.agent_bg {
                gl.delete_texture(t);
            }
            for card in &self.agent_cards {
                gl.delete_texture(card.texture);
            }
            gl.delete_vertex_array(self.vao);
            gl.delete_program(self.base);
            gl.delete_program(self.overlay);
            gl.delete_program(self.radar);
            gl.delete_program(self.card);
            gl.delete_program(self.scrim);
        }
        self.builder.destroy(gl);
    }
}
