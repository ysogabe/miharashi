//! Linux 専用: ffmpeg(libavformat / libavcodec)で動画を読む。2 通りある。
//! - `Decoder`: VA-API(表示カードの内蔵 GPU)でデコードして、1 枚ずつ dmabuf(DRM PRIME)の形で渡す(PoC-005 PR4 B2)。
//!   画素は GPU のメモリに置いたまま CPU に読み出さない。ソフトウェアデコードには戻らない(VA-API が使えなければ開けない)。
//!   GL の場面(`miharashi-gpu` の `VideoScene`)専用で、CPU の `Canvas` には載せられない。
//! - `CpuDecoder`: ソフトウェアでデコードして RGBA の画素列にする(だいだい画面のアバター、Issue #99)。
//!   GPU・VA-API・swscale を使わず、CPU の描画経路(`renderer = cpu` でも)に載せる小さな動画向け。
use ffmpeg_sys_next as ff;
use std::ffi::{CStr, CString};
use std::os::fd::RawFd;
use std::path::Path;
use std::ptr;

/// DRM の fourcc
const DRM_NV12: u32 = u32::from_le_bytes(*b"NV12");
const DRM_R8: u32 = u32::from_le_bytes(*b"R8  ");
const DRM_GR88: u32 = u32::from_le_bytes(*b"GR88");

/// fourcc を文字にする(ログ用)
pub fn fourcc_name(f: u32) -> String {
    String::from_utf8_lossy(&f.to_le_bytes())
        .trim_end()
        .to_string()
}

/// dmabuf の 1 枚(1 プレーン)。NV12 は Y(R8)と UV(GR88)の 2 枚になる。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layer {
    pub fd: RawFd,
    /// DRM の fourcc
    pub fourcc: u32,
    pub width: u32,
    pub height: u32,
    pub offset: u32,
    pub pitch: u32,
    pub modifier: u64,
}

/// デコードした 1 枚。drop するとサーフェスをデコーダーに返す(GPU が読み終えてから捨てる)。
pub struct Frame {
    src: *mut ff::AVFrame,
    mapped: *mut ff::AVFrame,
}

impl Frame {
    /// 各層(プレーンを 1 枚ずつ)。1 層に複数プレーンがある形(NV12 を 1 層で渡す形)は分けて返す。
    pub fn layers(&self) -> Vec<Layer> {
        // SAFETY: DRM_PRIME に写したフレームの data[0] は AVDRMFrameDescriptor
        unsafe {
            let d = &*((*self.mapped).data[0] as *const ff::AVDRMFrameDescriptor);
            let (w, h) = ((*self.src).width as u32, (*self.src).height as u32);
            let mut out = Vec::new();
            for l in &d.layers[..d.nb_layers as usize] {
                for (i, p) in l.planes[..l.nb_planes as usize].iter().enumerate() {
                    let obj = &d.objects[p.object_index as usize];
                    // 最初のプレーン(Y)は動画と同じ大きさ、それ以降(UV)は縦横半分
                    let (lw, lh) = if out.is_empty() {
                        (w, h)
                    } else {
                        (w.div_ceil(2), h.div_ceil(2))
                    };
                    // NV12 を 1 層で渡されたら、Y を R8、UV を GR88 の 1 枚ずつに分ける
                    let fourcc = match (l.format, i) {
                        (DRM_NV12, 0) => DRM_R8,
                        (DRM_NV12, _) => DRM_GR88,
                        (f, _) => f,
                    };
                    out.push(Layer {
                        fd: obj.fd,
                        fourcc,
                        width: lw,
                        height: lh,
                        offset: p.offset as u32,
                        pitch: p.pitch as u32,
                        modifier: obj.format_modifier,
                    });
                }
            }
            out
        }
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        // SAFETY: どちらも av_frame_alloc で作り、ほかで解放していない
        unsafe {
            ff::av_frame_free(&mut self.mapped);
            ff::av_frame_free(&mut self.src);
        }
    }
}

/// 動画ファイルを VA-API でデコードする。最後まで読んだら先頭に戻って繰り返す。
pub struct Decoder {
    fmt: *mut ff::AVFormatContext,
    codec: *mut ff::AVCodecContext,
    hw: *mut ff::AVBufferRef,
    pkt: *mut ff::AVPacket,
    stream: i32,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// 何周目か(0 から)
    pub loops: u32,
    /// 最後まで読んで、デコーダーに残りを吐き出させているところ
    draining: bool,
    pub codec_name: String,
}

