//! `vrm-view` の背景になる写真のスライドショー。写真画面(`miharashi-app` の `PhotoScreen`)と同じ道で、
//! デコードしただけの写真を `Frame::Photo`(`PhotoSource::Image` → 2 回目からは `Resident`)で
//! `miharashi-gpu` の `Compositor` に渡し、GPU で完成フレームを作ってフェードで切り替える。
//! 置き方は `miharashi_render::photo_layout::plan`(左 → 右 → 中央の巡回)。重ね絵(時刻などの文字)は出さない。
//! `miharashi-app` には依存しない(写真の読み込みと合成写真は `bench.rs` の考え方を写した)。
use image::RgbImage;
use miharashi_gpu::{Frame, PhotoLayer, PhotoSource, Placement, SourceImage};
use miharashi_render::photo_layout::{self, Mode, PhotoLayout};
use std::path::Path;
use std::time::{Duration, Instant};

/// 写真画面のフェードの長さ(`PhotoScreen::new` の `fade_len`)
pub const FADE_LEN: Duration = Duration::from_millis(800);

/// 読み込む写真の枚数の上限(全部をデコードしたまま持つので、12MP で 1 枚約 36MB)
pub const MAX_PHOTOS: usize = 6;

/// 合成写真(スマートフォンの写真に近い 12MP、細かい模様入り)。`variant` で色と縦横比を変える
/// (4:3、3:2、縦長の 3:4 を巡る)。
pub fn synthetic_photo(variant: usize) -> RgbImage {
    let (w, h) = match variant % 4 {
        0 => (4032u32, 3024u32),
        1 => (4032, 2688),
        2 => (3024, 4032),
        _ => (4032, 3024),
    };
    let mut seed = 0x9e37_79b9_u32.wrapping_add(variant as u32 * 0x0123_4567);
    let shift = variant as u32 * 61;
    RgbImage::from_fn(w, h, |x, y| {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        let n = seed & 0x3f;
        image::Rgb([
            ((x * 255 / w + n + shift) % 256) as u8,
            ((y * 255 / h + n + shift * 2) % 256) as u8,
            (((x + y) * 128 / (w + h) + n * 2 + shift * 3) % 256) as u8,
        ])
    })
}

