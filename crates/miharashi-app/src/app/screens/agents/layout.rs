//! プロジェクトごとの枠にカードを詰める配置計算。描画や時刻には依存しない。
//!
//! ボードは 1 枚だけ。プロジェクトは横に並ぶ枠になり、件数の多い枠には列を足す。
//! 列が足りないときは 1〜数件の枠を 1 列に縦に積み、それでも入らない列ではカードを重ねる。
pub mod family;

use std::collections::HashMap;

// 1920x1080 を 1.0 とした寸法。
const CARD_H: f32 = 210.0;
const STRIP: f32 = 58.0;
const STRIP_MIN: f32 = 40.0;
const MORE_H: f32 = 36.0;
const GAP: f32 = 16.0;
const LANE_GAP: f32 = 32.0;
const LANE_PAD: f32 = 12.0;
const LANE_HEAD: f32 = 56.0;
const LANE_FOOT: f32 = 8.0;
const MIN_W: f32 = 380.0;
const MAX_W: f32 = 760.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardMode {
    Full,
    /// 他のカードの後ろにあり、上端の 1 行だけが見える。
    Strip,
    /// 親子グループの静かな子。1 行だけの低いカード(状態の点・題名・経過時間)。
    Row,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CardSlot {
    pub rect: Rect,
    pub mode: CardMode,
    /// 同じ列の中での重なり順。大きいほど手前。
    pub z: u32,
    /// 親子グループの中での字下げの段(親 0、子 1、孫以降 2)。
    pub depth: u8,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Lane {
    /// `layout` に渡したグループの位置。
    pub group: usize,
    pub rect: Rect,
    /// 見出し(プロジェクト名と状態の点)の左上と幅。
    pub head: Rect,
    /// 入りきらずに隠したカード。左上と件数。
    pub more: Vec<(f32, f32, usize)>,
    /// 親子グループの配置で、同じプロジェクトが次の列へ続いた 2 つ目以降の枠。
    pub cont: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    pub lanes: Vec<Lane>,
    pub cards: HashMap<String, CardSlot>,
    /// 親子グループの枠(`family::plan_board` が作る。単独スレッドだけの配置では空)。
    pub families: Vec<family::FamilyShape>,
}

impl Layout {
    /// カードの置き場。カードが無い(「ほか N 件」にまとめた)スレッドは、親のカード、
    /// 親も無ければグループの見出しの位置を返す。ポップアップの元位置と戻り先に使う。
    pub fn anchor(&self, id: &str) -> Option<Rect> {
        if let Some(card) = self.cards.get(id) {
            return Some(card.rect);
        }
        let shape = self
            .families
            .iter()
            .find(|f| f.hidden_ids.iter().any(|h| h == id))?;
        shape
            .root
            .as_deref()
            .and_then(|root| self.cards.get(root))
            .map(|c| c.rect)
            .or(Some(shape.head))
    }
}

/// `groups` は表示順に並んだプロジェクトごとのスレッド ID。各グループの中は重要な順。
pub fn layout(area: Rect, scale: f32, groups: &[Vec<String>]) -> Layout {
    let mut out = Layout::default();
    let groups = groups
        .iter()
        .enumerate()
        .filter(|(_, g)| !g.is_empty())
        .collect::<Vec<_>>();
    if groups.is_empty() || area.w <= 0.0 || area.h <= 0.0 {
        return out;
    }
    let s = scale;
    let (gap, lane_gap, head) = (GAP * s, LANE_GAP * s, LANE_HEAD * s);
    let card_h = CARD_H * s;
    let col_h = area.h;
    let max_cols = (((area.w + lane_gap) / (MIN_W * s + lane_gap)).floor() as usize).max(1);
    let rows_full =
        (((col_h - head - LANE_FOOT * s + gap) / (card_h + gap)).floor() as usize).max(1);
    let block_h =
        |n: usize| head + n as f32 * card_h + n.saturating_sub(1) as f32 * gap + LANE_FOOT * s;

    // 1 単位 = 1 つの枠、または 1 列に縦に積んだ小さい枠の組。
    let mut units: Vec<Vec<usize>> = (0..groups.len()).map(|i| vec![i]).collect();
    let mut cols = vec![1usize; groups.len()];
    let fits_one_column = |i: usize| groups[i].1.len() <= rows_full;
    // 積むのは、全部を重ねずに並べるには列が足りないときだけ。
    let wanted = |units: &[Vec<usize>]| {
        units
            .iter()
            .map(|u| {
                if u.len() == 1 {
                    groups[u[0]].1.len().div_ceil(rows_full)
                } else {
                    1
                }
            })
            .sum::<usize>()
    };
    while wanted(&units) > max_cols {
        let mut best: Option<(f32, usize, usize)> = None;
        for a in 0..units.len() {
            for b in a + 1..units.len() {
                if !units[a]
                    .iter()
                    .chain(&units[b])
                    .all(|&i| fits_one_column(i))
                {
                    continue;
                }
                let members = units[a].len() + units[b].len();
                let h = units[a]
                    .iter()
                    .chain(&units[b])
                    .map(|&i| block_h(groups[i].1.len()))
                    .sum::<f32>()
                    + (members - 1) as f32 * gap;
                if h <= col_h && best.is_none_or(|(bh, _, _)| h < bh) {
                    best = Some((h, a, b));
                }
            }
        }
        let Some((_, a, b)) = best else { break };
        let moved = units.remove(b);
        units[a].extend(moved);
        units[a].sort_unstable();
    }
    units.sort_by_key(|u| u[0]);
    let col_count = |units: &[Vec<usize>], cols: &[usize]| {
        units
            .iter()
            .map(|u| if u.len() == 1 { cols[u[0]] } else { 1 })
            .sum::<usize>()
    };
    // 余った列は、1 列あたりの件数が最も多い枠へ。
    while col_count(&units, &cols) < max_cols {
        let crowded = units
            .iter()
            .filter(|u| u.len() == 1)
            .map(|u| u[0])
            .filter(|&i| groups[i].1.len() > cols[i] * rows_full)
            .max_by(|&a, &b| {
                let ra = groups[a].1.len() as f32 / cols[a] as f32;
                let rb = groups[b].1.len() as f32 / cols[b] as f32;
                ra.total_cmp(&rb).then(b.cmp(&a))
            });
        let Some(i) = crowded else { break };
        cols[i] += 1;
    }

    let ncols = col_count(&units, &cols);
    let inner_gaps = units
        .iter()
        .map(|u| if u.len() == 1 { cols[u[0]] - 1 } else { 0 })
        .sum::<usize>() as f32
        * gap;
    let lane_gaps = (units.len() - 1) as f32 * lane_gap;
    let cw = ((area.w - inner_gaps - lane_gaps) / ncols as f32)
        .min(MAX_W * s)
        .max(1.0);
    let used = ncols as f32 * cw + inner_gaps + lane_gaps;
    let mut x = area.x + (area.w - used) / 2.0;
    let pad = LANE_PAD * s;
    for unit in &units {
        if unit.len() == 1 {
            let i = unit[0];
            let (group, ids) = groups[i];
            let c = cols[i];
            let lw = c as f32 * cw + (c - 1) as f32 * gap;
            let mut lane = Lane {
                group,
                rect: Rect {
                    x: x - pad,
                    y: area.y,
                    w: lw + pad * 2.0,
                    h: col_h,
                },
                head: Rect {
                    x,
                    y: area.y + 10.0 * s,
                    w: lw,
                    h: head - 16.0 * s,
                },
                more: Vec::new(),
                cont: false,
            };
            let per = ids.len().div_ceil(c);
            for (ci, chunk) in ids.chunks(per.max(1)).enumerate() {
                let cx = x + ci as f32 * (cw + gap);
                let top = area.y + head;
                let avail = col_h - head - LANE_FOOT * s;
                if let Some(more) = place_column(&mut out.cards, chunk, cx, top, cw, avail, s, true)
                {
                    lane.more.push((cx, top, more));
                }
            }
            out.lanes.push(lane);
            x += lw + lane_gap;
        } else {
            let mut y = area.y;
            for &i in unit {
                let (group, ids) = groups[i];
                let bh = block_h(ids.len());
                out.lanes.push(Lane {
                    group,
                    rect: Rect {
                        x: x - pad,
                        y,
                        w: cw + pad * 2.0,
                        h: bh,
                    },
                    head: Rect {
                        x,
                        y: y + 10.0 * s,
                        w: cw,
                        h: head - 16.0 * s,
                    },
                    more: Vec::new(),
                    cont: false,
                });
                place_column(
                    &mut out.cards,
                    ids,
                    x,
                    y + head,
                    cw,
                    bh - head - LANE_FOOT * s,
                    s,
                    false,
                );
                y += bh + gap;
            }
            x += cw + lane_gap;
        }
    }
    out.lanes.sort_by_key(|l| l.group);
    out
}

/// 1 列分を配置し、入りきらずに隠した件数を返す。
/// 全部が入る列は重要な順に上から並べ、`fill` なら縦に伸ばして列を埋める。
/// 入らない列は静かなものを上に重ね、重要なものほど下の手前に全体を見せる。
#[allow(clippy::too_many_arguments)]
fn place_column(
    cards: &mut HashMap<String, CardSlot>,
    ids: &[String],
    x: f32,
    top: f32,
    w: f32,
    avail: f32,
    s: f32,
    fill: bool,
) -> Option<usize> {
    let n = ids.len();
    if n == 0 {
        return None;
    }
    let (gap, card_h, strip) = (GAP * s, CARD_H * s, STRIP * s);
    if n as f32 * card_h + (n - 1) as f32 * gap <= avail + 0.5 {
        let h = if fill {
            (avail - (n - 1) as f32 * gap) / n as f32
        } else {
            card_h
        };
        for (i, id) in ids.iter().enumerate() {
            cards.insert(
                id.clone(),
                CardSlot {
                    rect: Rect {
                        x,
                        y: top + i as f32 * (h + gap),
                        w,
                        h,
                    },
                    mode: CardMode::Full,
                    z: 0,
                    depth: 0,
                },
            );
        }
        return None;
    }
    let mut k = 1;
    while k < n && (n - k - 1) as f32 * strip + (k + 1) as f32 * card_h + k as f32 * gap <= avail {
        k += 1;
    }
    let room = avail - k as f32 * card_h - (k - 1) as f32 * gap;
    let mut back = n - k;
    let mut start = top;
    let mut hidden = 0;
    if room / (back as f32) < STRIP_MIN * s {
        let label = (MORE_H * s).min(room.max(0.0));
        let shown = (((room - label) / (STRIP_MIN * s)).floor().max(0.0) as usize).min(back);
        hidden = back - shown;
        back = shown;
        start += label;
    }
    let off = if back == 0 {
        0.0
    } else {
        ((top + room - start) / back as f32).min(card_h + gap)
    };
    // 後ろ: 一番静かなものが一番上。
    for (i, id) in ids[k..k + back].iter().rev().enumerate() {
        cards.insert(
            id.clone(),
            CardSlot {
                rect: Rect {
                    x,
                    y: start + i as f32 * off,
                    w,
                    h: card_h,
                },
                mode: CardMode::Strip,
                z: i as u32,
                depth: 0,
            },
        );
    }
    let mut y = start + back as f32 * off;
    for (j, id) in ids[..k].iter().rev().enumerate() {
        cards.insert(
            id.clone(),
            CardSlot {
                rect: Rect { x, y, w, h: card_h },
                mode: CardMode::Full,
                z: 1000 + j as u32,
                depth: 0,
            },
        );
        y += card_h + gap;
    }
    (hidden > 0).then_some(hidden)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(prefix: &str, n: usize) -> Vec<String> {
        (0..n).map(|i| format!("{prefix}{i}")).collect()
    }
    const AREA: Rect = Rect {
        x: 32.0,
        y: 200.0,
        w: 2496.0,
        h: 800.0,
    };

    #[test]
    fn few_threads_fill_their_columns_without_overlap() {
        let l = layout(AREA, 1.0, &[ids("a", 5), ids("b", 1), ids("c", 1)]);
        assert_eq!(l.cards.len(), 7);
        assert!(l.cards.values().all(|c| c.mode == CardMode::Full));
        // 5 件の枠は 2 列になり、1 件の枠のカードは縦に伸びる。
        let xs = (0..5)
            .map(|i| l.cards[&format!("a{i}")].rect.x as i32)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(xs.len(), 2);
        assert!(l.cards["b0"].rect.h > CARD_H * 2.0);
        // 重要な順に上から。
        assert!(l.cards["a0"].rect.y < l.cards["a1"].rect.y);
    }

    #[test]
    fn many_small_projects_share_columns_and_large_ones_overlap() {
        let groups = [
            ids("a", 9),
            ids("b", 4),
            ids("c", 3),
            ids("d", 2),
            ids("e", 1),
            ids("f", 1),
        ];
        let l = layout(AREA, 1.0, &groups);
        assert_eq!(l.cards.len(), 20);
        assert_eq!(l.lanes.len(), 6);
        // e と f は 1 列に縦に積む。
        assert_eq!(l.cards["e0"].rect.x, l.cards["f0"].rect.x);
        assert!(l.cards["e0"].rect.y < l.cards["f0"].rect.y);
        // 最重要のカードは全体が見え、静かなカードは後ろに重なる。
        assert_eq!(l.cards["a0"].mode, CardMode::Full);
        assert!(l.cards.values().any(|c| c.mode == CardMode::Strip));
        for card in l.cards.values() {
            assert!(card.rect.x >= AREA.x - 0.5);
            assert!(card.rect.x + card.rect.w <= AREA.x + AREA.w + 0.5);
            assert!(card.rect.y + card.rect.h <= AREA.y + AREA.h + 0.5);
        }
    }

    #[test]
    fn a_crowded_column_hides_the_quietest_behind_a_count() {
        let l = layout(Rect { w: 400.0, ..AREA }, 1.0, &[ids("a", 40)]);
        let more = l.lanes[0].more.iter().map(|m| m.2).sum::<usize>();
        assert!(more > 0);
        assert_eq!(l.cards.len() + more, 40);
        assert!(l.cards.contains_key("a0"));
        assert!(!l.cards.contains_key("a39"));
    }

    #[test]
    fn lanes_keep_the_given_order() {
        let l = layout(AREA, 1.0, &[ids("a", 1), ids("b", 6), ids("c", 1)]);
        assert!(l.cards["a0"].rect.x < l.cards["b0"].rect.x);
        assert!(l.cards["b0"].rect.x < l.cards["c0"].rect.x);
        assert_eq!(
            l.lanes.iter().map(|l| l.group).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }
}