extern "C" fn pick_vaapi(
    _: *mut ff::AVCodecContext,
    mut fmts: *const ff::AVPixelFormat,
) -> ff::AVPixelFormat {
    // SAFETY: ffmpeg が AV_PIX_FMT_NONE で終わる配列を渡す
    unsafe {
        while *fmts != ff::AVPixelFormat::AV_PIX_FMT_NONE {
            if *fmts == ff::AVPixelFormat::AV_PIX_FMT_VAAPI {
                return *fmts;
            }
            fmts = fmts.add(1);
        }
    }
    ff::AVPixelFormat::AV_PIX_FMT_NONE
}

fn err(what: &str, code: i32) -> String {
    let mut buf = [0 as std::ffi::c_char; 128];
    // SAFETY: buf の大きさを渡している
    unsafe { ff::av_strerror(code, buf.as_mut_ptr(), buf.len()) };
    let msg = unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy();
    format!("{what}: {msg}")
}

impl Decoder {
    /// `path` を開き、`render_node`(表示カードの /dev/dri/renderD*)の VA-API でデコードする準備をする。
    pub fn open(path: &Path, render_node: &Path) -> Result<Self, String> {
        let cpath = CString::new(path.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
        let cnode =
            CString::new(render_node.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
        let mut d = Self {
            fmt: ptr::null_mut(),
            codec: ptr::null_mut(),
            hw: ptr::null_mut(),
            pkt: ptr::null_mut(),
            stream: -1,
            width: 0,
            height: 0,
            fps: 0.0,
            loops: 0,
            draining: false,
            codec_name: String::new(),
        };
        // 途中で失敗しても Drop が後片付けする
        unsafe {
            let r =
                ff::avformat_open_input(&mut d.fmt, cpath.as_ptr(), ptr::null(), ptr::null_mut());
            if r < 0 {
                return Err(err(&path.display().to_string(), r));
            }
            let r = ff::avformat_find_stream_info(d.fmt, ptr::null_mut());
            if r < 0 {
                return Err(err("avformat_find_stream_info", r));
            }
            let mut dec: *const ff::AVCodec = ptr::null();
            let s = ff::av_find_best_stream(
                d.fmt,
                ff::AVMediaType::AVMEDIA_TYPE_VIDEO,
                -1,
                -1,
                &mut dec,
                0,
            );
            if s < 0 {
                return Err(err("動画のストリームが無い", s));
            }
            d.stream = s;
            let st = *(*d.fmt).streams.add(s as usize);
            d.codec = ff::avcodec_alloc_context3(dec);
            if d.codec.is_null() {
                return Err("avcodec_alloc_context3".into());
            }
            let r = ff::avcodec_parameters_to_context(d.codec, (*st).codecpar);
            if r < 0 {
                return Err(err("avcodec_parameters_to_context", r));
            }
            let r = ff::av_hwdevice_ctx_create(
                &mut d.hw,
                ff::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
                cnode.as_ptr(),
                ptr::null_mut(),
                0,
            );
            if r < 0 {
                return Err(err("VA-API を開けない", r));
            }
            (*d.codec).hw_device_ctx = ff::av_buffer_ref(d.hw);
            (*d.codec).get_format = Some(pick_vaapi);
            let r = ff::avcodec_open2(d.codec, dec, ptr::null_mut());
            if r < 0 {
                return Err(err("avcodec_open2", r));
            }
            d.pkt = ff::av_packet_alloc();
            d.width = (*d.codec).width as u32;
            d.height = (*d.codec).height as u32;
            let fr = (*st).avg_frame_rate;
            d.fps = if fr.den > 0 {
                fr.num as f64 / fr.den as f64
            } else {
                0.0
            };
            d.codec_name = CStr::from_ptr((*dec).name).to_string_lossy().into_owned();
        }
        Ok(d)
    }

    /// 次の 1 枚をデコードして返す。最後まで来たら先頭に戻る。
    pub fn next_frame(&mut self) -> Result<Frame, String> {
        unsafe {
            let src = ff::av_frame_alloc();
            if let Err(e) = receive_looping(
                self.fmt,
                self.codec,
                self.pkt,
                self.stream,
                &mut self.loops,
                &mut self.draining,
                src,
            ) {
                let mut f = src;
                ff::av_frame_free(&mut f);
                return Err(e);
            }
            if (*src).format != ff::AVPixelFormat::AV_PIX_FMT_VAAPI as i32 {
                let mut f = src;
                ff::av_frame_free(&mut f);
                return Err("VA-API のフレームではない(ソフトウェアデコードになった)".into());
            }
            let mapped = ff::av_frame_alloc();
            (*mapped).format = ff::AVPixelFormat::AV_PIX_FMT_DRM_PRIME as i32;
            let r = ff::av_hwframe_map(mapped, src, ff::AV_HWFRAME_MAP_READ as i32);
            let frame = Frame { src, mapped };
            if r < 0 {
                return Err(err("av_hwframe_map(DRM_PRIME)", r));
            }
            Ok(frame)
        }
    }
}

/// 次の 1 枚を `src` にデコードする。パケットを読んで渡し、最後まで来たら残りを吐き出させてから
/// 先頭に戻って `loops` を増やす(吐き出さないと、B フレームの最後の数枚が毎周落ちる)。
///
/// # Safety
/// 引数はどれも `Decoder` / `CpuDecoder` が持つ有効なもので、`src` は `av_frame_alloc` で確保済みであること。
unsafe fn receive_looping(
    fmt: *mut ff::AVFormatContext,
    codec: *mut ff::AVCodecContext,
    pkt: *mut ff::AVPacket,
    stream: i32,
    loops: &mut u32,
    draining: &mut bool,
    src: *mut ff::AVFrame,
) -> Result<(), String> {
    unsafe {
        // 1 周に 1 枚も出ない(壊れたファイル)と、先頭に戻って読み直すだけで終わらない。
        // この呼び出しの中で 2 回続けて何も出さずに最後まで来たら諦める
        let mut wraps = 0;
        loop {
            let r = ff::avcodec_receive_frame(codec, src);
            if r == 0 {
                return Ok(());
            }
            if r == ff::AVERROR_EOF {
                // 吐き出し終わった。先頭に戻る
                wraps += 1;
                if wraps > 1 {
                    return Err("1 枚もデコードできない".into());
                }
                ff::av_seek_frame(fmt, stream, 0, ff::AVSEEK_FLAG_BACKWARD);
                ff::avcodec_flush_buffers(codec);
                *draining = false;
                *loops += 1;
                continue;
            }
            if r != ff::AVERROR(libc_eagain()) {
                return Err(err("avcodec_receive_frame", r));
            }
            // パケットを 1 個読んで渡す。最後まで来たら残りを吐き出させる(null パケット)
            let r = ff::av_read_frame(fmt, pkt);
            if r == ff::AVERROR_EOF {
                if !*draining {
                    ff::avcodec_send_packet(codec, ptr::null());
                    *draining = true;
                }
                continue;
            }
            if r < 0 {
                return Err(err("av_read_frame", r));
            }
            if (*pkt).stream_index == stream {
                let r = ff::avcodec_send_packet(codec, pkt);
                if r < 0 && r != ff::AVERROR(libc_eagain()) {
                    ff::av_packet_unref(pkt);
                    return Err(err("avcodec_send_packet", r));
                }
            }
            ff::av_packet_unref(pkt);
        }
    }
}

const fn libc_eagain() -> i32 {
    11
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: どれも ffmpeg で確保し、ここでだけ解放する(null でもよい関数だけを使う)
        unsafe {
            ff::av_packet_free(&mut self.pkt);
            ff::avcodec_free_context(&mut self.codec);
            ff::av_buffer_unref(&mut self.hw);
            ff::avformat_close_input(&mut self.fmt);
        }
    }
}

/// RGBA(アルファは 255)の画素列。行は詰めてある(`data.len() == width * height * 4`)。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// YUV → RGB の変換の係数(8 ビット固定小数点)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Matrix {
    /// 輝度の倍率(256 = 1.0)と引くオフセット
    y_scale: i32,
    y_off: i32,
    rv: i32,
    gu: i32,
    gv: i32,
    bu: i32,
}

