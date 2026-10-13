//! フォトフレーム画面。ぼかし背景と写真は完成フレームとして渡され、ここでは貼って文字を重ねるだけ。
use crate::canvas::rgba;
use crate::photo_layout::{IRect, Mode, PhotoLayout, scale};
use crate::screens::Snapshot;
use crate::{Canvas, TextEngine};
use tiny_skia::Pixmap;

/// フォトフレームに表示する写真の情報。
pub struct PhotoInfo {
    pub place: String,
    /// 表示用の撮影日時(例: "撮影 2026/03/08 13:22")。不明なら空。
    pub taken: String,
    /// 撮影からの経過(例: "7か月前")。不明なら空。
    pub age: String,
    /// (現在位置 1 始まり, 総数)
    pub position: (usize, usize),
}

const WHITE: [u8; 4] = [255, 255, 255, 255];
const SOFT: [u8; 4] = [215, 222, 230, 255];
const DIM: [u8; 4] = [160, 175, 192, 255];

/// 文字列の幅が `max_w` に収まるよう、`size` 以下の大きさを返す。
fn fit(t: &mut TextEngine, text: &str, size: f32, max_w: f32) -> f32 {
    let w = t.measure(text, size);
    if w > max_w && w > 0.0 {
        size * max_w / w
    } else {
        size
    }
}

/// 時刻文字列("HH:MM:SS" など)から "HH:MM" を取り出す。
fn hhmm(time: &str) -> &str {
    time.get(..5).unwrap_or(time)
}

/// 焼き付き防止の位置ずれ(-8..=8 px、1 分ごとに変わる)。`time` は "HH:MM…"。
pub fn drift(time: &str) -> (i32, i32) {
    let n =
        |r: std::ops::Range<usize>| time.get(r).and_then(|s| s.parse::<i32>().ok()).unwrap_or(0);
    let m = n(0..2) * 60 + n(3..5);
    ((m * 7 + 3) % 17 - 8, (m * 11 + 5) % 17 - 8)
}

enum Item {
    Text(String, f32, [u8; 4]),
    Gap(f32),
    Rule,
}

/// 項目を縦に並べ、`area` の中央に置く(`off` は焼き付き防止のずれ)。
fn draw_block(
    c: &mut Canvas,
    t: &mut TextEngine,
    area: IRect,
    items: &[Item],
    off: (f32, f32),
    k: f32,
) {
    let h_of = |it: &Item| match it {
        Item::Text(_, size, _) => size * 1.25,
        Item::Gap(h) => *h,
        Item::Rule => 2.0 * k.max(1.0),
    };
    let total: f32 = items.iter().map(h_of).sum();
    let width = items
        .iter()
        .map(|it| match it {
            Item::Text(s, size, _) => t.measure(s, *size),
            _ => 0.0,
        })
        .fold(0.0, f32::max);
    let x = area.x as f32 + (area.w as f32 - width) / 2.0 + off.0;
    let mut y = area.y as f32 + (area.h as f32 - total) / 2.0 + off.1;
    for it in items {
        match it {
            Item::Text(s, size, col) => t.draw(c, s, x, y, *size, *col),
            Item::Rule => c.fill_rect(x, y, width, 2.0 * k.max(1.0), rgba(255, 255, 255, 70)),
            Item::Gap(_) => {}
        }
        y += h_of(it);
    }
}

/// 時刻と日付(常に大きく)。`avail` は文字に使える幅。
fn clock_items(t: &mut TextEngine, s: &Snapshot, avail: f32, k: f32) -> Vec<Item> {
    let probe = 100.0;
    let clock = (200.0 * k).min(probe * avail / t.measure("00:00", probe));
    let date = fit(t, &s.date, clock * 0.32, avail);
    vec![
        Item::Text(hhmm(&s.time).to_string(), clock, WHITE),
        Item::Gap(clock * 0.04),
        Item::Text(s.date.clone(), date, SOFT),
    ]
}

/// 写真の情報(場所・撮影日時・経過・位置)。`split_place` なら場所を空白で改行する。
fn info_items(
    t: &mut TextEngine,
    info: &PhotoInfo,
    avail: f32,
    k: f32,
    split_place: bool,
) -> Vec<Item> {
    let mut out = Vec::new();
    let place_size = if split_place { 40.0 } else { 36.0 } * k;
    let parts: Vec<&str> = if split_place {
        info.place.split_whitespace().collect()
    } else {
        vec![info.place.as_str()]
    };
    for p in parts.into_iter().filter(|p| !p.is_empty()) {
        let size = fit(t, p, place_size, avail);
        out.push(Item::Text(p.to_string(), size, WHITE));
    }
    out.push(Item::Gap(10.0 * k));
    for (text, size, col) in [
        (info.taken.clone(), 28.0, SOFT),
        (info.age.clone(), 28.0, SOFT),
        (
            format!("{} / {}", info.position.0, info.position.1),
            26.0,
            DIM,
        ),
    ] {
        if !text.is_empty() {
            let size = fit(t, &text, size * k, avail);
            out.push(Item::Text(text, size, col));
        }
    }
    out
}

