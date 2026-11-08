//! だいだい画面(Muse からの通知カードとドット絵のアバター)の描画。デバイスも設定も知らない。
//!
//! 画面は 2 層で描く。変わりにくい層(背景・カード・時計)は `daidai_base` で一度描いて取っておき、
//! 毎フレームは取っておいた絵を戻してから `draw_avatar` でアバターだけを上下にずらして重ねる。
//! アバターは Nearest で拡大するので、ドットが潰れたりにじんだりしない。
use crate::agents::{fit_label_text, wrap_text};
use crate::canvas::{Canvas, rgba};
use crate::photo_layout::scale;
use crate::text::TextEngine;
use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};

/// アバター素材の一辺(画素)。実ファイルもこの大きさ(正方形)を想定するが、違っても縦横を保って収める。
pub const AVATAR_SIZE: u32 = 480;
/// 一度に並べるカードの上限(これを超えたぶんは「ほか N 件」)。
pub const MAX_CARDS: usize = 4;
/// アバターが上下に揺れる周期 [秒]。
pub const BOB_PERIOD_SECS: f32 = 3.2;
/// 揺れの振れ幅(高さ 1080 のときの画素数。片側)。
const BOB_AMPLITUDE: f32 = 7.0;

const BG: [u8; 4] = [10, 14, 22, 255];
const PANEL: [u8; 4] = [22, 29, 42, 255];
const TEXT: [u8; 4] = [236, 241, 250, 255];
const SUB: [u8; 4] = [170, 181, 198, 255];
const DIM: [u8; 4] = [112, 124, 142, 255];
/// だいだい色(アバターの後ろの光と、通知の種類が info のとき以外の基調)
const ORANGE: [u8; 4] = [255, 146, 48, 255];

/// アバターのポーズ。素材は 1 ポーズ 1 枚。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Pose {
    #[default]
    Idle,
    Working,
    Milestone,
    Making,
}

impl Pose {
    pub const ALL: [Pose; 4] = [Pose::Idle, Pose::Working, Pose::Milestone, Pose::Making];

    /// フィードの `avatar_pose` の値。知らない値は `Idle`。
    pub fn parse(s: &str) -> Pose {
        match s {
            "working" => Pose::Working,
            "milestone" => Pose::Milestone,
            "making" => Pose::Making,
            _ => Pose::Idle,
        }
    }

    /// フィードの値と素材ファイル名(`daidai-<name>.png`)に使う名前。
    pub fn name(self) -> &'static str {
        match self {
            Pose::Idle => "idle",
            Pose::Working => "working",
            Pose::Milestone => "milestone",
            Pose::Making => "making",
        }
    }
}

/// 通知の種類。知らない値は `Info`。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CardKind {
    Morning,
    Request,
    #[default]
    Info,
}

impl CardKind {
    pub fn parse(s: &str) -> CardKind {
        match s {
            "morning" => CardKind::Morning,
            "request" => CardKind::Request,
            _ => CardKind::Info,
        }
    }

    fn label(self) -> &'static str {
        match self {
            CardKind::Morning => "MORNING",
            CardKind::Request => "REQUEST",
            CardKind::Info => "INFO",
        }
    }

    fn accent(self) -> [u8; 4] {
        match self {
            CardKind::Morning => [255, 198, 66, 255],
            CardKind::Request => [255, 120, 70, 255],
            CardKind::Info => [76, 190, 255, 255],
        }
    }
}

/// 描く通知カード 1 枚。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardView {
    pub kind: CardKind,
    pub title: String,
    pub body: String,
}

/// アバター以外の層に出すもの。同じなら描き直さない。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DaidaiView {
    /// 期限内のカード(上から並べる順)
    pub cards: Vec<CardView>,
    /// "07:12"(分が変わると描き直す)
    pub time: String,
    /// "2026/01/01 (木)"
    pub date: String,
}

/// アバターを置く位置(揺れていないとき)。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AvatarGeom {
    /// 左上(画面の画素。整数に丸める)
    pub x: i32,
    pub y: i32,
    /// 一辺(画素)
    pub size: f32,
}

pub fn avatar_geometry(w: u32, h: u32) -> AvatarGeom {
    let k = scale(h);
    let size = AVATAR_SIZE as f32 * k;
    let x = (100.0 * k).min((w as f32 - size) / 2.0).max(0.0);
    AvatarGeom {
        x: x.round() as i32,
        y: ((h as f32 - size) / 2.0).round() as i32,
        size,
    }
}