impl Matrix {
    /// `bt709`: BT.709(HD の動画)か BT.601 か。`full_range`: 0〜255 の全範囲か、16〜235 の放送範囲か。
    pub fn new(bt709: bool, full_range: bool) -> Self {
        let (y_scale, y_off) = if full_range { (256, 0) } else { (298, 16) };
        // 色差の倍率は放送範囲だと 224/255 ぶん大きい
        let k = |v: f64| -> i32 {
            let v = if full_range { v } else { v * 255.0 / 224.0 };
            (v * 256.0).round() as i32
        };
        if bt709 {
            Self {
                y_scale,
                y_off,
                rv: k(1.5748),
                gu: k(0.1873),
                gv: k(0.4681),
                bu: k(1.8556),
            }
        } else {
            Self {
                y_scale,
                y_off,
                rv: k(1.402),
                gu: k(0.344136),
                gv: k(0.714136),
                bu: k(1.772),
            }
        }
    }

    #[inline]
    fn rgb(&self, y: i32, u: i32, v: i32) -> [u8; 3] {
        let yy = (y - self.y_off) * self.y_scale;
        let (u, v) = (u - 128, v - 128);
        let c = |x: i32| ((x + 128) >> 8).clamp(0, 255) as u8;
        [
            c(yy + self.rv * v),
            c(yy - self.gu * u - self.gv * v),
            c(yy + self.bu * u),
        ]
    }
}

