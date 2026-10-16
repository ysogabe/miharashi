//! 状況画面の目視確認用。全項目を埋めた Snapshot と、ほとんど None の Snapshot を
//! 1920x1080 / 2560x1080 で /tmp/claude-status-demo/ に書き出す。
use miharashi_render::screens::{GpuLoad, Snapshot, StatusPage, status};
use miharashi_render::{Canvas, TextEngine};

fn wave(n: usize, base: f32, amp: f32, phase: f32) -> Vec<f32> {
    (0..n)
        .map(|i| {
            let p = phase + i as f32;
            (base + amp * (p * 0.21).sin() + amp * 0.5 * (p * 0.67).sin()).max(0.0)
        })
        .collect()
}

fn full() -> Snapshot {
    let n = 120;
    Snapshot {
        date: "2026/10/12 (月)".into(),
        time: "16:05:12".into(),
        cpu: Some(13.0),
        cpu_temp: Some(63.0),
        gpu: Some(97.0),
        gpu_temp: Some(84.0),
        mem_used: Some(3.3),
        mem_total: Some(31.0),
        ssd_temps: vec![
            ("Lexar NM790".into(), 44.0),
            ("WD_BLACK".into(), 40.0),
            ("Samsung 990 PRO".into(), 92.0),
        ],
        fans: vec![
            ("CPU/CS".into(), "1200 rpm".into()),
            ("GPU".into(), "32%".into()),
            ("SYS_FAN1".into(), "--".into()),
        ],
        cpu_history: wave(n, 30.0, 20.0, 0.0),
        gpu_history: wave(n, 60.0, 30.0, 40.0),
        // 3 台(NVIDIA 2 台 + 内蔵、内蔵の温度は取れない想定)
        gpus: vec![
            GpuLoad {
                label: "GPU0".into(),
                name: "RTX 5090".into(),
                usage: Some(97.0),
                temp: Some(84.0),
                history: wave(n, 60.0, 30.0, 40.0),
                mem_used_mib: Some(21500),
                mem_total_mib: Some(31838),
                mem_history: wave(n, 68.0, 8.0, 30.0),
            },
            GpuLoad {
                label: "GPU1".into(),
                name: "RTX 4090".into(),
                usage: Some(41.0),
                temp: Some(66.0),
                history: wave(n, 40.0, 20.0, 70.0),
                mem_used_mib: Some(6100),
                mem_total_mib: Some(23788),
                mem_history: wave(n, 26.0, 5.0, 60.0),
            },
            GpuLoad {
                label: "GPU2".into(),
                name: "Radeon(内蔵)".into(),
                usage: Some(3.0),
                temp: None,
                history: wave(n, 5.0, 4.0, 90.0),
                mem_used_mib: Some(263),
                mem_total_mib: Some(15719),
                mem_history: wave(n, 1.7, 0.3, 80.0),
            },
        ],
        cpu_power_w: Some(95.0),
        gpu_power_w: Some(142.0),
        cpu_energy_today_wh: Some(1200.0),
        gpu_energy_today_wh: Some(900.0),
        total_energy_wh: Some(123_456.0),
        price_per_kwh: Some(31.0),
        cpu_power_history: wave(n, 80.0, 40.0, 3.0),
        gpu_power_history: wave(n, 140.0, 60.0, 50.0),
        psi_cpu: Some(35.0),
        psi_mem: Some(0.0),
        psi_io: Some(2.0),
        swap_used_gb: Some(3.1),
        swap_total_gb: Some(8.0),
        load_avg: Some([2.5, 1.9, 1.2]),
        vram_used_mib: Some(21_500),
        vram_total_mib: Some(31_838),
        gpu_clock_mhz: Some(180),
        gpu_clock_max_mhz: Some(2520),
        gpu_pstate: Some("P8".into()),
        gpu_processes: vec![
            ("ollama_llama_server".into(), 4242, 9_800),
            ("Xorg".into(), 1337, 120),
            ("chromium-browser --type=gpu-process".into(), 98765, 480),
        ],
        net_iface: Some("enp11s0".into()),
        net_rx_mbps: Some(12.4),
        net_tx_mbps: Some(1.2),
        net_rx_history: wave(n, 120.0, 100.0, 7.0),
        net_tx_history: wave(n, 20.0, 15.0, 9.0),
        psi_cpu_history: wave(n, 30.0, 25.0, 11.0),
        psi_mem_history: wave(n, 10.0, 9.0, 21.0),
        disk_root_used_pct: Some(41.0),
        disk_root_used_total_gb: Some((412.0, 953.0)),
        disk_io: vec![
            ("nvme0n1".into(), 0.1, 0.0),
            ("nvme1n1".into(), 2.5, 8.2),
            ("sda".into(), 0.0, 0.0),
        ],
        failed_units: Some(0),
        feed_status: Some("online".into()),
        peaks_today: vec![
            ("電力".into(), "291 W".into(), "15:40".into()),
            ("GPU 温度".into(), "84°C".into(), "15:42".into()),
            ("CPU 温度".into(), "78°C".into(), "11:03".into()),
            ("受信".into(), "412.5 Mbps".into(), "09:12".into()),
            ("VRAM".into(), "14200 MiB".into(), "13:55".into()),
        ],
    }
}

fn sparse() -> Snapshot {
    Snapshot {
        date: "2026/10/12 (月)".into(),
        time: "16:05:12".into(),
        cpu: Some(5.0),
        ..Default::default()
    }
}

fn main() -> Result<(), String> {
    let path = std::path::Path::new("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc");
    let mut t = TextEngine::from_font_file("Noto Sans CJK JP", path).map_err(|e| e.to_string())?;
    let out = std::path::Path::new("/tmp/claude-status-demo");
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    for (name, snap) in [("full", full()), ("sparse", sparse())] {
        for (w, h) in [(1920, 1080), (2560, 1080)] {
            for (pname, page) in [
                ("overview", StatusPage::Overview),
                ("details", StatusPage::Details),
            ] {
                let mut c = Canvas::new(w, h);
                status(&mut c, &mut t, &snap, page);
                let file = out.join(format!("{name}-{pname}-{w}x{h}.png"));
                c.save_png(&file)?;
                println!("{}", file.display());
            }
        }
    }
    Ok(())
}