/// 表示を始めてから `secs` 秒たったときのアバターの上下のずれ(画素。下が正)。
/// 整数に丸めるので、ドットは常に画素の境目に載る。
pub fn bob_offset(secs: f32, canvas_h: u32) -> i32 {
    let phase = secs / BOB_PERIOD_SECS * std::f32::consts::TAU;
    (phase.sin() * BOB_AMPLITUDE * scale(canvas_h)).round() as i32
}

/// 背景・アバターの後ろの光と影・カード・時計。アバターは含まない。
pub fn daidai_base(c: &mut Canvas, t: &mut TextEngine, v: &DaidaiView) {
    let (w, h) = (c.width() as f32, c.height() as f32);
    let k = scale(c.height());
    c.clear(rgba(BG[0], BG[1], BG[2], BG[3]));
    let g = avatar_geometry(c.width(), c.height());
    let (cx, cy) = (g.x as f32 + g.size / 2.0, g.y as f32 + g.size / 2.0);
    // アバターの後ろの光と足元の影。揺れても動かない
    c.fill_circle(
        cx,
        cy,
        g.size * 0.62,
        rgba(ORANGE[0], ORANGE[1], ORANGE[2], 14),
    );
    c.fill_circle(
        cx,
        cy,
        g.size * 0.48,
        rgba(ORANGE[0], ORANGE[1], ORANGE[2], 16),
    );
    let shadow: Vec<(f32, f32)> = (0..48)
        .map(|i| {
            let a = i as f32 / 48.0 * std::f32::consts::TAU;
            (
                cx + a.cos() * g.size * 0.30,
                g.y as f32 + g.size + 14.0 * k + a.sin() * 16.0 * k,
            )
        })
        .collect();
    c.fill_rings(&[shadow], rgba(0, 0, 0, 110));

    let left = g.x as f32 + g.size + 80.0 * k;
    let right = w - 100.0 * k;
    if v.cards.is_empty() {
        empty_panel(c, t, v, left, right, h, k);
    } else {
        cards_panel(c, t, v, left, right, h, k);
    }
}

fn empty_panel(
    c: &mut Canvas,
    t: &mut TextEngine,
    v: &DaidaiView,
    left: f32,
    right: f32,
    h: f32,
    k: f32,
) {
    // 控えめに: 大きな時計、日付、「通知はありません」だけ
    let mid = (left + right) / 2.0;
    let center = |t: &mut TextEngine, c: &mut Canvas, s: &str, y: f32, size: f32, col: [u8; 4]| {
        let x = mid - t.measure(s, size) / 2.0;
        t.draw(c, s, x, y, size, col);
    };
    let top = h / 2.0 - 190.0 * k;
    center(t, c, &v.time, top, 220.0 * k, DIM);
    center(t, c, &v.date, top + 270.0 * k, 44.0 * k, DIM);
    center(
        t,
        c,
        "通知はありません",
        top + 340.0 * k,
        34.0 * k,
        [84, 95, 112, 255],
    );
}

fn cards_panel(
    c: &mut Canvas,
    t: &mut TextEngine,
    v: &DaidaiView,
    left: f32,
    right: f32,
    h: f32,
    k: f32,
) {
    let width = (right - left).max(1.0);
    let shown = v.cards.len().min(MAX_CARDS);
    let (card_h, gap) = (200.0 * k, 24.0 * k);
    let total = shown as f32 * card_h + (shown as f32 - 1.0) * gap;
    let more = v.cards.len() - shown;
    let top = ((h - total) / 2.0).max(110.0 * k);
    // 右上に日付と時刻
    t.draw_right(
        c,
        &format!("{}  {}", v.date, v.time),
        right,
        38.0 * k,
        38.0 * k,
        DIM,
    );
    let pad = 30.0 * k;
    for (i, card) in v.cards.iter().take(shown).enumerate() {
        let y = top + i as f32 * (card_h + gap);
        let accent = card.kind.accent();
        c.fill_rect(
            left,
            y,
            width,
            card_h,
            rgba(PANEL[0], PANEL[1], PANEL[2], 255),
        );
        c.fill_rect(
            left,
            y,
            8.0 * k,
            card_h,
            rgba(accent[0], accent[1], accent[2], 255),
        );
        let (tx, text_w) = (left + pad + 8.0 * k, width - 2.0 * pad - 8.0 * k);
        t.draw(c, card.kind.label(), tx, y + 16.0 * k, 22.0 * k, accent);
        let title = fit_label_text(t, &card.title, 42.0 * k, text_w);
        t.draw(c, &title, tx, y + 44.0 * k, 42.0 * k, TEXT);
        let body_size = 28.0 * k;
        for (n, line) in wrap_text(t, &card.body, body_size, text_w, 2)
            .iter()
            .enumerate()
        {
            t.draw(
                c,
                line,
                tx,
                y + 108.0 * k + n as f32 * body_size * 1.35,
                body_size,
                SUB,
            );
        }
    }
    if more > 0 {
        let y = top + shown as f32 * (card_h + gap) - gap + 14.0 * k;
        t.draw_right(c, &format!("ほか {more} 件"), right, y, 26.0 * k, DIM);
    }
}

