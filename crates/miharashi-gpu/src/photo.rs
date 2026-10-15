//! 写真の完成フレームを GPU で作る(PoC-005 PR3b)。
//! 取得スレッドはデコードだけを行い、ここで写真 1 枚につき 1 回、縮小・ぼかし背景・影・配置を
//! 画面と同じ大きさのテクスチャに描く。見た目を CPU 経路(`miharashi_render::photo::compose_image`)に
//! 合わせるため、同じ手順(Lanczos3 の縮小、1/16 の縮小画像に箱型ぼかし 3 回、双一次の拡大、
//! 明るさ 0.42 倍、10 段の影)をシェーダーで再現する。
use glow::HasContext;

/// 全画面の三角形(compose.rs と同じ)。各パスは `gl_FragCoord` を画素の位置として使う
const VERT: &str = r"#version 300 es
void main() {
    vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
";

// 1 方向の縮小(image クレートの `resize` と同じ標本化)。axis 0 は横、1 は縦。
// 出力の画素 o は、拡大縮小後の長さ out_len の画像の (o + offset) 番目(中央切り抜き用)。
// kernel 0 は Lanczos3、1 は面積平均(`thumbnail` の代わり)
const FRAG_RESAMPLE: &str = r"#version 300 es
precision highp float;
precision highp int;
uniform highp sampler2D src;
uniform int axis;
uniform int in_len;
uniform float out_len;
uniform int offset;
uniform int kernel;
out vec4 color;
const float PI = 3.14159265358979;
float sinc(float t) {
    if (t == 0.0) return 1.0;
    float a = t * PI;
    return sin(a) / a;
}
void main() {
    ivec2 o = ivec2(gl_FragCoord.xy);
    int oi = (axis == 0 ? o.x : o.y) + offset;
    float ratio = float(in_len) / out_len;
    float sratio = max(ratio, 1.0);
    float c = (float(oi) + 0.5) * ratio;
    vec4 sum = vec4(0.0);
    float wsum = 0.0;
    if (kernel == 0) {
        float support = 3.0 * sratio;
        int left = clamp(int(floor(c - support)), 0, in_len - 1);
        int right = clamp(int(ceil(c + support)), left + 1, in_len);
        float center = c - 0.5;
        for (int i = left; i < right; i++) {
            float x = (float(i) - center) / sratio;
            float w = abs(x) < 3.0 ? sinc(x) * sinc(x / 3.0) : 0.0;
            sum += w * texelFetch(src, axis == 0 ? ivec2(i, o.y) : ivec2(o.x, i), 0);
            wsum += w;
        }
    } else {
        float lo = float(oi) * ratio;
        float hi = lo + ratio;
        int left = clamp(int(floor(lo)), 0, in_len - 1);
        int right = clamp(int(ceil(hi)), left + 1, in_len);
        for (int i = left; i < right; i++) {
            float w = max(min(hi, float(i + 1)) - max(lo, float(i)), 0.0);
            sum += w * texelFetch(src, axis == 0 ? ivec2(i, o.y) : ivec2(o.x, i), 0);
            wsum += w;
        }
    }
    color = sum / wsum;
}
";

// 箱型ぼかし(半径 3、端は端の画素で延長)を 1 方向にかける。CPU の `box_blur` の 1 回分の半分
const FRAG_BLUR: &str = r"#version 300 es
precision highp float;
precision highp int;
uniform highp sampler2D src;
uniform int axis;
out vec4 color;
void main() {
    ivec2 p = ivec2(gl_FragCoord.xy);
    ivec2 last = textureSize(src, 0) - 1;
    vec4 s = vec4(0.0);
    for (int k = -3; k <= 3; k++) {
        ivec2 q = axis == 0 ? ivec2(clamp(p.x + k, 0, last.x), p.y)
                            : ivec2(p.x, clamp(p.y + k, 0, last.y));
        s += texelFetch(src, q, 0);
    }
    color = s / 7.0;
}
";

// 仕上げ: ぼかし背景を双一次で拡大して暗くし、影を落として写真を置く(cover は写真だけ)。
// 値は CPU 経路と同じく 0〜255 の整数に丸めながら計算する
const FRAG_FINISH: &str = r"#version 300 es
precision highp float;
precision highp int;
uniform highp sampler2D bg;
uniform highp sampler2D photo;
uniform int cover;
uniform ivec4 rect;
uniform float shadow_step;
uniform vec2 size;
out vec4 color;
void main() {
    ivec2 p = ivec2(gl_FragCoord.xy);
    if (cover == 1) {
        vec4 s = texelFetch(photo, p, 0);
        color = vec4(s.rgb * s.a, 1.0);
        return;
    }
    vec3 b = round(texture(bg, gl_FragCoord.xy / size).rgb * 255.0);
    b = floor(b * 0.42);
    // 影: 写真を d = i × shadow_step だけ広げた矩形(i = 1..10)を、黒の不透明度 9/255 で重ねる
    int n = 0;
    for (int i = 1; i <= 10; i++) {
        float d = float(i) * shadow_step;
        vec2 lo = vec2(rect.xy) - d;
        vec2 hi = vec2(rect.xy + rect.zw) + d;
        if (all(greaterThan(gl_FragCoord.xy, lo)) && all(lessThan(gl_FragCoord.xy, hi))) n++;
    }
    for (int k = 0; k < n; k++) b = floor((b * 246.0 + 255.0) / 256.0);
    vec3 c = b / 255.0;
    if (all(greaterThanEqual(p, rect.xy)) && all(lessThan(p, rect.xy + rect.zw))) {
        vec4 s = texelFetch(photo, p - rect.xy, 0);
        c = s.rgb * s.a + c * (1.0 - s.a);
    }
    color = vec4(c, 1.0);
}
";

/// 写真の置き方(`miharashi_render::photo_layout::PhotoLayout` から作る)。
#[derive(Clone, Copy, Debug)]
pub struct Placement {
    /// true なら画面いっぱいに中央切り抜き。false なら `rect` に写真を置き、周りはぼかし背景
    pub cover: bool,
    /// 写真の位置と大きさ (x, y, w, h)
    pub rect: (u32, u32, u32, u32),
    /// 画面の倍率(高さ 1080 で 1.0)。影の間隔に使う
    pub scale: f32,
}

/// デコードしただけの写真。
#[derive(Clone, Copy)]
pub struct SourceImage<'a> {
    /// RGB8(`channels` = 3)か straight RGBA8(4)、上から下、詰めて並べる
    pub pixels: &'a [u8],
    pub size: (u32, u32),
    pub channels: u32,
    pub place: Placement,
}

