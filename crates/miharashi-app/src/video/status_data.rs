//! 状況画面の動画用の仮データ。実機らしい緩やかな変動を、秒番号から決まる疑似乱数で作る(実時計も乱数源も読まない)。
//! 仮想時刻の 1 秒ごとに 1 サンプル進み、各値の最新値は履歴の末尾と一致する。
//! `DummyData`(テスト・`--dummy`)とは別で、本番の経路には使わない。
use crate::data::{self, HISTORY};
use miharashi_render::screens::Snapshot;

/// 整数と種から [0, 1) の値(SplitMix64 の仕上げ)。
fn hash(i: i64, salt: u64) -> f32 {
    let mut x =
        (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ salt.wrapping_mul(0xD1B5_4A32_D192_ED03);
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    (x >> 40) as f32 / (1u64 << 24) as f32
}

/// なめらかな値ノイズ(`period` 秒ごとの格子点を滑らかにつなぐ)。[0, 1)
fn noise(t: i64, period: f32, salt: u64) -> f32 {
    let x = t as f32 / period;
    let i = x.floor();
    let f = x - i;
    let s = f * f * (3.0 - 2.0 * f);
    let (a, b) = (hash(i as i64, salt), hash(i as i64 + 1, salt));
    a + (b - a) * s
}

/// 負荷のかかった時間帯。`window` 秒ごとに `chance` の確率で、15〜30 秒ほど、緩やかに立ち上がって緩やかに下がる。0〜1
fn burst(t: i64, window: i64, chance: f32, salt: u64) -> f32 {
    let w = t.div_euclid(window);
    let r = t.rem_euclid(window) as f32;
    if hash(w, salt) >= chance {
        return 0.0;
    }
    let start = 0.1 * window as f32 + 0.4 * window as f32 * hash(w, salt + 1);
    let len = 15.0 + 15.0 * hash(w, salt + 2);
    let u = (r - start) / len;
    let ramp = |x: f32| {
        let x = x.clamp(0.0, 1.0);
        x * x * (3.0 - 2.0 * x)
    };
    // 上がりは速め(u の 0〜0.3)、下がりはゆっくり(最後の 0.5)
    ramp(u / 0.3) * ramp((1.0 - u) / 0.5)
}

fn cpu(t: i64) -> f32 {
    let b = burst(t, 100, 0.45, 100);
    (14.0
        + 10.0 * noise(t, 9.0, 1)
        + 4.0 * noise(t, 3.0, 2)
        + 55.0 * b * (0.7 + 0.3 * noise(t, 5.0, 3)))
    .clamp(1.0, 99.0)
}

fn gpu(t: i64) -> f32 {
    let b = burst(t, 100, 0.45, 100);
    (8.0 + 10.0 * noise(t, 11.0, 4)
        + 3.0 * noise(t, 3.0, 5)
        + 70.0 * b * (0.75 + 0.25 * noise(t, 6.0, 6)))
    .clamp(0.0, 99.0)
}

fn vram_pct(t: i64) -> f32 {
    (46.0 + 6.0 * noise(t, 40.0, 7) + 22.0 * burst(t, 100, 0.45, 100)).clamp(0.0, 99.0)
}

fn igpu(t: i64) -> f32 {
    (2.0 + 4.0 * noise(t, 7.0, 10) + 2.0 * noise(t, 2.0, 11) + 6.0 * burst(t, 100, 0.45, 100))
        .clamp(0.0, 99.0)
}

fn net_rx(t: i64) -> f32 {
    let n = noise(t, 8.0, 8);
    6.0 + 30.0 * n * n + 320.0 * burst(t, 70, 0.35, 200) * (0.6 + 0.4 * noise(t, 4.0, 9))
}

fn net_tx(t: i64) -> f32 {
    2.0 + 6.0 * noise(t, 10.0, 12) + 0.06 * net_rx(t)
}

fn psi_cpu(t: i64) -> f32 {
    (0.12 * (cpu(t) - 20.0)).max(0.0)
}

fn psi_mem(t: i64) -> f32 {
    1.0 + 2.0 * noise(t, 15.0, 13) + 3.0 * burst(t, 100, 0.45, 100)
}

/// 秒 `t` までの `HISTORY` 秒分(古い順)。末尾が秒 `t` の値。
fn history(t: i64, f: impl Fn(i64) -> f32) -> Vec<f32> {
    (t - HISTORY as i64 + 1..=t).map(f).collect()
}

/// 動画の `second` 秒目(任意の整数。履歴はその前の `HISTORY` 秒を含む)の状況画面のデータ。
pub fn snapshot(second: i64) -> Snapshot {
    let t = second;
    let mut s = data::dummy_snapshot(0.0);
    let (c, g, v) = (cpu(t), gpu(t), vram_pct(t));
    let ig = igpu(t);
    let (cpu_h, gpu_h) = (history(t, cpu), history(t, gpu));
    let (cpu_w, gpu_w) = (|t| 28.0 + 1.2 * cpu(t), |t| 18.0 + 2.0 * gpu(t));
    let vram_total = s.vram_total_mib.unwrap_or(15941);
    let vram_used = (v / 100.0 * vram_total as f32).round() as u32;

    s.cpu = Some(c);
    s.cpu_temp = Some(38.0 + c * 0.4);
    s.gpu = Some(g);
    s.gpu_temp = Some(40.0 + g * 0.35);
    s.cpu_history = cpu_h;
    s.gpu_history = gpu_h.clone();
    s.cpu_power_w = Some(cpu_w(t));
    s.gpu_power_w = Some(gpu_w(t));
    s.cpu_power_history = history(t, cpu_w);
    s.gpu_power_history = history(t, gpu_w);
    s.psi_cpu = Some(psi_cpu(t));
    s.psi_mem = Some(psi_mem(t));
    s.psi_cpu_history = history(t, psi_cpu);
    s.psi_mem_history = history(t, psi_mem);
    s.load_avg = Some([0.3 + c * 0.04, 0.3 + c * 0.035, 0.3 + c * 0.03]);
    s.vram_used_mib = Some(vram_used);
    s.gpu_clock_mhz = Some((300.0 + 25.0 * g) as u32);
    s.gpu_pstate = Some(
        if g >= 50.0 {
            "P0"
        } else if g >= 20.0 {
            "P2"
        } else {
            "P8"
        }
        .into(),
    );
    s.net_rx_mbps = Some(net_rx(t));
    s.net_tx_mbps = Some(net_tx(t));
    s.net_rx_history = history(t, net_rx);
    s.net_tx_history = history(t, net_tx);
    if let Some(d) = s.gpus.first_mut() {
        d.usage = Some(g);
        d.temp = s.gpu_temp;
        d.history = gpu_h;
        d.mem_used_mib = Some(vram_used);
        d.mem_history = history(t, vram_pct);
    }
    if let Some(d) = s.gpus.get_mut(1) {
        d.usage = Some(ig);
        d.history = history(t, igpu);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 最新値と履歴の末尾が一致し、履歴は 1 秒に 1 サンプルずつ進む。
    #[test]
    fn latest_matches_history_tail_and_advances_one_sample() {
        let (a, b) = (snapshot(700), snapshot(701));
        assert_eq!(a.cpu, a.cpu_history.last().copied());
        assert_eq!(a.gpu, a.gpu_history.last().copied());
        assert_eq!(a.net_rx_mbps, a.net_rx_history.last().copied());
        assert_eq!(a.psi_cpu, a.psi_cpu_history.last().copied());
        let g = &a.gpus[0];
        assert_eq!(g.usage, g.history.last().copied());
        let used = g.mem_used_mib.unwrap() as f32 / g.mem_total_mib.unwrap() as f32 * 100.0;
        assert!((used - g.mem_history.last().unwrap()).abs() < 0.01);
        assert_eq!(a.cpu_history[1..], b.cpu_history[..HISTORY - 1]);
        assert_eq!(
            a.gpus[0].mem_history[1..],
            b.gpus[0].mem_history[..HISTORY - 1]
        );
    }

    /// 履歴は緩やかに動く(隣り合う 1 秒でほぼ飛ばない)。値は範囲内。
    #[test]
    fn series_are_smooth_and_bounded() {
        let s = snapshot(900);
        for h in [&s.cpu_history, &s.gpu_history, &s.gpus[0].mem_history] {
            assert_eq!(h.len(), HISTORY);
            assert!(h.iter().all(|v| (0.0..=100.0).contains(v)));
            let max_step = h
                .windows(2)
                .map(|w| (w[1] - w[0]).abs())
                .fold(0.0, f32::max);
            assert!(max_step < 14.0, "隣り合う秒の差 {max_step}");
        }
    }

    #[test]
    fn same_second_gives_same_data() {
        assert_eq!(snapshot(1234).cpu_history, snapshot(1234).cpu_history);
    }
}