/// 出力の 1 画素(軸 1 本ぶん)が、元のどの 2 画素をどの重みで混ぜるか。重みは 0..=256(後ろの画素の割合)。
fn taps(out_n: u32, src_n: u32) -> Vec<(usize, usize, u32)> {
    let last = src_n.saturating_sub(1) as f32;
    (0..out_n)
        .map(|o| {
            let pos = ((o as f32 + 0.5) * src_n as f32 / out_n as f32 - 0.5).clamp(0.0, last);
            let i0 = pos.floor();
            let w = ((pos - i0) * 256.0).round() as u32;
            let i0 = i0 as usize;
            (i0, (i0 + 1).min(last as usize), w)
        })
        .collect()
}

/// 1 面の 4 近傍を重み(0..=256)で混ぜる。
#[inline]
fn bilinear(p: &[u8], stride: usize, x: (usize, usize, u32), y: (usize, usize, u32)) -> i32 {
    let at = |r: usize, c: usize| p[r * stride + c] as u32;
    let top = at(y.0, x.0) * (256 - x.2) + at(y.0, x.1) * x.2;
    let bot = at(y.1, x.0) * (256 - x.2) + at(y.1, x.1) * x.2;
    ((top * (256 - y.2) + bot * y.2 + 32768) >> 16) as i32
}

/// YUV420P(Y、U、V の 3 面。色差は縦横半分)を、`out` の大きさの RGBA にする。大きさが違えば双線形で拡大縮小する。
/// ffmpeg に触れない純粋な関数(試験しやすいよう分けてある)。
pub fn yuv420p_to_rgba(
    planes: [&[u8]; 3],
    strides: [usize; 3],
    src: (u32, u32),
    out: (u32, u32),
    m: Matrix,
) -> Result<Rgba, String> {
    let (sw, sh) = src;
    let (cw, ch) = (sw.div_ceil(2), sh.div_ceil(2));
    if sw == 0 || sh == 0 || out.0 == 0 || out.1 == 0 || out.0 > 8192 || out.1 > 8192 {
        return Err(format!("大きさが範囲外: {sw}x{sh} → {}x{}", out.0, out.1));
    }
    let need = |stride: usize, w: u32, h: u32| stride * (h as usize - 1) + w as usize;
    if strides[0] < sw as usize
        || strides[1] < cw as usize
        || strides[2] < cw as usize
        || planes[0].len() < need(strides[0], sw, sh)
        || planes[1].len() < need(strides[1], cw, ch)
        || planes[2].len() < need(strides[2], cw, ch)
    {
        return Err("画素の面が小さすぎる".into());
    }
    let (xl, yl) = (taps(out.0, sw), taps(out.1, sh));
    let (xc, yc) = (taps(out.0, cw), taps(out.1, ch));
    let mut data = Vec::with_capacity(out.0 as usize * out.1 as usize * 4);
    for oy in 0..out.1 as usize {
        for ox in 0..out.0 as usize {
            let y = bilinear(planes[0], strides[0], xl[ox], yl[oy]);
            let u = bilinear(planes[1], strides[1], xc[ox], yc[oy]);
            let v = bilinear(planes[2], strides[2], xc[ox], yc[oy]);
            let [r, g, b] = m.rgb(y, u, v);
            data.extend_from_slice(&[r, g, b, 255]);
        }
    }
    Ok(Rgba {
        width: out.0,
        height: out.1,
        data,
    })
}

