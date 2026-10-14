//! `miharashi bench-gpu`: 将来の重いエフェクトに向けた GPU のベンチマーク(PoC-005 PR4)。
//! B1 コネクトーム: MaleCNS 全体級(ニューロン 16 万、結合 300 万本)の合成グラフを、表示カードの
//! render node に surfaceless で描く。表示中の miharashi を止めずに測れる(KMS には出さない)。
//! 1 秒ごとに fps、GPU の時間、CPU 使用率、iGPU の使用率、CPU パッケージの電力を出す。
use miharashi_gpu::connectome::{self, FrameTimes, Lines, RunOpts, Scene};
use miharashi_linux::sysinfo::{RaplTracker, SelfCpu};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub struct Opts {
    /// 表示カードの PCI アドレス(この GPU の render node に描く)
    pub pci: String,
    pub size: (u32, u32),
    pub duration: Duration,
    pub fps: u32,
    pub neurons: u32,
    pub synapses: u32,
    pub stim: usize,
    pub gain: f32,
    pub lines: Lines,
    pub rapl_helper: PathBuf,
    /// 最後のフレームを PNG で保存する先
    pub out: Option<PathBuf>,
}

/// 1 秒ごとと全体の集計に使う値。
#[derive(Default)]
struct Window {
    frames: u32,
    step: Vec<f32>,
    draw: Vec<f32>,
    total: Vec<f32>,
    submit: Vec<f32>,
    wait: Vec<f32>,
}

impl Window {
    fn push(&mut self, t: &FrameTimes) {
        self.frames += 1;
        self.submit.push(t.submit);
        self.wait.push(t.wait);
        if let (Some(s), Some(d)) = (t.gpu_step, t.gpu_draw) {
            self.step.push(s);
            self.draw.push(d);
            self.total.push(s + d);
        }
    }
}

/// 平均と p95。空なら None。
fn mean_p95(v: &mut [f32]) -> Option<(f32, f32)> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f32::total_cmp);
    let mean = v.iter().sum::<f32>() / v.len() as f32;
    let p95 = v[((v.len() - 1) as f32 * 0.95).round() as usize];
    Some((mean, p95))
}

fn fmt_ms(v: &mut [f32]) -> String {
    mean_p95(v).map_or("-".into(), |(m, p)| format!("{m:.2}/{p:.2}"))
}

fn fmt_opt(v: Option<f32>, digits: usize) -> String {
    v.map_or("-".into(), |x| format!("{x:.digits$}"))
}