/// 全面写真の下に敷く半透明の情報帯(時刻・日付・場所・撮影日時・位置)。
fn cover_band(
    c: &mut Canvas,
    t: &mut TextEngine,
    s: &Snapshot,
    info: Option<&PhotoInfo>,
    band: IRect,
) {
    let k = band.h as f32 / 130.0;
    let (x, y, w) = (band.x as f32, band.y as f32, band.w as f32);
    c.fill_rect(x, y, w, band.h as f32, rgba(0, 0, 0, 150));
    t.draw(c, hhmm(&s.time), x + 40.0 * k, y + 8.0 * k, 60.0 * k, WHITE);
    t.draw(c, &s.date, x + 40.0 * k, y + 80.0 * k, 30.0 * k, SOFT);
    if let Some(i) = info {
        let r = x + w - 40.0 * k;
        t.draw_right(c, &i.place, r, y + 14.0 * k, 38.0 * k, WHITE);
        let taken = [i.taken.as_str(), i.age.as_str()]
            .iter()
            .filter(|s| !s.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join("  ");
        t.draw_right(c, &taken, r, y + 66.0 * k, 28.0 * k, SOFT);
        let pos = format!("{} / {}", i.position.0, i.position.1);
        t.draw_right(c, &pos, r, y + 98.0 * k, 22.0 * k, DIM);
    }
}

/// 余白に時刻・日付・写真の情報を重ねる。
fn overlay(
    c: &mut Canvas,
    t: &mut TextEngine,
    s: &Snapshot,
    layout: &PhotoLayout,
    info: Option<&PhotoInfo>,
) {
    let k = scale(c.height());
    let (dx, dy) = drift(&s.time);
    let off = (dx as f32 * k, dy as f32 * k);
    match (layout.mode, layout.info2) {
        (Mode::Cover, _) => cover_band(c, t, s, info, layout.info),
        (Mode::Center, Some(right)) => {
            let pad = 32.0 * k;
            let mut items = clock_items(t, s, layout.info.w as f32 - 2.0 * pad, k);
            draw_block(c, t, layout.info, &items, off, k);
            if let Some(i) = info {
                items = info_items(t, i, right.w as f32 - 2.0 * pad, k, true);
                draw_block(c, t, right, &items, off, k);
            }
        }
        _ => {
            let avail = layout.info.w as f32 - 2.0 * 48.0 * k;
            let mut items = clock_items(t, s, avail, k);
            if let Some(i) = info {
                items.push(Item::Gap(36.0 * k));
                items.push(Item::Rule);
                items.push(Item::Gap(28.0 * k));
                items.extend(info_items(t, i, avail, k, false));
            }
            draw_block(c, t, layout.info, &items, off, k);
        }
    }
}

/// 画面2: ぼかし背景つきの写真 + 時刻・日付・写真の情報。
/// `photo` は (完成フレーム, 配置)。`fade` は切替中の (旧フレーム, 新フレームの不透明度)。
/// `notice` は写真の取得状態の案内(例: 再接続中)。写真が 1 枚もないときは中央に大きく出す。
pub fn photo_frame(
    c: &mut Canvas,
    t: &mut TextEngine,
    s: &Snapshot,
    photo: Option<(&Pixmap, &PhotoLayout)>,
    info: Option<&PhotoInfo>,
    fade: Option<(&Pixmap, f32)>,
    notice: Option<&str>,
) {
    c.clear(rgba(0, 0, 0, 255));
    let k = scale(c.height());
    let Some((frame, layout)) = photo else {
        let msg = notice.unwrap_or("写真を読み込み中…");
        let (w, h) = (c.width() as f32, c.height() as f32);
        let mw = t.measure(msg, 36.0 * k);
        t.draw(c, msg, (w - mw) / 2.0, h / 2.0 - 20.0 * k, 36.0 * k, DIM);
        return;
    };
    match fade {
        Some((old, alpha)) => {
            c.draw_pixmap(old, 0, 0);
            c.draw_pixmap_opacity(frame, 0, 0, alpha);
        }
        None => c.draw_pixmap(frame, 0, 0),
    }
    overlay(c, t, s, layout, info);
    if let Some(n) = notice {
        // 最後の写真を表示したまま、右上に小さく状態を出す。
        let w = c.width() as f32;
        let tw = t.measure(n, 24.0 * k);
        c.fill_rect(
            w - tw - 56.0 * k,
            24.0 * k,
            tw + 32.0 * k,
            44.0 * k,
            rgba(0, 0, 0, 150),
        );
        t.draw(
            c,
            n,
            w - tw - 40.0 * k,
            32.0 * k,
            24.0 * k,
            [255, 200, 120, 255],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drift_stays_within_range_and_changes_every_minute() {
        let mut prev = None;
        for m in 0..24 * 60 {
            let d = drift(&format!("{:02}:{:02}:30", m / 60, m % 60));
            assert!((-8..=8).contains(&d.0) && (-8..=8).contains(&d.1), "{d:?}");
            assert_ne!(Some(d), prev, "{m} 分");
            prev = Some(d);
        }
        assert_eq!(drift("12:34:00"), drift("12:34:59"), "同じ分なら同じ");
        assert_eq!(drift("??"), drift("00:00"), "壊れた時刻でも落ちない");
    }

    #[test]
    fn hhmm_drops_seconds() {
        assert_eq!(hhmm("09:41:07"), "09:41");
        assert_eq!(hhmm("09:41"), "09:41");
        assert_eq!(hhmm(""), "");
    }
}