/// 動画を CPU(ソフトウェア)でデコードして RGBA にする。最後まで読んだら先頭に戻って繰り返す。
/// VA-API は使わない。小さな動画(だいだい画面のアバター 720x720)を、CPU の描画経路に載せるためのもの。
pub struct CpuDecoder {
    fmt: *mut ff::AVFormatContext,
    codec: *mut ff::AVCodecContext,
    pkt: *mut ff::AVPacket,
    /// 直近にデコードした 1 枚(次のデコードで使い回す)
    frame: *mut ff::AVFrame,
    stream: i32,
    draining: bool,
    /// まだ 1 枚もデコードしていない
    empty: bool,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// 何周目か(0 から)
    pub loops: u32,
    pub codec_name: String,
}

// SAFETY: ffmpeg のコンテキストは持ち主(この構造体)だけが触る。スレッドをまたいで動かしてよい(同時には使わない)
unsafe impl Send for CpuDecoder {}

impl CpuDecoder {
    /// `path` を開く。映像のストリームがあり、YUV420P のデコードができなければ `Err`(音声は読まない)。
    pub fn open(path: &Path) -> Result<Self, String> {
        let cpath = CString::new(path.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
        let mut d = Self {
            fmt: ptr::null_mut(),
            codec: ptr::null_mut(),
            pkt: ptr::null_mut(),
            frame: ptr::null_mut(),
            stream: -1,
            draining: false,
            empty: true,
            width: 0,
            height: 0,
            fps: 0.0,
            loops: 0,
            codec_name: String::new(),
        };
        // 途中で失敗しても Drop が後片付けする
        unsafe {
            let r =
                ff::avformat_open_input(&mut d.fmt, cpath.as_ptr(), ptr::null(), ptr::null_mut());
            if r < 0 {
                return Err(err(&path.display().to_string(), r));
            }
            let r = ff::avformat_find_stream_info(d.fmt, ptr::null_mut());
            if r < 0 {
                return Err(err("avformat_find_stream_info", r));
            }
            let mut dec: *const ff::AVCodec = ptr::null();
            let s = ff::av_find_best_stream(
                d.fmt,
                ff::AVMediaType::AVMEDIA_TYPE_VIDEO,
                -1,
                -1,
                &mut dec,
                0,
            );
            if s < 0 {
                return Err(err("動画のストリームが無い", s));
            }
            d.stream = s;
            let st = *(*d.fmt).streams.add(s as usize);
            d.codec = ff::avcodec_alloc_context3(dec);
            if d.codec.is_null() {
                return Err("avcodec_alloc_context3".into());
            }
            let r = ff::avcodec_parameters_to_context(d.codec, (*st).codecpar);
            if r < 0 {
                return Err(err("avcodec_parameters_to_context", r));
            }
            // 1 スレッド: 描画の周期の中で順に読む。別スレッドを増やして CPU を散らさない
            (*d.codec).thread_count = 1;
            let r = ff::avcodec_open2(d.codec, dec, ptr::null_mut());
            if r < 0 {
                return Err(err("avcodec_open2", r));
            }
            d.pkt = ff::av_packet_alloc();
            d.frame = ff::av_frame_alloc();
            if d.pkt.is_null() || d.frame.is_null() {
                return Err("av_packet_alloc / av_frame_alloc".into());
            }
            d.width = (*d.codec).width as u32;
            d.height = (*d.codec).height as u32;
            let fr = (*st).avg_frame_rate;
            d.fps = if fr.den > 0 {
                fr.num as f64 / fr.den as f64
            } else {
                0.0
            };
            d.codec_name = CStr::from_ptr((*dec).name).to_string_lossy().into_owned();
        }
        Ok(d)
    }

    /// 次の 1 枚をデコードする(画素への変換はしない。飛ばす枚数ぶんはこれだけ呼ぶ)。最後まで来たら先頭に戻る。
    pub fn decode_next(&mut self) -> Result<(), String> {
        unsafe {
            ff::av_frame_unref(self.frame);
            receive_looping(
                self.fmt,
                self.codec,
                self.pkt,
                self.stream,
                &mut self.loops,
                &mut self.draining,
                self.frame,
            )?;
            let f = (*self.frame).format;
            if f != ff::AVPixelFormat::AV_PIX_FMT_YUV420P as i32
                && f != ff::AVPixelFormat::AV_PIX_FMT_YUVJ420P as i32
            {
                return Err(format!("YUV420P ではない画素形式({f})"));
            }
            self.empty = false;
        }
        Ok(())
    }

    /// 直近に `decode_next` した 1 枚を `out`(幅, 高さ)の RGBA にする。
    pub fn rgba(&self, out: (u32, u32)) -> Result<Rgba, String> {
        if self.empty {
            return Err("まだデコードしていない".into());
        }
        // SAFETY: decode_next が成功した frame は YUV420P で、data / linesize が有効
        unsafe {
            let f = &*self.frame;
            let (w, h) = (f.width as u32, f.height as u32);
            let (cw, ch) = (w.div_ceil(2) as usize, h.div_ceil(2) as usize);
            let strides = [
                f.linesize[0].max(0) as usize,
                f.linesize[1].max(0) as usize,
                f.linesize[2].max(0) as usize,
            ];
            let plane = |i: usize, rows: usize, width: usize| {
                if f.data[i].is_null() || strides[i] == 0 || rows == 0 {
                    &[][..]
                } else {
                    std::slice::from_raw_parts(f.data[i], strides[i] * (rows - 1) + width)
                }
            };
            let planes = [
                plane(0, h as usize, w as usize),
                plane(1, ch, cw),
                plane(2, ch, cw),
            ];
            let full = f.color_range == ff::AVColorRange::AVCOL_RANGE_JPEG
                || f.format == ff::AVPixelFormat::AV_PIX_FMT_YUVJ420P as i32;
            let bt709 = f.colorspace == ff::AVColorSpace::AVCOL_SPC_BT709;
            yuv420p_to_rgba(planes, strides, (w, h), out, Matrix::new(bt709, full))
        }
    }
}

impl Drop for CpuDecoder {
    fn drop(&mut self) {
        // SAFETY: どれも ffmpeg で確保し、ここでだけ解放する(null でもよい関数だけを使う)
        unsafe {
            ff::av_frame_free(&mut self.frame);
            ff::av_packet_free(&mut self.pkt);
            ff::avcodec_free_context(&mut self.codec);
            ff::avformat_close_input(&mut self.fmt);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ffmpeg のコマンドと VA-API(renderD128)がある環境だけで動く。無ければ skip する。
    #[test]
    fn decodes_to_nv12_dmabuf_layers_and_loops() {
        let dir = std::env::temp_dir().join(format!("miharashi-video-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("t.mp4");
        let made = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi"])
            .args(["-i", "testsrc2=size=320x240:rate=30", "-t", "1"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&file)
            .status()
            .is_ok_and(|s| s.success());
        if !made {
            eprintln!("ffmpeg が無いので skip");
            return;
        }
        let mut d = match Decoder::open(&file, Path::new("/dev/dri/renderD128")) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("VA-API が使えないので skip: {e}");
                return;
            }
        };
        assert_eq!((d.width, d.height), (320, 240));
        assert!((d.fps - 30.0).abs() < 0.01, "{}", d.fps);
        let f = d.next_frame().unwrap();
        let layers = f.layers();
        eprintln!("{layers:?}");
        assert_eq!(layers.len(), 2, "Y と UV の 2 枚");
        assert_eq!(layers[0].fourcc, DRM_R8);
        assert_eq!(layers[1].fourcc, DRM_GR88);
        assert_eq!((layers[1].width, layers[1].height), (160, 120));
        drop(f);
        // 1 秒(30 枚)を超えて読むと先頭に戻る
        for _ in 0..40 {
            d.next_frame().unwrap();
        }
        assert_eq!(d.loops, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- CpuDecoder と色の変換(ffmpeg のコマンドが無い環境では、コマンドを使う試験だけ skip する) ----

    /// ffmpeg のコマンドで試験用の動画を作る。作れなければ None(skip)。
    fn make_clip(
        name: &str,
        secs: u32,
        extra: &[&str],
    ) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
        let dir = std::env::temp_dir().join(format!("miharashi-cpu-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok()?;
        let file = dir.join("t.mp4");
        let ok = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi"])
            .args(["-i", "testsrc2=size=96x64:rate=10"])
            .args(["-t", &secs.to_string()])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .args(extra)
            .arg(&file)
            .status()
            .is_ok_and(|s| s.success());
        ok.then_some((dir, file))
    }

    /// 全面が 1 色の YUV420P を作る
    fn flat(w: usize, h: usize, y: u8, u: u8, v: u8) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        (
            vec![y; w * h],
            vec![u; w.div_ceil(2) * h.div_ceil(2)],
            vec![v; w.div_ceil(2) * h.div_ceil(2)],
        )
    }

    #[test]
    fn flat_colours_convert_to_the_expected_rgb() {
        // 放送範囲(16〜235)の黒・白・無彩色、BT.601
        let m = Matrix::new(false, false);
        for (y, want) in [(16, 0u8), (235, 255), (126, 128)] {
            let (py, pu, pv) = flat(8, 8, y, 128, 128);
            let out = yuv420p_to_rgba([&py, &pu, &pv], [8, 4, 4], (8, 8), (8, 8), m).unwrap();
            for px in out.data.chunks(4) {
                for c in &px[..3] {
                    assert!((*c as i32 - want as i32).abs() <= 1, "y={y}: {px:?}");
                }
                assert_eq!(px[3], 255);
            }
        }
        // 赤: BT.601 の (Y, U, V) = (81, 90, 240) ≒ (255, 0, 0)
        let (py, pu, pv) = flat(4, 4, 81, 90, 240);
        let out = yuv420p_to_rgba([&py, &pu, &pv], [4, 2, 2], (4, 4), (4, 4), m).unwrap();
        let px = &out.data[..4];
        assert!(px[0] > 245 && px[1] < 10 && px[2] < 10, "{px:?}");
        // BT.709 では同じ値が違う色になる(行列を取り違えると気づける)
        let out709 = yuv420p_to_rgba(
            [&py, &pu, &pv],
            [4, 2, 2],
            (4, 4),
            (4, 4),
            Matrix::new(true, false),
        )
        .unwrap();
        assert_ne!(&out709.data[..3], &out.data[..3]);
        // 全範囲では 0 と 255 がそのまま黒と白
        let full = Matrix::new(false, true);
        for (y, want) in [(0, 0u8), (255, 255)] {
            let (py, pu, pv) = flat(2, 2, y, 128, 128);
            let out = yuv420p_to_rgba([&py, &pu, &pv], [2, 1, 1], (2, 2), (2, 2), full).unwrap();
            assert_eq!(out.data[0], want);
        }
    }

    #[test]
    fn scaling_keeps_a_flat_colour_and_the_requested_size() {
        let (py, pu, pv) = flat(10, 6, 126, 128, 128);
        let m = Matrix::new(false, false);
        for out in [(5, 3), (10, 6), (20, 12), (7, 7), (1, 1)] {
            let r = yuv420p_to_rgba([&py, &pu, &pv], [10, 5, 5], (10, 6), out, m).unwrap();
            assert_eq!((r.width, r.height), out);
            assert_eq!(r.data.len(), (out.0 * out.1 * 4) as usize);
            assert!(r.data.chunks(4).all(|p| (p[0] as i32 - 128).abs() <= 1));
        }
    }

    #[test]
    fn scaling_a_gradient_stays_between_its_ends_and_keeps_the_order() {
        // 左が暗く右が明るい輝度の面は、縮小しても左から右へ単調に明るくなる
        let w = 12;
        let py: Vec<u8> = (0..w * 4).map(|i| 16 + (i % w) as u8 * 18).collect();
        let (_, pu, pv) = flat(w, 4, 0, 128, 128);
        let r = yuv420p_to_rgba(
            [&py, &pu, &pv],
            [w, w / 2, w / 2],
            (w as u32, 4),
            (5, 2),
            Matrix::new(false, true),
        )
        .unwrap();
        let row: Vec<u8> = r.data.chunks(4).take(5).map(|p| p[0]).collect();
        assert!(row.windows(2).all(|p| p[0] < p[1]), "{row:?}");
    }

    #[test]
    fn short_planes_and_bad_sizes_are_errors_not_panics() {
        let (py, pu, pv) = flat(8, 8, 100, 128, 128);
        let m = Matrix::new(false, false);
        assert!(yuv420p_to_rgba([&py[..10], &pu, &pv], [8, 4, 4], (8, 8), (8, 8), m).is_err());
        assert!(yuv420p_to_rgba([&py, &pu, &pv], [4, 4, 4], (8, 8), (8, 8), m).is_err());
        assert!(yuv420p_to_rgba([&py, &pu, &pv], [8, 4, 4], (0, 8), (8, 8), m).is_err());
        assert!(yuv420p_to_rgba([&py, &pu, &pv], [8, 4, 4], (8, 8), (0, 8), m).is_err());
        assert!(yuv420p_to_rgba([&py, &pu, &pv], [8, 4, 4], (8, 8), (9000, 8), m).is_err());
    }

    #[test]
    fn cpu_decoder_returns_every_frame_once_per_loop_with_b_frames() {
        // B フレームがある動画でも、1 周ちょうど 20 枚(10fps x 2 秒)。最後の数枚が落ちない
        let Some((dir, file)) = make_clip("loop", 2, &["-bf", "2"]) else {
            eprintln!("ffmpeg が無いので skip");
            return;
        };
        let mut d = CpuDecoder::open(&file).unwrap();
        assert_eq!((d.width, d.height), (96, 64));
        assert!((d.fps - 10.0).abs() < 0.01, "{}", d.fps);
        assert_eq!(d.codec_name, "h264");
        let first = {
            d.decode_next().unwrap();
            d.rgba((96, 64)).unwrap()
        };
        assert_eq!((first.width, first.height), (96, 64));
        for _ in 1..20 {
            d.decode_next().unwrap();
            assert_eq!(d.loops, 0);
        }
        // 21 枚目は先頭に戻っていて、1 枚目と同じ絵になる(周回が 1 つ増える)
        d.decode_next().unwrap();
        assert_eq!(d.loops, 1);
        assert_eq!(d.rgba((96, 64)).unwrap(), first, "先頭に戻る");
        // 何周しても落ちない
        for _ in 0..60 {
            d.decode_next().unwrap();
        }
        assert_eq!(d.loops, 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cpu_decoder_converts_and_scales_the_decoded_frame() {
        let Some((dir, file)) = make_clip("scale", 1, &[]) else {
            eprintln!("ffmpeg が無いので skip");
            return;
        };
        let mut d = CpuDecoder::open(&file).unwrap();
        assert!(d.rgba((48, 32)).is_err(), "デコード前は変換できない");
        d.decode_next().unwrap();
        let full = d.rgba((96, 64)).unwrap();
        let half = d.rgba((48, 32)).unwrap();
        assert_eq!(half.data.len(), 48 * 32 * 4);
        assert!(full.data.iter().any(|&b| b != 0 && b != 255), "絵がある");
        // 縮小しても全体の明るさはほぼ同じ
        let mean = |r: &Rgba| {
            r.data.chunks(4).map(|p| p[1] as u64).sum::<u64>() as f64 / (r.width * r.height) as f64
        };
        assert!(
            (mean(&full) - mean(&half)).abs() < 6.0,
            "{} {}",
            mean(&full),
            mean(&half)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cpu_decoder_refuses_broken_and_missing_files() {
        assert!(CpuDecoder::open(Path::new("/nonexistent/miharashi.mp4")).is_err());
        let dir = std::env::temp_dir().join(format!("miharashi-cpu-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bad = dir.join("bad.mp4");
        std::fs::write(&bad, b"not an mp4 at all").unwrap();
        assert!(CpuDecoder::open(&bad).is_err());
        std::fs::write(&bad, b"").unwrap();
        assert!(CpuDecoder::open(&bad).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 同梱の素材 4 本(720x720 / 24fps / 8 秒 = 192 枚)が、この経路で読めて、1 周が 192 枚ちょうどになる。
    #[test]
    fn the_bundled_daidai_clips_decode_and_loop_at_192_frames() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../miharashi-render/assets/daidai");
        for pose in ["idle", "working", "milestone", "making"] {
            let mut d = CpuDecoder::open(&dir.join(format!("daidai-{pose}.mp4")))
                .unwrap_or_else(|e| panic!("{pose}: {e}"));
            assert_eq!((d.width, d.height), (720, 720), "{pose}");
            assert!((d.fps - 24.0).abs() < 0.01, "{pose}");
            d.decode_next().unwrap();
            let first = d.rgba((480, 480)).unwrap();
            for _ in 1..192 {
                d.decode_next().unwrap();
            }
            assert_eq!(d.loops, 0, "{pose}: 192 枚目までは 1 周目");
            d.decode_next().unwrap();
            assert_eq!(d.loops, 1, "{pose}");
            assert_eq!(d.rgba((480, 480)).unwrap(), first, "{pose}: 先頭に戻る");
        }
    }
}