fn sysfs_u64(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn mib(v: Option<u64>) -> String {
    v.map_or("-".into(), |b| format!("{}MB", b >> 20))
}

pub fn run(o: &Opts) -> Result<(), String> {
    let dev = PathBuf::from(format!("/sys/bus/pci/devices/{}", o.pci));
    let vram = || sysfs_u64(&dev.join("mem_info_vram_used"));
    let gtt = || sysfs_u64(&dev.join("mem_info_gtt_used"));
    let busy = || sysfs_u64(&dev.join("gpu_busy_percent")).map(|v| v as f32);

    let node = miharashi_gpu::render_node_for_pci(&o.pci);
    let off = miharashi_gpu::Offscreen::open(&node)?;
    println!("render node: {}", node.display());

    let t0 = Instant::now();
    let graph = connectome::synth(o.neurons, o.synapses, 1);
    println!(
        "graph: neurons={} synapses={} synth={:.0}ms",
        o.neurons,
        o.synapses,
        t0.elapsed().as_secs_f32() * 1000.0
    );
    let (vram0, gtt0) = (vram(), gtt());

    let opts = RunOpts {
        size: o.size,
        duration: o.duration,
        fps: o.fps,
        stim: o.stim,
        seed: 1,
    };
    let (result, img) = off.with_fbo(o.size, |gl| -> Result<f32, String> {
        println!("gl: {}", connectome::caps(gl));
        let t0 = Instant::now();
        let mut scene = Scene::new(gl, &graph)?;
        scene.gain = o.gain;
        scene.lines = o.lines;
        println!(
            "upload: {:.0}ms / vram {} → {} / gtt {} → {}",
            t0.elapsed().as_secs_f32() * 1000.0,
            mib(vram0),
            mib(vram()),
            mib(gtt0),
            mib(gtt())
        );
        println!(
            "run: {}x{} fps={} stim={} gain={} lines={:?} {}s",
            o.size.0,
            o.size.1,
            if o.fps == 0 { "max".into() } else { o.fps.to_string() },
            o.stim,
            o.gain,
            o.lines,
            o.duration.as_secs()
        );
        let mut cpu = SelfCpu::default();
        let mut rapl = RaplTracker::new(&o.rapl_helper);
        let now = Instant::now();
        cpu.poll(now);
        rapl.poll(now);
        let mut cpu_total = SelfCpu::default();
        cpu_total.poll(now);
        let (mut sec, mut all) = (Window::default(), Window::default());
        let (mut busy_s, mut watts_s, mut cpu_s) = (Vec::new(), Vec::new(), Vec::new());
        let mut next = Duration::from_secs(1);
        connectome::run(gl, &mut scene, &opts, |_, elapsed, t| {
            sec.push(t);
            all.push(t);
            if elapsed < next {
                return;
            }
            next += Duration::from_secs(1);
            let now = Instant::now();
            let (c, w, b) = (cpu.poll(now), rapl.poll(now), busy());
            cpu_s.extend(c);
            watts_s.extend(w);
            busy_s.extend(b);
            println!(
                "t={:>4}s frames={:>3} gpu={} (step {} / draw {}) submit={} wait={} cpu={}% busy={}% W={}",
                elapsed.as_secs(),
                sec.frames,
                fmt_ms(&mut sec.total),
                fmt_ms(&mut sec.step),
                fmt_ms(&mut sec.draw),
                fmt_ms(&mut sec.submit),
                fmt_ms(&mut sec.wait),
                fmt_opt(c, 1),
                fmt_opt(b, 0),
                fmt_opt(w, 1),
            );
            sec = Window::default();
        })?;
        let cpu_run = cpu_total.poll(Instant::now());
        let active = scene.active_fraction(gl);
        let lit = scene.active_lines(gl);
        scene.destroy(gl);
        let avg = |v: &[f32]| (!v.is_empty()).then(|| v.iter().sum::<f32>() / v.len() as f32);
        let fps = all.frames as f32 / o.duration.as_secs_f32();
        // GPU が埋まっている割合(タイマーで測った時間 × fps)。gpu_busy_percent はこの GPU では当てにならない
        let gpu_load = avg(&all.total).map(|ms| ms * fps / 10.0);
        println!(
            "summary: frames={} fps={:.1} gpu={} gpu_load={}% step={} draw={} submit={} cpu={}% busy={}% W={} active={:.2}% lit_lines={}",
            all.frames,
            fps,
            fmt_ms(&mut all.total),
            fmt_opt(gpu_load, 0),
            fmt_ms(&mut all.step),
            fmt_ms(&mut all.draw),
            fmt_ms(&mut all.submit),
            fmt_opt(cpu_run, 2),
            fmt_opt(avg(&busy_s), 0),
            fmt_opt(avg(&watts_s), 1),
            active * 100.0,
            lit,
        );
        println!("        (ms は 平均/p95。cpu は計測区間全体の自プロセス、1 コア = 100%。busy は gpu_busy_percent の 1 秒ごとの平均)");
        Ok(active)
    })?;
    result?;
    if let Some(path) = &o.out {
        image::RgbaImage::from_raw(o.size.0, o.size.1, img)
            .ok_or("画像の大きさが合わない")?
            .save(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        println!("png: {}", path.display());
    }
    Ok(())
}