/// アバターを `bob`(画素。下が正)だけずらして重ねる。Nearest で拡大するので、ドットの縁はにじまない。
pub fn draw_avatar(c: &mut Canvas, avatar: &Pixmap, bob: i32) {
    let g = avatar_geometry(c.width(), c.height());
    // 縦横を保って g.size に収める(素材が正方形でなくても潰さない)
    let s = g.size / avatar.width().max(avatar.height()).max(1) as f32;
    let (dw, dh) = (avatar.width() as f32 * s, avatar.height() as f32 * s);
    let x = g.x as f32 + ((g.size - dw) / 2.0).round();
    let y = g.y as f32 + ((g.size - dh) / 2.0).round() + bob as f32;
    let paint = PixmapPaint {
        quality: FilterQuality::Nearest,
        ..PixmapPaint::default()
    };
    c.pixmap.draw_pixmap(
        0,
        0,
        avatar.as_ref(),
        &paint,
        Transform::from_row(s, 0.0, 0.0, s, x, y),
        None,
    );
}

/// 1 フレーム分をまとめて描く(PNG と試験用。実機の画面は `daidai_base` を取っておく)。
pub fn daidai_screen(
    c: &mut Canvas,
    t: &mut TextEngine,
    v: &DaidaiView,
    avatar: &Pixmap,
    bob: i32,
) {
    daidai_base(c, t, v);
    draw_avatar(c, avatar, bob);
}

/// 素材 PNG を読む。壊れている・大きすぎるものは `Err`。
pub fn decode_avatar(png: &[u8]) -> Result<Pixmap, String> {
    let p = Pixmap::decode_png(png).map_err(|e| e.to_string())?;
    if p.width() == 0 || p.height() == 0 || p.width() > 4096 || p.height() > 4096 {
        return Err(format!(
            "素材の大きさが範囲外: {}x{}",
            p.width(),
            p.height()
        ));
    }
    Ok(p)
}

/// リポジトリに入れてあるプレースホルダー素材(`assets/daidai/daidai-<pose>.png`)。実ファイルへは同じ名前で差し替える。
pub fn embedded_avatar_png(pose: Pose) -> &'static [u8] {
    match pose {
        Pose::Idle => include_bytes!("../assets/daidai/daidai-idle.png"),
        Pose::Working => include_bytes!("../assets/daidai/daidai-working.png"),
        Pose::Milestone => include_bytes!("../assets/daidai/daidai-milestone.png"),
        Pose::Making => include_bytes!("../assets/daidai/daidai-making.png"),
    }
}