/// CPU 経路の `cover` と同じ、中央切り抜き前の拡大縮小後の大きさ。
fn cover_size((iw, ih): (u32, u32), (w, h): (u32, u32)) -> (u32, u32) {
    let (iw, ih) = (iw as f32, ih as f32);
    let scale = (w as f32 / iw).max(h as f32 / ih);
    let (rw, rh) = ((iw * scale).ceil() as u32, (ih * scale).ceil() as u32);
    (rw.max(w), rh.max(h))
}

/// image クレートの `thumbnail` と同じ、縦横比を保って (nw, nh) に収まる大きさ。
fn fit_size((w, h): (u32, u32), (nw, nh): (u32, u32)) -> (u32, u32) {
    let ratio = (nw as f64 / w as f64).min(nh as f64 / h as f64);
    let r = |v: u32| ((v as f64 * ratio).round() as u32).max(1);
    (r(w), r(h))
}

pub(crate) fn texture(
    gl: &glow::Context,
    internal: u32,
    (w, h): (u32, u32),
    filter: u32,
) -> Result<glow::Texture, String> {
    unsafe {
        let tex = gl.create_texture()?;
        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        gl.tex_storage_2d(glow::TEXTURE_2D, 1, internal, w as i32, h as i32);
        for (k, v) in [
            (glow::TEXTURE_MIN_FILTER, filter),
            (glow::TEXTURE_MAG_FILTER, filter),
            (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
            (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
        ] {
            gl.tex_parameter_i32(glow::TEXTURE_2D, k, v as i32);
        }
        Ok(tex)
    }
}

pub(crate) fn program(gl: &glow::Context, vert: &str, frag: &str) -> Result<glow::Program, String> {
    unsafe {
        let program = gl.create_program()?;
        for (kind, src) in [(glow::VERTEX_SHADER, vert), (glow::FRAGMENT_SHADER, frag)] {
            let s = gl.create_shader(kind)?;
            gl.shader_source(s, src);
            gl.compile_shader(s);
            if !gl.get_shader_compile_status(s) {
                return Err(format!("shader: {}", gl.get_shader_info_log(s)));
            }
            gl.attach_shader(program, s);
            gl.delete_shader(s);
        }
        gl.link_program(program);
        if !gl.get_program_link_status(program) {
            return Err(format!("link: {}", gl.get_program_info_log(program)));
        }
        Ok(program)
    }
}

pub struct PhotoBuilder {
    resample: glow::Program,
    blur: glow::Program,
    finish: glow::Program,
    fbo: glow::Framebuffer,
    /// 縦方向の縮小の途中結果の形式(RGBA16F に描けなければ RGBA8)
    mid_format: u32,
    pub max_texture: u32,
}

impl PhotoBuilder {
    pub fn new(gl: &glow::Context) -> Result<Self, String> {
        let resample = program(gl, VERT, FRAG_RESAMPLE)?;
        let blur = program(gl, VERT, FRAG_BLUR)?;
        let finish = program(gl, VERT, FRAG_FINISH)?;
        unsafe {
            gl.use_program(Some(finish));
            gl.uniform_1_i32(gl.get_uniform_location(finish, "bg").as_ref(), 0);
            gl.uniform_1_i32(gl.get_uniform_location(finish, "photo").as_ref(), 1);
            let mut b = Self {
                resample,
                blur,
                finish,
                fbo: gl.create_framebuffer()?,
                mid_format: glow::RGBA16F,
                max_texture: gl.get_parameter_i32(glow::MAX_TEXTURE_SIZE).max(0) as u32,
            };
            // GLES 3.2 は浮動小数点のテクスチャに描けるはずだが、念のため確かめる
            let t = texture(gl, glow::RGBA16F, (4, 4), glow::NEAREST)?;
            let prev = gl.get_parameter_framebuffer(glow::FRAMEBUFFER_BINDING);
            b.target(gl, t);
            if gl.check_framebuffer_status(glow::FRAMEBUFFER) != glow::FRAMEBUFFER_COMPLETE {
                b.mid_format = glow::RGBA8;
            }
            gl.bind_framebuffer(glow::FRAMEBUFFER, prev);
            gl.delete_texture(t);
            Ok(b)
        }
    }

    /// このパスの出力先を `tex` にする。
    fn target(&self, gl: &glow::Context, tex: glow::Texture) {
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(tex),
                0,
            );
        }
    }

    fn run(&self, gl: &glow::Context, dst: glow::Texture, (w, h): (u32, u32)) {
        self.target(gl, dst);
        unsafe {
            gl.viewport(0, 0, w as i32, h as i32);
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
        }
    }

    /// `src`(in_size)を、拡大縮小後の大きさ `full` の画像の `crop` から `out` の範囲として作る。
    /// 縦 → 横の順(image クレートと同じ)。戻り値は RGBA8 のテクスチャ。
    #[allow(clippy::too_many_arguments)]
    fn resample(
        &self,
        gl: &glow::Context,
        src: glow::Texture,
        in_size: (u32, u32),
        full: (u32, u32),
        crop: (u32, u32),
        out: (u32, u32),
        kernel: i32,
    ) -> Result<glow::Texture, String> {
        let p = self.resample;
        let mid = texture(gl, self.mid_format, (in_size.0, out.1), glow::NEAREST)?;
        let dst = texture(gl, glow::RGBA8, out, glow::NEAREST)?;
        unsafe {
            gl.use_program(Some(p));
            let u = |n: &str| gl.get_uniform_location(p, n);
            gl.uniform_1_i32(u("kernel").as_ref(), kernel);
            for (axis, s, i, f, c, size) in [
                (1, src, in_size.1, full.1, crop.1, (in_size.0, out.1)),
                (0, mid, in_size.0, full.0, crop.0, out),
            ] {
                gl.active_texture(glow::TEXTURE0);
                gl.bind_texture(glow::TEXTURE_2D, Some(s));
                gl.uniform_1_i32(u("axis").as_ref(), axis);
                gl.uniform_1_i32(u("in_len").as_ref(), i as i32);
                gl.uniform_1_f32(u("out_len").as_ref(), f as f32);
                gl.uniform_1_i32(u("offset").as_ref(), c as i32);
                self.run(gl, if axis == 1 { mid } else { dst }, size);
            }
            gl.delete_texture(mid);
        }
        Ok(dst)
    }

    /// 写真の完成フレーム(画面の大きさ、RGBA8、1 行目が画面の上)を作る。
    /// 今の framebuffer の割り当ては呼び出し側で戻す。
    pub fn build(
        &self,
        gl: &glow::Context,
        vao: glow::VertexArray,
        img: &SourceImage<'_>,
        size: (u32, u32),
    ) -> Result<glow::Texture, String> {
        let (iw, ih) = img.size;
        let (fmt, internal) = match img.channels {
            3 => (glow::RGB, glow::RGB8),
            4 => (glow::RGBA, glow::RGBA8),
            n => return Err(format!("写真のチャンネル数 {n} には対応していない")),
        };
        if iw.max(ih) > self.max_texture {
            return Err(format!(
                "写真 {iw}x{ih} が GPU の上限 {} を超える",
                self.max_texture
            ));
        }
        assert_eq!(
            img.pixels.len(),
            (iw * ih * img.channels) as usize,
            "写真の大きさが違う"
        );
        let src = texture(gl, internal, img.size, glow::NEAREST)?;
        unsafe {
            gl.disable(glow::BLEND);
            gl.bind_vertex_array(Some(vao));
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            gl.tex_sub_image_2d(
                glow::TEXTURE_2D,
                0,
                0,
                0,
                iw as i32,
                ih as i32,
                fmt,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(img.pixels)),
            );
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
        }
        let pl = img.place;
        let (w, h) = size;
        let mut temps = vec![src];
        let (bg, photo) = if pl.cover {
            let full = cover_size(img.size, size);
            let crop = ((full.0 - w) / 2, (full.1 - h) / 2);
            let photo = self.resample(gl, src, img.size, full, crop, size, 0)?;
            (None, photo)
        } else {
            // ぼかし背景: 縮小画像 → 1/16 の大きさに中央切り抜き → 箱型ぼかし 3 回
            let small = ((w / 16).max(2), (h / 16).max(2));
            let thumb = fit_size(img.size, (small.0 * 4, small.1 * 4));
            let t = self.resample(gl, src, img.size, thumb, (0, 0), thumb, 1)?;
            temps.push(t);
            let full = cover_size(thumb, small);
            let crop = ((full.0 - small.0) / 2, (full.1 - small.1) / 2);
            let bg = self.resample(gl, t, thumb, full, crop, small, 0)?;
            let tmp = texture(gl, glow::RGBA8, small, glow::NEAREST)?;
            temps.push(tmp);
            unsafe {
                gl.use_program(Some(self.blur));
                let axis = gl.get_uniform_location(self.blur, "axis");
                gl.active_texture(glow::TEXTURE0);
                for _ in 0..3 {
                    for (a, s, d) in [(0, bg, tmp), (1, tmp, bg)] {
                        gl.bind_texture(glow::TEXTURE_2D, Some(s));
                        gl.uniform_1_i32(axis.as_ref(), a);
                        self.run(gl, d, small);
                    }
                }
                gl.bind_texture(glow::TEXTURE_2D, Some(bg));
                for k in [glow::TEXTURE_MIN_FILTER, glow::TEXTURE_MAG_FILTER] {
                    gl.tex_parameter_i32(glow::TEXTURE_2D, k, glow::LINEAR as i32);
                }
            }
            let (pw, ph) = (pl.rect.2, pl.rect.3);
            let photo = self.resample(gl, src, img.size, (pw, ph), (0, 0), (pw, ph), 0)?;
            (Some(bg), photo)
        };
        temps.push(photo);
        temps.extend(bg);
        let frame = texture(gl, glow::RGBA8, size, glow::NEAREST)?;
        unsafe {
            let p = self.finish;
            gl.use_program(Some(p));
            let u = |n: &str| gl.get_uniform_location(p, n);
            gl.uniform_1_i32(u("cover").as_ref(), pl.cover as i32);
            let (x, y, rw, rh) = pl.rect;
            gl.uniform_4_i32(u("rect").as_ref(), x as i32, y as i32, rw as i32, rh as i32);
            gl.uniform_1_f32(u("shadow_step").as_ref(), 3.0 * pl.scale);
            gl.uniform_2_f32(u("size").as_ref(), w as f32, h as f32);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, bg.or(Some(photo)));
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, Some(photo));
            gl.active_texture(glow::TEXTURE0);
            self.run(gl, frame, size);
            for t in temps {
                gl.delete_texture(t);
            }
        }
        Ok(frame)
    }

    pub fn destroy(&self, gl: &glow::Context) {
        unsafe {
            gl.delete_framebuffer(self.fbo);
            gl.delete_program(self.resample);
            gl.delete_program(self.blur);
            gl.delete_program(self.finish);
        }
    }
}
