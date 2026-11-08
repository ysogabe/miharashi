//! だいだい画面のポーズ動画(Issue #99)。アバターの PNG の代わりに、ポーズごとの MP4 をループ再生する。
//!
//! 動画は `[daidai] avatar_dir` の `daidai-<pose>.mp4` だけから読む(組み込みの既定は PNG のまま)。
//! ファイルが無い・大きすぎる・壊れている・デコードできない・このビルドに `video` が無いときは、そのポーズは PNG のまま
//! (画面は落とさない。理由はそのポーズにつき 1 回だけ journal に残す)。
//!
//! 画面の描画の流れ(CPU の `Canvas` の上に `draw_avatar` で重ねる)は PNG のときと同じで、動画の 1 枚が
//! `Pixmap` になって `draw_avatar` に渡るだけなので、四隅の角丸の切り抜きなど見た目は変わらない。
//!
//! 再生の進み方は壁時計で決める(「描いた回数」ではなく、始めてからの経過時間 x フレームレート)。描き直しの上限を
//! 動画のフレームレートより小さくしても、再生が遅くならず、間のフレームは読み飛ばす(読み飛ばしでは色の変換をしない)。
//! 画面が見えていない・モニターが消えているときは、デコーダーごと捨てて何も読まない(戻ったら先頭から)。
use miharashi_render::Pixmap;
use miharashi_render::daidai::Pose;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// 動画ファイルの大きさの上限(開く前にファイルの大きさで確かめる)。素材は 1 本 0.3〜0.6MB 程度。
pub const MAX_VIDEO_BYTES: u64 = 32 * 1024 * 1024;
/// 動画のフレームレートの範囲(これを外れた動画は使わない)。
const FPS_RANGE: std::ops::RangeInclusive<f64> = 1.0..=120.0;
/// 描き直しの間に読み進めるフレームの上限。これを超えて遅れたら、遅れを取り戻さず時計を合わせ直す
/// (一時停止や重い処理のあとに、まとめて読み進めて固まらない)。
const MAX_CATCH_UP: u64 = 12;

/// ポーズに対応する動画のファイル名(`daidai-working.mp4` など。PNG と同じ名前の拡張子違い)。
pub fn video_file_name(pose: Pose) -> String {
    format!("daidai-{}.mp4", pose.name())
}

/// 動画 1 本から、先頭から順に(最後まで来たら先頭に戻って)フレームを読む口。
/// 実体は `video` つきのビルドの ffmpeg。試験は偽物で差し替える。
pub trait FrameSource {
    /// 次の 1 枚を読む(画素にはしない。読み飛ばす枚数ぶんはこれだけ呼ぶ)。
    fn decode_next(&mut self) -> Result<(), String>;
    /// 直近に `decode_next` した 1 枚を画素にする。
    fn frame(&mut self) -> Result<Pixmap, String>;
    /// 動画のフレームレート。
    fn fps(&self) -> f64;
}

/// ポーズの動画を開く。`Err` はそのポーズを PNG にする理由。
pub type Opener = Box<dyn Fn(Pose) -> Result<Box<dyn FrameSource>, String>>;