/// 見本(`png` の fixture 用)。
pub fn demo_view() -> DaidaiView {
    DaidaiView {
        cards: vec![
            CardView {
                kind: CardKind::Morning,
                title: "おはようございます".into(),
                body: "これはPoCのサンプル表示です。実際の朝の通知がここに出ます。".into(),
            },
            CardView {
                kind: CardKind::Request,
                title: "お願い".into(),
                body: "確認してほしいことがあるときに、だいだいからここで知らせます(サンプル)。"
                    .into(),
            },
        ],
        time: "12:00".into(),
        date: "2026/01/01 (木)".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn avatar() -> Pixmap {
        decode_avatar(embedded_avatar_png(Pose::Idle)).expect("埋め込み素材は読める")
    }

    #[test]
    fn unknown_pose_and_kind_fall_back() {
        assert_eq!(Pose::parse("working"), Pose::Working);
        assert_eq!(Pose::parse("milestone"), Pose::Milestone);
        assert_eq!(Pose::parse("making"), Pose::Making);
        assert_eq!(Pose::parse("dancing"), Pose::Idle);
        assert_eq!(Pose::parse(""), Pose::Idle);
        assert_eq!(CardKind::parse("request"), CardKind::Request);
        assert_eq!(CardKind::parse("???"), CardKind::Info);
    }

    #[test]
    fn every_embedded_pose_decodes() {
        for p in Pose::ALL {
            let a = decode_avatar(embedded_avatar_png(p)).unwrap_or_else(|e| panic!("{p:?}: {e}"));
            assert_eq!((a.width(), a.height()), (AVATAR_SIZE, AVATAR_SIZE));
            assert_eq!(Pose::parse(p.name()), p);
        }
    }

    #[test]
    fn bob_is_small_integer_and_swings_both_ways() {
        let offs: Vec<i32> = (0..64)
            .map(|i| bob_offset(i as f32 * BOB_PERIOD_SECS / 64.0, 1080))
            .collect();
        assert_eq!(offs[0], 0);
        assert!(offs.iter().all(|o| o.abs() <= 7));
        assert!(offs.iter().any(|&o| o > 0) && offs.iter().any(|&o| o < 0));
        // 小さい画面では振れ幅も縮む
        assert!(bob_offset(BOB_PERIOD_SECS / 4.0, 540) <= 4);
    }

    #[test]
    fn avatar_is_drawn_with_nearest_so_dots_stay_crisp() {
        // 2x2 の市松模様を 10 倍に拡大: 境目以外の画素は元の 4 色だけで、混ざった色は出ない
        let mut dots = Pixmap::new(2, 2).unwrap();
        let cols = [
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
            [255, 255, 0, 255],
        ];
        for (i, c) in cols.iter().enumerate() {
            dots.data_mut()[i * 4..i * 4 + 4].copy_from_slice(c);
        }
        let mut c = Canvas::new(1920, 1080);
        c.clear(rgba(BG[0], BG[1], BG[2], 255));
        draw_avatar(&mut c, &dots, 0);
        let px = c.pixmap.data();
        let mut seen = std::collections::HashSet::new();
        for p in px.as_chunks::<4>().0 {
            seen.insert(*p);
        }
        // 背景 + 4 色だけ(補間されていれば中間色が増える)
        assert_eq!(seen.len(), 5, "{seen:?}");
    }

    #[test]
    fn avatar_moves_with_the_bob_offset() {
        let a = avatar();
        let (mut c0, mut c1) = (Canvas::new(960, 540), Canvas::new(960, 540));
        draw_avatar(&mut c0, &a, 0);
        draw_avatar(&mut c1, &a, 4);
        assert_ne!(c0.pixmap.data(), c1.pixmap.data());
    }

    #[test]
    fn cards_and_empty_states_render_without_panicking() {
        let mut t = TextEngine::new("Noto Sans CJK JP");
        let a = avatar();
        let mut c = Canvas::new(1280, 720);
        let mut many = demo_view();
        for i in 0..7 {
            many.cards.push(CardView {
                kind: CardKind::Info,
                title: format!("とても長いタイトル{i}").repeat(8),
                body: "本文".repeat(200),
            });
        }
        let empty = DaidaiView {
            time: "07:12".into(),
            date: "2026/01/01 (木)".into(),
            ..DaidaiView::default()
        };
        for v in [demo_view(), many, empty, DaidaiView::default()] {
            daidai_screen(&mut c, &mut t, &v, &a, 3);
            assert_eq!(&c.pixmap.data()[..4], &[BG[0], BG[1], BG[2], 255]);
        }
    }

    #[test]
    fn card_view_differs_from_empty_view() {
        let mut t = TextEngine::new("Noto Sans CJK JP");
        let (mut a, mut b) = (Canvas::new(960, 540), Canvas::new(960, 540));
        daidai_base(&mut a, &mut t, &demo_view());
        daidai_base(&mut b, &mut t, &DaidaiView::default());
        assert_ne!(a.pixmap.data(), b.pixmap.data());
    }
}
