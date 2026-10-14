//! Linux 専用: ffmpeg(libavformat / libavcodec)で動画を読み、VA-API(表示カードの内蔵 GPU)でデコードして、
//! 1 枚ずつ dmabuf(DRM PRIME)の形で渡す(PoC-005 PR4 B2)。画素は GPU のメモリに置いたまま CPU に読み出さない。
//! ソフトウェアデコードには戻らない(VA-API が使えなければ開けない)。
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
            loop {
                let r = ff::avcodec_receive_frame(self.codec, src);
                if r == 0 {
                    break;
                }
                if r != ff::AVERROR(libc_eagain()) {
                    let mut f = src;
                    ff::av_frame_free(&mut f);
                    return Err(err("avcodec_receive_frame", r));
                }
                // パケットを 1 個読んで渡す。最後まで来たら先頭に戻る
                let r = ff::av_read_frame(self.fmt, self.pkt);
                if r == ff::AVERROR_EOF {
                    ff::av_seek_frame(self.fmt, self.stream, 0, ff::AVSEEK_FLAG_BACKWARD);
                    ff::avcodec_flush_buffers(self.codec);
                    self.loops += 1;
                    continue;
                }
                if r < 0 {
                    let mut f = src;
                    ff::av_frame_free(&mut f);
                    return Err(err("av_read_frame", r));
                }
                if (*self.pkt).stream_index == self.stream {
                    let r = ff::avcodec_send_packet(self.codec, self.pkt);
                    if r < 0 && r != ff::AVERROR(libc_eagain()) {
                        ff::av_packet_unref(self.pkt);
                        let mut f = src;
                        ff::av_frame_free(&mut f);
                        return Err(err("avcodec_send_packet", r));
                    }
                }
                ff::av_packet_unref(self.pkt);
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
}