/// `dir` の JPEG(拡張子 jpg / jpeg、名前順、最大 `MAX_PHOTOS` 枚)を読む。1 枚もなければ合成写真を 4 枚作る。
/// 戻りの 2 つ目は、実際に読んだ JPEG の数(0 なら合成)。
pub fn load(dir: Option<&Path>) -> Result<(Vec<RgbImage>, usize), String> {
    let mut paths = Vec::new();
    if let Some(dir) = dir {
        for e in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let p = e.map_err(|e| e.to_string())?.path();
            let ext = p
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase);
            if matches!(ext.as_deref(), Some("jpg" | "jpeg")) {
                paths.push(p);
            }
        }
        paths.sort();
        paths.truncate(MAX_PHOTOS);
    }
    if paths.is_empty() {
        return Ok(((0..4).map(synthetic_photo).collect(), 0));
    }
    let n = paths.len();
    let imgs = paths
        .iter()
        .map(|p| {
            image::open(p)
                .map(|i| i.to_rgb8())
                .map_err(|e| format!("{}: {e}", p.display()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((imgs, n))
}

struct Item {
    id: u64,
    photo: usize,
    layout: PhotoLayout,
    /// GPU に渡したか(渡したあとは `Resident`)
    sent: bool,
}

pub struct Slideshow {
    photos: Vec<RgbImage>,
    screen: (u32, u32),
    interval: Duration,
    /// 巡回の次の候補(`photo_layout::plan` の `slot`)
    slot: usize,
    next_photo: usize,
    next_id: u64,
    current: Item,
    /// フェード中の (旧フレーム, 開始時刻)
    fade: Option<(Item, Instant)>,
    last_change: Instant,
}

impl Slideshow {
    pub fn new(
        photos: Vec<RgbImage>,
        screen: (u32, u32),
        interval: Duration,
        now: Instant,
    ) -> Self {
        assert!(!photos.is_empty());
        let mut s = Self {
            photos,
            screen,
            interval,
            slot: 0,
            next_photo: 0,
            next_id: 1,
            current: Item {
                id: 0,
                photo: 0,
                layout: photo_layout::plan(&mut 0, screen.0, screen.1, 1, 1),
                sent: false,
            },
            fade: None,
            last_change: now,
        };
        s.current = s.make_item();
        s
    }

    fn make_item(&mut self) -> Item {
        let photo = self.next_photo % self.photos.len();
        self.next_photo += 1;
        let img = &self.photos[photo];
        let layout = photo_layout::plan(
            &mut self.slot,
            self.screen.0,
            self.screen.1,
            img.width(),
            img.height(),
        );
        let id = self.next_id;
        self.next_id += 1;
        Item {
            id,
            photo,
            layout,
            sent: false,
        }
    }

    /// 時刻を進める。間隔が過ぎていれば次の写真に切り替えてフェードを始める(true を返す)。
    pub fn update(&mut self, now: Instant) -> bool {
        if self
            .fade
            .as_ref()
            .is_some_and(|(_, t0)| now.duration_since(*t0) >= FADE_LEN)
        {
            self.fade = None;
        }
        if now.duration_since(self.last_change) < self.interval {
            return false;
        }
        let next = self.make_item();
        let old = std::mem::replace(&mut self.current, next);
        self.fade = Some((old, now));
        self.last_change = now;
        true
    }

    /// フェードの進み(新フレームの不透明度 0〜1)。フェード中でなければ None。
    pub fn fade_alpha(&self, now: Instant) -> Option<f32> {
        self.fade.as_ref().map(|(_, t0)| {
            (now.duration_since(*t0).as_secs_f32() / FADE_LEN.as_secs_f32()).min(1.0)
        })
    }

    pub fn current_layout(&self) -> &PhotoLayout {
        &self.current.layout
    }

    pub fn current_id(&self) -> u64 {
        self.current.id
    }

    fn layer<'a>(&'a self, it: &Item) -> PhotoLayer<'a> {
        let src = if it.sent {
            PhotoSource::Resident
        } else {
            let img = &self.photos[it.photo];
            PhotoSource::Image(SourceImage {
                pixels: img.as_raw(),
                size: (img.width(), img.height()),
                channels: 3,
                place: Placement {
                    cover: it.layout.mode == Mode::Cover,
                    rect: (
                        it.layout.photo.x,
                        it.layout.photo.y,
                        it.layout.photo.w,
                        it.layout.photo.h,
                    ),
                    scale: photo_layout::scale(self.screen.1),
                },
            })
        };
        PhotoLayer { id: it.id, src }
    }

    /// いま描く `Frame::Photo`(重ね絵なし)。描いたあとに `mark_sent` を呼ぶこと。
    pub fn frame(&self, now: Instant) -> Frame<'_> {
        let old = self
            .fade
            .as_ref()
            .map(|(it, _)| (self.layer(it), self.fade_alpha(now).unwrap_or(1.0)));
        Frame::Photo {
            photo: self.layer(&self.current),
            old,
            overlay: None,
        }
    }

    /// 描いた写真は GPU が持っているので、次からは `Resident` で渡す。
    pub fn mark_sent(&mut self) {
        self.current.sent = true;
        if let Some((it, _)) = &mut self.fade {
            it.sent = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny(w: u32, h: u32) -> RgbImage {
        RgbImage::from_pixel(w, h, image::Rgb([10, 20, 30]))
    }

    #[test]
    fn switches_every_interval_with_fade_of_800ms() {
        let t0 = Instant::now();
        let mut s = Slideshow::new(
            vec![tiny(40, 30), tiny(30, 40)],
            (2560, 1080),
            Duration::from_secs(10),
            t0,
        );
        assert_eq!(s.current_id(), 1);
        assert!(!s.update(t0 + Duration::from_secs(9)));
        assert!(s.fade_alpha(t0).is_none());
        let t1 = t0 + Duration::from_secs(10);
        assert!(s.update(t1));
        assert_eq!(s.current_id(), 2);
        let half = s.fade_alpha(t1 + Duration::from_millis(400)).unwrap();
        assert!((half - 0.5).abs() < 1e-3);
        // フェードが終わると旧フレームを手放す
        s.update(t1 + Duration::from_millis(800));
        assert!(s.fade_alpha(t1 + Duration::from_millis(800)).is_none());
        assert!(matches!(
            s.frame(t1 + Duration::from_secs(1)),
            Frame::Photo { old: None, .. }
        ));
    }

    #[test]
    fn photo_goes_resident_after_first_frame_and_layout_cycles() {
        let t0 = Instant::now();
        let mut s = Slideshow::new(vec![tiny(40, 30)], (2560, 1080), Duration::from_secs(1), t0);
        let src = |s: &Slideshow| match s.frame(t0) {
            Frame::Photo { photo, .. } => matches!(photo.src, PhotoSource::Image(_)),
            _ => unreachable!(),
        };
        assert!(src(&s), "初めての写真はデコード済みの画素ごと渡す");
        s.mark_sent();
        assert!(!src(&s), "2 回目からは Resident");
        // 4:3 の写真は 2560x1080 で 左 → 右 → 中央 と巡る
        let mut modes = vec![s.current_layout().mode];
        for k in 1..3 {
            s.update(t0 + Duration::from_secs(k));
            modes.push(s.current_layout().mode);
        }
        assert_eq!(modes, [Mode::Left, Mode::Right, Mode::Center]);
    }

    #[test]
    fn synthetic_photos_vary_in_aspect() {
        let a = synthetic_photo(0);
        let b = synthetic_photo(2);
        assert!(a.width() > a.height() && b.height() > b.width());
    }

    /// 写真画面と同じ道(Compositor の Frame::Photo)で写真を描いた上に、VRM の層を重ねる。
    /// VRM の外は写真のまま、VRM のある所は変わること(vrm-view の描き方を表示なしで確かめる)。
    #[test]
    fn vrm_layer_overlays_photo_frame_offscreen() {
        use crate::{Framing, Model, Renderer, StageRegion, VrmLayer, synth};
        use miharashi_gpu::Offscreen;
        use miharashi_gpu::compose::Compositor;
        let Ok(off) = Offscreen::open(Path::new("/dev/dri/renderD128")) else {
            eprintln!("GLES が使えないので skip");
            return;
        };
        let size = (640u32, 270u32);
        let t0 = Instant::now();
        let s = Slideshow::new(
            vec![RgbImage::from_pixel(160, 120, image::Rgb([200, 120, 40]))],
            size,
            Duration::from_secs(10),
            t0,
        );
        let (rx, ry, rw, rh) = StageRegion::Left.rect(size);
        let (_, img) = off
            .with_fbo(size, |gl| {
                let target = unsafe {
                    glow::HasContext::get_parameter_framebuffer(gl, glow::FRAMEBUFFER_BINDING)
                };
                let mut comp = Compositor::new(gl, size).unwrap();
                let model =
                    Model::from_glb(&synth::build_vrm_glb(&synth::Opts::default())).unwrap();
                let layer = VrmLayer::new(gl, (rw, rh)).unwrap();
                unsafe { glow::HasContext::bind_framebuffer(gl, glow::FRAMEBUFFER, target) };
                let mut r = Renderer::new(gl, model).unwrap();
                r.clear_color = [0.0; 4];
                r.animate(0.0, 1.0 / 30.0);
                miharashi_gpu::draw_frame_with(
                    &mut comp,
                    gl,
                    size,
                    target,
                    &s.frame(t0),
                    |gl, _| {
                        r.update(gl);
                        let cam = r.framing_camera(Framing::Full, (rw, rh), 0.0);
                        layer.bind(gl);
                        r.render_camera(gl, (rw, rh), &cam.0, cam.1);
                        layer.composite(gl, target, (rx as i32, ry as i32));
                    },
                )
                .unwrap();
                r.destroy(gl);
                layer.destroy(gl);
                comp.destroy(gl);
            })
            .unwrap();
        if let Ok(p) = std::env::var("VRM_STAGE_PNG") {
            image::RgbaImage::from_raw(size.0, size.1, img.clone())
                .unwrap()
                .save(p)
                .unwrap();
        }
        let px = |x: u32, y: u32| &img[((y * size.0 + x) * 4) as usize..][..4];
        // 領域の外(右端)は写真の描画結果のまま(写真かぼかし背景。いずれも透明ではなく不透明)
        assert_eq!(px(size.0 - 1, 135)[3], 255);
        // 領域の中に、写真と違う画素(モデル)がある。領域外に漏れていない
        let changed = |x0: u32, x1: u32| {
            (0..size.1)
                .flat_map(|y| (x0..x1).map(move |x| (x, y)))
                .filter(|&(x, y)| px(x, y)[..3] != px(size.0 - 1, 135)[..3] && px(x, y)[3] == 255)
                .count()
        };
        assert!(changed(0, rw) > 500, "領域内にモデルが描かれる");
    }
}