/// `dir` の `daidai-<pose>.mp4` を開く `Opener`。`dir` が空なら None(動画は使わない)。
pub fn dir_opener(dir: &str) -> Option<Opener> {
    if dir.is_empty() {
        return None;
    }
    let dir = PathBuf::from(dir);
    Some(Box::new(move |pose| {
        let path = dir.join(video_file_name(pose));
        let len = std::fs::metadata(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .len();
        if len > MAX_VIDEO_BYTES {
            return Err(format!(
                "{}: {MAX_VIDEO_BYTES} バイトを超えている",
                path.display()
            ));
        }
        open_file(&path)
    }))
}

#[cfg(all(feature = "video", target_os = "linux"))]
fn open_file(path: &std::path::Path) -> Result<Box<dyn FrameSource>, String> {
    let dec = miharashi_video::CpuDecoder::open(path)?;
    if dec.width == 0 || dec.height == 0 || dec.width > 8192 || dec.height > 8192 {
        return Err(format!(
            "動画の大きさが範囲外: {}x{}",
            dec.width, dec.height
        ));
    }
    // 縦横の比を保ち、アバターの描画の大きさ(AVATAR_SIZE)より大きいときだけ縮める
    let s =
        (miharashi_render::daidai::AVATAR_SIZE as f32 / dec.width.max(dec.height) as f32).min(1.0);
    let out = (
        ((dec.width as f32 * s).round() as u32).max(1),
        ((dec.height as f32 * s).round() as u32).max(1),
    );
    Ok(Box::new(Ffmpeg { dec, out }))
}

#[cfg(not(all(feature = "video", target_os = "linux")))]
fn open_file(path: &std::path::Path) -> Result<Box<dyn FrameSource>, String> {
    Err(format!(
        "{}: このビルドには動画の機能が無い(cargo build --features video が要る)",
        path.display()
    ))
}

#[cfg(all(feature = "video", target_os = "linux"))]
struct Ffmpeg {
    dec: miharashi_video::CpuDecoder,
    out: (u32, u32),
}

#[cfg(all(feature = "video", target_os = "linux"))]
impl FrameSource for Ffmpeg {
    fn decode_next(&mut self) -> Result<(), String> {
        self.dec.decode_next()
    }

    fn frame(&mut self) -> Result<Pixmap, String> {
        let rgba = self.dec.rgba(self.out)?;
        let mut px = Pixmap::new(rgba.width, rgba.height).ok_or("画素の大きさが 0")?;
        // アルファは 255 なので、premultiplied でも同じ値
        px.data_mut().copy_from_slice(&rgba.data);
        Ok(px)
    }

    fn fps(&self) -> f64 {
        self.dec.fps
    }
}

/// 経過時間から、そのとき出すべきフレームの通し番号(0 から。周回はデコーダーが勝手に戻るので、通し番号は増え続ける)。
pub fn frame_number(elapsed: Duration, fps: f64) -> u64 {
    (elapsed.as_secs_f64() * fps).floor().max(0.0) as u64
}

/// ポーズ 1 本ぶんの再生。
pub struct Player {
    pose: Pose,
    source: Box<dyn FrameSource>,
    fps: f64,
    /// 通し番号 0 のフレームを出す時刻(遅れたら合わせ直す)
    started: Instant,
    /// 読み終えたフレームの数(次に読むのが、この番号)
    decoded: u64,
    frame: Option<Pixmap>,
}

impl Player {
    fn new(pose: Pose, source: Box<dyn FrameSource>, now: Instant) -> Result<Self, String> {
        let fps = source.fps();
        if !FPS_RANGE.contains(&fps) {
            return Err(format!("動画のフレームレートが範囲外: {fps}"));
        }
        let mut p = Self {
            pose,
            source,
            fps,
            started: now,
            decoded: 0,
            frame: None,
        };
        // 最初の 1 枚まで読めて初めて「再生できる」(読めない動画は、ここで PNG に戻す)
        p.advance(now)?;
        if p.frame.is_none() {
            return Err("最初のフレームを読めない".into());
        }
        Ok(p)
    }

    /// `now` に出すべきフレームまで読み進める。新しいフレームを出せたら `true`。
    /// 描き直しの間に複数枚ぶん進むときは、途中のフレームは読むだけで画素にしない。
    fn advance(&mut self, now: Instant) -> Result<bool, String> {
        let target = frame_number(now.saturating_duration_since(self.started), self.fps);
        if target < self.decoded {
            return Ok(false);
        }
        let mut need = target + 1 - self.decoded;
        if need > MAX_CATCH_UP {
            // 大きく遅れた。取り戻さず、次の 1 枚を「今」の枚として時計を合わせる(枚の真ん中に置いて、
            // 浮動小数点の誤差で 1 枚ずれないようにする)
            let behind = Duration::from_secs_f64((self.decoded as f64 + 0.5) / self.fps);
            self.started = now.checked_sub(behind).unwrap_or(now);
            need = 1;
        }
        for _ in 0..need {
            self.source.decode_next()?;
        }
        self.decoded += need;
        self.frame = Some(self.source.frame()?);
        Ok(true)
    }
}

/// だいだい画面が持つ動画の再生全体(開く・切り替える・止める)。
pub struct VideoPlayback {
    opener: Option<Opener>,
    /// 描き直しの間隔の下限
    interval: Duration,
    player: Option<Player>,
    /// 開けなかったポーズ(起動のあいだ開き直さない。理由は 1 回だけ残す)
    failed: Vec<Pose>,
    /// 再生を始めたことを残したポーズ
    announced: Vec<Pose>,
}

impl VideoPlayback {
    /// `opener` が None なら動画は使わない(PNG だけ)。`max_fps`: 描き直しの回数の上限(フレーム / 秒)。
    pub fn new(opener: Option<Opener>, max_fps: u32) -> Self {
        Self {
            opener,
            interval: Duration::from_secs_f64(1.0 / f64::from(max_fps.clamp(1, 120))),
            player: None,
            failed: Vec::new(),
            announced: Vec::new(),
        }
    }

    /// 動画を使わない(`png` の fixture や素材の置き場が無いとき)。
    pub fn none() -> Self {
        Self::new(None, 12)
    }

    /// 再生中か(= `pose` のフレームがある)。
    pub fn playing(&self) -> bool {
        self.player.is_some()
    }

    /// 描き直しの間隔の下限(再生中だけ意味がある)。
    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// いま出すフレーム。`pose` の動画が再生中でなければ None(PNG を使う)。
    pub fn frame(&self, pose: Pose) -> Option<&Pixmap> {
        self.player
            .as_ref()
            .filter(|p| p.pose == pose)
            .and_then(|p| p.frame.as_ref())
    }

    /// 止める。デコーダーを捨てるので、何も読まず、メモリも持たない。
    pub fn stop(&mut self) {
        self.player = None;
    }

    /// 表示中に毎周期呼ぶ。`pose` が変わっていれば開き直し、時計に合わせて読み進める。
    /// 途中で読めなくなったら、そのポーズは PNG に戻す(画面は落とさない)。
    pub fn update(&mut self, pose: Pose, now: Instant) {
        if self.player.as_ref().is_some_and(|p| p.pose != pose) {
            self.player = None;
        }
        if self.player.is_none() {
            let Some(opener) = &self.opener else { return };
            if self.failed.contains(&pose) {
                return;
            }
            match opener(pose).and_then(|src| Player::new(pose, src, now)) {
                Ok(p) => {
                    if !self.announced.contains(&pose) {
                        self.announced.push(pose);
                        eprintln!(
                            "daidai: ポーズ動画 {} を再生(最大 {:.0} フレーム/秒)",
                            pose.name(),
                            1.0 / self.interval.as_secs_f64()
                        );
                    }
                    self.player = Some(p);
                }
                Err(e) => {
                    eprintln!(
                        "daidai: ポーズ動画 {} を使えない(PNG を使う): {e}",
                        pose.name()
                    );
                    self.failed.push(pose);
                }
            }
            return;
        }
        let Some(p) = self.player.as_mut() else {
            return;
        };
        if let Err(e) = p.advance(now) {
            eprintln!(
                "daidai: ポーズ動画 {} を読めなくなった(PNG に戻す): {e}",
                pose.name()
            );
            self.failed.push(pose);
            self.player = None;
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// 試験用の動画。`n` 枚で一周し、i 枚目の絵は赤の値が i(1x1)。読んだ枚数と画素にした回数を数える。
    pub struct Fake {
        pub n: u32,
        pub fps: f64,
        next: u32,
        pub stats: Rc<RefCell<Stats>>,
    }

    #[derive(Default, Debug)]
    pub struct Stats {
        pub decoded: u32,
        pub converted: u32,
        pub loops: u32,
    }

    impl Fake {
        pub fn new(n: u32, fps: f64, stats: Rc<RefCell<Stats>>) -> Self {
            Self {
                n,
                fps,
                next: 0,
                stats,
            }
        }
    }

    impl FrameSource for Fake {
        fn decode_next(&mut self) -> Result<(), String> {
            let mut s = self.stats.borrow_mut();
            s.decoded += 1;
            self.next += 1;
            if self.next > self.n {
                self.next = 1;
                s.loops += 1;
            }
            Ok(())
        }
        fn frame(&mut self) -> Result<Pixmap, String> {
            self.stats.borrow_mut().converted += 1;
            let mut p = Pixmap::new(1, 1).unwrap();
            // 直近に読んだ枚の番号(0 から)を赤に入れる
            p.data_mut()
                .copy_from_slice(&[(self.next - 1) as u8, 0, 0, 255]);
            Ok(p)
        }
        fn fps(&self) -> f64 {
            self.fps
        }
    }

    /// 偽物の動画を、開いた回数つきで返す `Opener`。`missing` のポーズは開けない。
    pub fn fake_opener(
        n: u32,
        fps: f64,
        missing: &'static [Pose],
        stats: Rc<RefCell<Stats>>,
        opens: Rc<RefCell<Vec<Pose>>>,
    ) -> Opener {
        Box::new(move |pose| {
            opens.borrow_mut().push(pose);
            if missing.contains(&pose) {
                return Err("無い".into());
            }
            Ok(Box::new(Fake::new(n, fps, stats.clone())))
        })
    }

    fn red(p: &VideoPlayback, pose: Pose) -> u8 {
        p.frame(pose).expect("再生中").data()[0]
    }

    fn secs(s: f64) -> Duration {
        Duration::from_secs_f64(s)
    }

    /// 24fps の動画の `n` 枚目の真ん中の時刻(境目の浮動小数点の誤差を避ける)。
    fn at(t0: Instant, n: f64) -> Instant {
        t0 + secs(n / 24.0)
    }

    #[test]
    fn video_file_names_follow_the_pose_names() {
        assert_eq!(video_file_name(Pose::Idle), "daidai-idle.mp4");
        assert_eq!(video_file_name(Pose::Working), "daidai-working.mp4");
        assert_eq!(video_file_name(Pose::Milestone), "daidai-milestone.mp4");
        assert_eq!(video_file_name(Pose::Making), "daidai-making.mp4");
        // PNG と同じ名前の拡張子違い(素材の差し替えの約束)
        for p in Pose::ALL {
            assert_eq!(
                video_file_name(p).trim_end_matches(".mp4"),
                format!("daidai-{}", p.name())
            );
        }
    }

    #[test]
    fn frame_number_follows_the_wall_clock() {
        assert_eq!(frame_number(secs(0.0), 24.0), 0);
        // 境目(1/24 秒 = 41,666,666.67 ns)の前後
        assert_eq!(frame_number(Duration::from_nanos(41_666_000), 24.0), 0);
        assert_eq!(frame_number(Duration::from_nanos(41_667_000), 24.0), 1);
        assert_eq!(frame_number(secs(7.99), 24.0), 191);
        assert_eq!(
            frame_number(secs(8.0), 24.0),
            192,
            "8 秒で一周(通し番号は増え続ける)"
        );
        assert_eq!(frame_number(secs(16.0), 24.0), 384);
    }

    #[test]
    fn plays_the_frame_of_the_wall_clock_and_wraps_at_the_loop_boundary() {
        let stats = Rc::new(RefCell::new(Stats::default()));
        let opens = Rc::new(RefCell::new(Vec::new()));
        let mut v = VideoPlayback::new(Some(fake_opener(192, 24.0, &[], stats.clone(), opens)), 24);
        let t0 = Instant::now();
        v.update(Pose::Working, t0);
        assert!(v.playing());
        assert_eq!(red(&v, Pose::Working), 0, "最初は先頭");
        // 1 枚ずつ 192 枚目(番号 191)まで。まだ 1 周目
        for k in 1..192 {
            v.update(Pose::Working, at(t0, f64::from(k) + 0.5));
            assert_eq!(red(&v, Pose::Working), k as u8);
        }
        assert_eq!(stats.borrow().loops, 0);
        // 8 秒を過ぎると先頭に戻る
        v.update(Pose::Working, at(t0, 192.5));
        assert_eq!(red(&v, Pose::Working), 0);
        assert_eq!(stats.borrow().loops, 1);
        v.update(Pose::Working, at(t0, 195.5));
        assert_eq!(red(&v, Pose::Working), 3);
    }

    #[test]
    fn a_lower_redraw_rate_skips_frames_without_slowing_the_clip_or_converting_them() {
        let stats = Rc::new(RefCell::new(Stats::default()));
        let opens = Rc::new(RefCell::new(Vec::new()));
        let mut v = VideoPlayback::new(Some(fake_opener(192, 24.0, &[], stats.clone(), opens)), 12);
        let t0 = Instant::now();
        v.update(Pose::Idle, t0);
        // 12 回描き直す間(1 秒)に 24 枚進む。画素にするのは描いた 12 枚(+ 最初の 1 枚)だけ
        for i in 1..=12 {
            v.update(Pose::Idle, at(t0, f64::from(i) * 2.0 + 0.5));
        }
        assert_eq!(red(&v, Pose::Idle), 24, "速さは動画のまま");
        let s = stats.borrow();
        assert_eq!(s.decoded, 25);
        assert_eq!(s.converted, 13, "読み飛ばした枚は画素にしない: {s:?}");
    }

    #[test]
    fn a_long_gap_resyncs_the_clock_instead_of_decoding_everything_at_once() {
        let stats = Rc::new(RefCell::new(Stats::default()));
        let opens = Rc::new(RefCell::new(Vec::new()));
        let mut v = VideoPlayback::new(Some(fake_opener(192, 24.0, &[], stats.clone(), opens)), 12);
        let t0 = Instant::now();
        v.update(Pose::Idle, t0);
        // 10 秒止まったあと: 240 枚ぶん読まず、次の 1 枚だけ読む
        v.update(Pose::Idle, at(t0, 240.5));
        assert_eq!(stats.borrow().decoded, 2);
        assert_eq!(red(&v, Pose::Idle), 1);
        // 時計は合い直しているので、次の周期からは普通に進む(3 枚ぶん)
        v.update(Pose::Idle, at(t0, 243.5));
        assert_eq!(stats.borrow().decoded, 5);
        assert_eq!(red(&v, Pose::Idle), 4);
    }

    #[test]
    fn a_pose_change_switches_to_that_poses_clip_from_its_start() {
        let stats = Rc::new(RefCell::new(Stats::default()));
        let opens = Rc::new(RefCell::new(Vec::new()));
        let mut v = VideoPlayback::new(Some(fake_opener(192, 24.0, &[], stats, opens.clone())), 12);
        let t0 = Instant::now();
        v.update(Pose::Idle, t0);
        for k in 1..24 {
            v.update(Pose::Idle, at(t0, f64::from(k) + 0.5));
        }
        assert_eq!(red(&v, Pose::Idle), 23);
        v.update(Pose::Making, at(t0, 24.5));
        assert!(v.frame(Pose::Idle).is_none(), "前のポーズの絵は出さない");
        assert_eq!(red(&v, Pose::Making), 0, "新しいポーズは先頭から");
        assert_eq!(*opens.borrow(), [Pose::Idle, Pose::Making]);
        // 同じポーズのあいだは開き直さず、新しい動画の時計で進む
        v.update(Pose::Making, at(t0, 26.5));
        assert_eq!(opens.borrow().len(), 2);
        assert_eq!(red(&v, Pose::Making), 2);
    }

    #[test]
    fn a_missing_or_broken_clip_falls_back_to_png_once_and_the_others_still_play() {
        let stats = Rc::new(RefCell::new(Stats::default()));
        let opens = Rc::new(RefCell::new(Vec::new()));
        let mut v = VideoPlayback::new(
            Some(fake_opener(
                192,
                24.0,
                &[Pose::Working],
                stats,
                opens.clone(),
            )),
            12,
        );
        let t0 = Instant::now();
        v.update(Pose::Working, t0);
        assert!(!v.playing(), "開けないポーズは動画なし(PNG)");
        assert!(v.frame(Pose::Working).is_none());
        // 何度呼んでも、開き直さない(毎周期ファイルを探しに行かない)
        for i in 1..5 {
            v.update(Pose::Working, t0 + secs(f64::from(i)));
        }
        assert_eq!(opens.borrow().len(), 1);
        // ほかのポーズは再生できる
        v.update(Pose::Idle, t0 + secs(5.0));
        assert!(v.playing());
        // 開けなかったポーズに戻ってもPNG
        v.update(Pose::Working, t0 + secs(6.0));
        assert!(!v.playing());
    }

    #[test]
    fn a_source_that_fails_midway_drops_back_to_png() {
        struct Dies(u32);
        impl FrameSource for Dies {
            fn decode_next(&mut self) -> Result<(), String> {
                if self.0 == 0 {
                    return Err("壊れた".into());
                }
                self.0 -= 1;
                Ok(())
            }
            fn frame(&mut self) -> Result<Pixmap, String> {
                Ok(Pixmap::new(1, 1).unwrap())
            }
            fn fps(&self) -> f64 {
                24.0
            }
        }
        let mut v = VideoPlayback::new(Some(Box::new(|_| Ok(Box::new(Dies(3))))), 12);
        let t0 = Instant::now();
        v.update(Pose::Idle, t0);
        assert!(v.playing());
        for k in 1..10 {
            v.update(Pose::Idle, at(t0, f64::from(k) + 0.5));
        }
        assert!(!v.playing(), "読めなくなったら PNG に戻る");
        v.update(Pose::Idle, at(t0, 20.5));
        assert!(!v.playing(), "壊れた動画は開き直さない");
        // 最初の 1 枚も読めない動画は、再生を始めない
        let mut v = VideoPlayback::new(Some(Box::new(|_| Ok(Box::new(Dies(0))))), 12);
        v.update(Pose::Idle, t0);
        assert!(!v.playing());
    }

    #[test]
    fn a_bad_frame_rate_is_not_played() {
        for fps in [0.0, -1.0, 0.5, 500.0, f64::NAN] {
            let stats = Rc::new(RefCell::new(Stats::default()));
            let opens = Rc::new(RefCell::new(Vec::new()));
            let mut v = VideoPlayback::new(Some(fake_opener(10, fps, &[], stats, opens)), 12);
            v.update(Pose::Idle, Instant::now());
            assert!(!v.playing(), "fps={fps}");
        }
    }

    #[test]
    fn stop_drops_the_decoder_and_reads_nothing_until_restarted() {
        let stats = Rc::new(RefCell::new(Stats::default()));
        let opens = Rc::new(RefCell::new(Vec::new()));
        let mut v = VideoPlayback::new(Some(fake_opener(192, 24.0, &[], stats.clone(), opens)), 12);
        let t0 = Instant::now();
        v.update(Pose::Idle, t0);
        v.update(Pose::Idle, t0 + secs(1.0));
        let before = stats.borrow().decoded;
        v.stop();
        assert!(!v.playing());
        assert_eq!(stats.borrow().decoded, before);
        // 戻ったら先頭から
        v.update(Pose::Idle, t0 + secs(30.0));
        assert_eq!(red(&v, Pose::Idle), 0);
    }

    #[test]
    fn without_an_opener_nothing_plays() {
        let mut v = VideoPlayback::none();
        v.update(Pose::Idle, Instant::now());
        assert!(!v.playing());
        assert!(
            dir_opener("").is_none(),
            "素材の置き場が空なら動画は使わない"
        );
    }

    #[test]
    fn the_redraw_interval_is_the_cap() {
        for (fps, ms) in [(12u32, 83), (30, 33), (1, 1000)] {
            let v = VideoPlayback::new(None, fps);
            assert_eq!(v.interval().as_millis(), ms, "{fps}");
        }
    }

    #[test]
    fn dir_opener_reports_missing_oversized_and_broken_files_as_errors() {
        let dir = std::env::temp_dir().join(format!("miharashi-daidai-vid-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let open = dir_opener(dir.to_str().unwrap()).unwrap();
        // 無い
        assert!(open(Pose::Idle).is_err());
        // 壊れている(またはこのビルドに動画の機能が無い)
        std::fs::write(dir.join(video_file_name(Pose::Working)), b"not an mp4").unwrap();
        assert!(open(Pose::Working).is_err());
        // 空
        std::fs::write(dir.join(video_file_name(Pose::Making)), b"").unwrap();
        assert!(open(Pose::Making).is_err());
        // 大きすぎる(中身は読まずに断る)
        let big = std::fs::File::create(dir.join(video_file_name(Pose::Milestone))).unwrap();
        big.set_len(MAX_VIDEO_BYTES + 1).unwrap();
        let e = open(Pose::Milestone).err().expect("大きすぎる");
        assert!(e.contains("超えている"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 同梱の素材 4 本が、本物のデコーダーで読めて、壁時計どおりに進む(`video` つきのビルドだけ)。
    #[cfg(all(feature = "video", target_os = "linux"))]
    #[test]
    fn the_bundled_clips_play_through_the_real_decoder() {
        let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../miharashi-render/assets/daidai");
        let open = dir_opener(assets.to_str().unwrap()).unwrap();
        let t0 = Instant::now();
        for pose in Pose::ALL {
            let mut v = VideoPlayback::new(Some(dir_opener(assets.to_str().unwrap()).unwrap()), 12);
            v.update(pose, t0);
            let first = v.frame(pose).expect("再生できる").clone();
            assert_eq!((first.width(), first.height()), (480, 480), "{pose:?}");
            // 12 回/秒で 1 周(192 枚 = 96 回)。4 秒後の絵は先頭と違い(動いている)、8 秒で先頭に戻る
            for i in 1..=96 {
                v.update(pose, at(t0, f64::from(i) * 2.0 + 0.5));
                if i == 48 {
                    assert_ne!(
                        v.frame(pose).unwrap().data(),
                        first.data(),
                        "{pose:?}: 動いている"
                    );
                }
            }
            assert_eq!(
                v.frame(pose).unwrap().data(),
                first.data(),
                "{pose:?}: 一周して先頭"
            );
            assert!(open(pose).is_ok());
        }
    }

    /// 本物のデコーダーでも、壊れたファイルは PNG に戻る。
    #[cfg(all(feature = "video", target_os = "linux"))]
    #[test]
    fn a_corrupt_mp4_through_the_real_decoder_falls_back() {
        let dir = std::env::temp_dir().join(format!("miharashi-daidai-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../miharashi-render/assets/daidai/daidai-idle.mp4");
        let bytes = std::fs::read(src).unwrap();
        // 途中で切れている(moov が末尾にあれば開けない / 先頭だけ読めて途中で終わる)
        std::fs::write(dir.join("daidai-idle.mp4"), &bytes[..bytes.len() / 3]).unwrap();
        // 先頭が壊れている
        let mut head = bytes.clone();
        head[..64].fill(0xff);
        std::fs::write(dir.join("daidai-working.mp4"), head).unwrap();
        // 中身が H.264 でない(拡張子だけ mp4)
        std::fs::write(dir.join("daidai-making.mp4"), vec![0u8; 4096]).unwrap();
        let mut v = VideoPlayback::new(dir_opener(dir.to_str().unwrap()), 12);
        let t0 = Instant::now();
        for (i, pose) in [Pose::Idle, Pose::Working, Pose::Making]
            .into_iter()
            .enumerate()
        {
            // 切れた動画は、読めるところまで再生して、読めなくなったら PNG に戻る(落ちない)
            for k in 0..400 {
                v.update(
                    pose,
                    t0 + secs(f64::from(i as u32 * 1000 + k) / 12.0 + 0.01),
                );
            }
        }
        // ここまで panic せず来られればよい。壊れた動画は最後には PNG(frame が無い)
        assert!(v.frame(Pose::Making).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
