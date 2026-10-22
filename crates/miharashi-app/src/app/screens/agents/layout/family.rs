//! 親子グループ(subagent の親子)の組み立てと配置。描画にも時刻にも依存しない純粋な計算。
//!
//! 流れ: `build_families`(スレッドを親子のまとまりにする)→ `plan_board`(プロジェクトの枠に詰める)。
//! 親子グループが 1 つも無いときは、従来の単独カードの配置(`layout::layout`)にそのまま任せる。
//!
//! 詰め方(列は上から下へ流す):
//! 1. 親子グループは 1 列に収まる範囲で、静かな子(応答完了・その他)を 1 行表示に縮める。
//! 2. それでも 1 列を超える組は、次の列に「…の子・続き」として続ける。
//! 3. 列が足りなければ、静かな子を「ほか N 件」に集約する(要確認の子は集約しない)。
//! 4. それでも入らなければ、親子グループ単位でページに分ける(要確認を持つ組が最初のページ)。
use super::{
    CARD_H, CardMode, CardSlot, GAP, LANE_FOOT, LANE_GAP, LANE_HEAD, LANE_PAD, Lane, Layout, MAX_W,
    MIN_W, Rect, layout,
};
use std::collections::HashMap;

/// 字下げの上限(子 = 1、孫以降 = 2)。
pub const MAX_DEPTH: u8 = 2;
const FAM_PAD: f32 = 8.0;
const FAM_HEAD: f32 = 28.0;
const CHILD_H: f32 = 144.0;
const ROW_H: f32 = 38.0;
const MORE_LINE: f32 = 32.0;
const CHILD_GAP: f32 = 6.0;
pub const INDENT: f32 = 24.0;
/// 接続線の縦線が、親カードの左端から入る距離。
pub const LINK_INSET: f32 = 12.0;
pub const LINK_W: f32 = 2.0;

/// 重要さの段。`AgentScreen::rank` の段(要確認 → 実行中 → その他)と同じ。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    Attention,
    Running,
    Quiet,
}

/// `build_families` への入力。`nodes` は重要な順(先頭が最重要)に並べて渡す。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub id: String,
    /// subagent の親スレッド ID(fork や親なしは None)。
    pub parent: Option<String>,
    pub tier: Tier,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub id: String,
    /// 字下げの段。親 0、子 1、孫以降 2。
    pub depth: u8,
    pub tier: Tier,
    /// `members` の中の親の位置。親カードが無い(親情報なし)子は None。
    pub parent: Option<usize>,
    /// `build_families` に渡した `nodes` での位置。小さいほど重要。
    pub rank: usize,
    /// 全員が静かな部分木(1 行表示・集約の単位)の番号。要確認や実行中を含む子は None。
    pub unit: Option<usize>,
}

/// 親を先頭にした 1 組。単独スレッドは 1 人だけの組(枠も線も付けない)。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Family {
    /// 親を先頭に、子を重要な順に、孫は親の子の直後に並べた順。
    pub members: Vec<Member>,
    /// 親が一覧に無い子の組。値は親の ID(`members` に親のカードは無い)。
    pub orphan_of: Option<String>,
    /// プロジェクトの枠を決めるスレッドの `nodes` での位置(親、親情報なしなら最も重要な子)。
    pub lead: usize,
    /// 1 行表示・集約の単位の数。
    pub units: usize,
}

impl Family {
    /// 枠と接続線を付ける組か。
    pub fn framed(&self) -> bool {
        self.members.len() > 1 || self.orphan_of.is_some()
    }
    pub fn has_attention(&self) -> bool {
        self.members.iter().any(|m| m.tier == Tier::Attention)
    }
    /// 組の中で最も重要なスレッドの位置。
    pub fn best(&self) -> usize {
        self.members
            .iter()
            .map(|m| m.rank)
            .min()
            .unwrap_or(usize::MAX)
    }
    /// 親のカードの ID(親情報なしの組は None)。
    pub fn root_id(&self) -> Option<&str> {
        match (&self.orphan_of, self.members.first()) {
            (None, Some(m)) if m.depth == 0 => Some(&m.id),
            _ => None,
        }
    }
}

/// 可視のスレッドを親子の組にする。組は最も重要なメンバーの順。
/// 親が可視のスレッドの子は親と同じ組に入り(親の親も同様にたどる)、親が可視でない子は親ごとに
/// 「親情報なし」の組にまとめる。親をたどって輪になっているときは、最も重要なスレッドを根にして輪を切る。
pub fn build_families(nodes: &[Node]) -> Vec<Family> {
    let n = nodes.len();
    let index: HashMap<&str, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, node)| (node.id.as_str(), i))
        .collect();
    let mut parent: Vec<Option<usize>> = nodes
        .iter()
        .enumerate()
        .map(|(i, node)| {
            node.parent
                .as_deref()
                .and_then(|p| index.get(p).copied())
                .filter(|&p| p != i)
        })
        .collect();
    for start in 0..n {
        let mut cur = parent[start];
        let mut steps = 0;
        while let Some(c) = cur {
            if c == start {
                // 輪の中でいちばん重要なものの親を切る。
                let mut cycle = vec![start];
                let mut x = parent[start];
                while let Some(v) = x.filter(|&v| v != start) {
                    cycle.push(v);
                    x = parent[v];
                }
                if let Some(&top) = cycle.iter().min() {
                    parent[top] = None;
                }
                break;
            }
            steps += 1;
            if steps > n {
                break;
            }
            cur = parent[c];
        }
    }
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, p) in parent.iter().enumerate() {
        if let Some(p) = p {
            children[*p].push(i);
        }
    }
    let orphan_of = |i: usize| -> Option<&str> {
        if parent[i].is_some() {
            return None;
        }
        nodes[i]
            .parent
            .as_deref()
            .filter(|p| *p != nodes[i].id && !index.contains_key(p))
    };

    let mut families = Vec::new();
    let mut orphans: Vec<(String, Vec<usize>)> = Vec::new();
    for (i, _) in parent.iter().enumerate().filter(|(_, p)| p.is_none()) {
        match orphan_of(i) {
            Some(p) => match orphans.iter_mut().find(|(id, _)| id == p) {
                Some((_, v)) => v.push(i),
                None => orphans.push((p.to_string(), vec![i])),
            },
            None => families.push(make_family(nodes, &children, &[i], 0, None)),
        }
    }
    for (p, roots) in orphans {
        families.push(make_family(nodes, &children, &roots, 1, Some(p)));
    }
    families.sort_by_key(|f| (f.best(), f.lead));
    families
}

fn make_family(
    nodes: &[Node],
    children: &[Vec<usize>],
    roots: &[usize],
    first_depth: u8,
    orphan_of: Option<String>,
) -> Family {
    let mut members: Vec<Member> = Vec::new();
    // 深さ優先。子は重要な順(nodes の順)に並んでいる。
    let mut stack: Vec<(usize, u8, Option<usize>)> = roots
        .iter()
        .rev()
        .map(|&r| (r, first_depth, None))
        .collect();
    while let Some((i, depth, parent)) = stack.pop() {
        let at = members.len();
        members.push(Member {
            id: nodes[i].id.clone(),
            depth: depth.min(MAX_DEPTH),
            tier: nodes[i].tier,
            parent,
            rank: i,
            unit: None,
        });
        for &c in children[i].iter().rev() {
            stack.push((c, depth.saturating_add(1), Some(at)));
        }
    }
    // 静かな部分木(自分と子孫が全員 Quiet)。親(深さ 0)は縮めも隠しもしない。
    let mut quiet: Vec<bool> = members.iter().map(|m| m.tier == Tier::Quiet).collect();
    for m in (0..members.len()).rev() {
        if !quiet[m]
            && let Some(p) = members[m].parent
        {
            quiet[p] = false;
        }
    }
    // 単位の番号。親が単位に入っていない静かな子から新しい単位を始め、その子孫は同じ単位に入る。
    let mut units = 0;
    for m in 0..members.len() {
        if !quiet[m] || (members[m].depth == 0 && orphan_of.is_none()) {
            continue;
        }
        let inherited = members[m].parent.and_then(|p| members[p].unit);
        members[m].unit = inherited.or_else(|| {
            units += 1;
            Some(units - 1)
        });
    }
    let lead = if orphan_of.is_some() {
        members.iter().map(|m| m.rank).min().unwrap_or(0)
    } else {
        members.first().map_or(0, |m| m.rank)
    };
    Family {
        members,
        orphan_of,
        lead,
        units,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnitState {
    Card,
    Row,
    Hidden,
}

#[derive(Clone, Copy, Debug)]
enum Entry {
    Member { m: usize, row: bool },
    More(usize),
}

struct Ctx {
    s: f32,
    area: Rect,
    max_cols: usize,
}

impl Ctx {
    fn gap(&self) -> f32 {
        GAP * self.s
    }
    fn head(&self) -> f32 {
        LANE_HEAD * self.s
    }
    fn foot(&self) -> f32 {
        LANE_FOOT * self.s
    }
    fn entry_h(&self, f: &Family, e: &Entry) -> f32 {
        self.s
            * match e {
                Entry::More(_) => MORE_LINE,
                Entry::Member { row: true, .. } => ROW_H,
                Entry::Member { m, .. } if f.members[*m].depth == 0 => CARD_H,
                Entry::Member { .. } => CHILD_H,
            }
    }
    fn piece_h(&self, f: &Family, framed: bool, entries: &[Entry]) -> f32 {
        let sum: f32 = entries.iter().map(|e| self.entry_h(f, e)).sum();
        if !framed {
            return sum;
        }
        let gaps = entries.len().saturating_sub(1) as f32 * CHILD_GAP * self.s;
        sum + gaps + (FAM_PAD * 2.0 + FAM_HEAD) * self.s
    }
}

/// 組の中身を、単位ごとの表示(カード・1 行・隠す)に従って並べる。
fn entries_of(f: &Family, states: &[UnitState]) -> Vec<Entry> {
    let mut out = Vec::with_capacity(f.members.len() + 1);
    let mut hidden = 0;
    for (m, mem) in f.members.iter().enumerate() {
        match mem.unit.map(|u| states[u]) {
            Some(UnitState::Hidden) => hidden += 1,
            Some(UnitState::Row) => out.push(Entry::Member { m, row: true }),
            _ => out.push(Entry::Member { m, row: false }),
        }
    }
    if hidden > 0 {
        out.push(Entry::More(hidden));
    }
    out
}

struct Piece {
    fam: usize,
    entries: Vec<Entry>,
    framed: bool,
    continued: bool,
    y: f32,
    h: f32,
}

struct Seg {
    project: usize,
    cont: bool,
    col: usize,
    top: f32,
    bottom: f32,
    pieces: Vec<Piece>,
}

struct Packed {
    segs: Vec<Seg>,
    cols: usize,
}

struct Packer<'a> {
    c: &'a Ctx,
    fams: &'a [PageFam<'a>],
    segs: Vec<Seg>,
    col: usize,
    y: f32,
    open: Option<usize>,
}

impl<'a> Packer<'a> {
    fn begin(&mut self, project: usize) {
        let cont = self.segs.iter().any(|s| s.project == project);
        self.segs.push(Seg {
            project,
            cont,
            col: self.col,
            top: self.y,
            bottom: self.y,
            pieces: Vec::new(),
        });
        self.open = Some(self.segs.len() - 1);
        self.y += self.c.head();
    }

    fn end_seg(&mut self) {
        let Some(i) = self.open.take() else { return };
        if self.segs[i].pieces.is_empty() {
            self.y = self.segs[i].top;
            self.segs.pop();
        } else {
            let bottom = self.y - self.c.gap() + self.c.foot();
            self.segs[i].bottom = bottom;
            self.y = bottom + self.c.gap();
        }
    }

    fn next_col(&mut self, project: usize) {
        self.end_seg();
        self.col += 1;
        self.y = 0.0;
        self.begin(project);
    }

    fn push_piece(&mut self, fam: usize, entries: Vec<Entry>, framed: bool, continued: bool) {
        let f = self.fams[fam].f;
        let h = self.c.piece_h(f, framed, &entries);
        let i = self.open.expect("open segment");
        self.segs[i].pieces.push(Piece {
            fam,
            entries,
            framed,
            continued,
            y: self.y,
            h,
        });
        self.y += h + self.c.gap();
    }

    fn place(&mut self, project: usize, fam: usize) {
        let f = self.fams[fam].f;
        let framed = f.framed();
        let mut entries = entries_of(f, &self.fams[fam].states);
        let mut continued = false;
        loop {
            let i = self.open.expect("open segment");
            let remaining = self.c.area.h - self.y - self.c.foot();
            let whole = self.c.piece_h(f, framed, &entries);
            if whole <= remaining + 0.5 {
                self.push_piece(fam, entries, framed, continued);
                return;
            }
            let can_gain = !self.segs[i].pieces.is_empty() || self.segs[i].top > 0.0;
            let fresh = self.c.area.h - self.c.head() - self.c.foot();
            if can_gain && (whole <= fresh + 0.5 || !framed) {
                self.next_col(project);
                continue;
            }
            if !framed {
                self.push_piece(fam, entries, framed, continued);
                return;
            }
            let kmin = (if continued || f.orphan_of.is_some() {
                1
            } else {
                2
            })
            .min(entries.len());
            let k = (kmin..=entries.len())
                .rev()
                .find(|&k| self.c.piece_h(f, true, &entries[..k]) <= remaining + 0.5);
            let k = match k {
                Some(k) => k,
                None if can_gain => {
                    self.next_col(project);
                    continue;
                }
                None => kmin,
            };
            let rest = entries.split_off(k);
            self.push_piece(fam, entries, true, continued);
            continued = true;
            entries = rest;
            if entries.is_empty() {
                return;
            }
            self.next_col(project);
        }
    }
}

/// 1 ページに載せる組。`states` は単位ごとの表示。
struct PageFam<'a> {
    f: &'a Family,
    project: usize,
    idx: usize,
    states: Vec<UnitState>,
}

fn pack(c: &Ctx, fams: &[PageFam<'_>], fresh_columns: bool) -> Packed {
    let mut p = Packer {
        c,
        fams,
        segs: Vec::new(),
        col: 0,
        y: 0.0,
        open: None,
    };
    let mut projects: Vec<usize> = fams.iter().map(|f| f.project).collect();
    projects.dedup();
    for project in projects {
        if fresh_columns && p.y > 0.0 {
            p.col += 1;
            p.y = 0.0;
        }
        p.begin(project);
        for fi in (0..fams.len()).filter(|&fi| fams[fi].project == project) {
            p.place(project, fi);
        }
        p.end_seg();
    }
    p.end_seg();
    let cols = p.segs.iter().map(|s| s.col + 1).max().unwrap_or(0);
    Packed { segs: p.segs, cols }
}

/// 1 列に収まるまで、静かな単位を後ろから 1 行表示にする。
fn natural_states(c: &Ctx, f: &Family) -> Vec<UnitState> {
    let mut states = vec![UnitState::Card; f.units];
    let avail = c.area.h - c.head() - c.foot();
    while c.piece_h(f, f.framed(), &entries_of(f, &states)) > avail {
        let Some(u) = states.iter().rposition(|s| *s == UnitState::Card) else {
            break;
        };
        states[u] = UnitState::Row;
    }
    states
}

/// 1 ページ分(`items` は (プロジェクト, そのプロジェクトの組の位置))を詰める。
/// 入らなければ、全部を 1 行表示 → 静かな単位を後ろから 1 つずつ隠す、の順に縮める。
fn build_page(
    c: &Ctx,
    projects: &[Vec<Family>],
    items: &[(usize, usize)],
    force: bool,
) -> Option<Layout> {
    let mut items = items.to_vec();
    items.sort_unstable();
    let mut fams: Vec<PageFam<'_>> = items
        .iter()
        .map(|&(project, idx)| {
            let f = &projects[project][idx];
            PageFam {
                f,
                project,
                idx,
                states: natural_states(c, f),
            }
        })
        .collect();
    let mut all_rows = false;
    loop {
        for fresh in [true, false] {
            let packed = pack(c, &fams, fresh);
            if packed.cols <= c.max_cols {
                return Some(finish(c, &fams, packed));
            }
        }
        if !all_rows {
            all_rows = true;
            let mut changed = false;
            for fam in &mut fams {
                for s in &mut fam.states {
                    if *s == UnitState::Card {
                        *s = UnitState::Row;
                        changed = true;
                    }
                }
            }
            if changed {
                continue;
            }
        }
        // 隠せる静かな単位が最も多い組から、いちばん静かな 1 つを隠す。
        let target = fams
            .iter()
            .enumerate()
            .filter_map(|(i, fam)| {
                let left = fam
                    .states
                    .iter()
                    .filter(|s| **s != UnitState::Hidden)
                    .count();
                (left > 0).then_some((left, i))
            })
            .max();
        match target {
            Some((_, i)) => {
                if let Some(u) = fams[i].states.iter().rposition(|s| *s != UnitState::Hidden) {
                    fams[i].states[u] = UnitState::Hidden;
                }
            }
            None => {
                if !force {
                    return None;
                }
                // どうしても入らない(要確認が何十件もある)ときは、右端の外へはみ出して並べる。
                let packed = pack(c, &fams, false);
                return Some(finish(c, &fams, packed));
            }
        }
    }
}

/// 詰めた結果を座標にする。
fn finish(c: &Ctx, fams: &[PageFam<'_>], packed: Packed) -> Layout {
    let s = c.s;
    let mut out = Layout::default();
    let cols = packed.cols.clamp(1, c.max_cols);
    let lane_gap = LANE_GAP * s;
    let cw = ((c.area.w - (cols - 1) as f32 * lane_gap) / cols as f32)
        .min(MAX_W * s)
        .max(1.0);
    let used = cols as f32 * cw + (cols - 1) as f32 * lane_gap;
    let x0 = c.area.x + (c.area.w - used) / 2.0;
    let pad = LANE_PAD * s;
    let (fpad, fhead) = (FAM_PAD * s, FAM_HEAD * s);
    for (si, seg) in packed.segs.iter().enumerate() {
        let x = x0 + seg.col as f32 * (cw + lane_gap);
        let last_in_col = !packed.segs[si + 1..].iter().any(|o| o.col == seg.col);
        let h = if last_in_col {
            c.area.h - seg.top
        } else {
            seg.bottom - seg.top
        };
        out.lanes.push(Lane {
            group: seg.project,
            rect: Rect {
                x: x - pad,
                y: c.area.y + seg.top,
                w: cw + pad * 2.0,
                h,
            },
            head: Rect {
                x,
                y: c.area.y + seg.top + 10.0 * s,
                w: cw,
                h: c.head() - 16.0 * s,
            },
            more: Vec::new(),
            cont: seg.cont,
        });
        for piece in &seg.pieces {
            let fam = &fams[piece.fam];
            let f = fam.f;
            let top = c.area.y + piece.y;
            if !piece.framed {
                let Entry::Member { m, .. } = piece.entries[0] else {
                    continue;
                };
                out.cards.insert(
                    f.members[m].id.clone(),
                    CardSlot {
                        rect: Rect {
                            x,
                            y: top,
                            w: cw,
                            h: piece.h,
                        },
                        mode: CardMode::Full,
                        z: 0,
                        depth: 0,
                    },
                );
                continue;
            }
            let mut shape = FamilyShape {
                project: fam.project,
                family: fam.idx,
                continued: piece.continued,
                orphan: f.orphan_of.is_some(),
                root: f.root_id().map(str::to_string),
                rect: Rect {
                    x,
                    y: top,
                    w: cw,
                    h: piece.h,
                },
                head: Rect {
                    x: x + fpad,
                    y: top + fpad,
                    w: cw - fpad * 2.0,
                    h: fhead,
                },
                members: Vec::new(),
                more: None,
                hidden_ids: Vec::new(),
            };
            let mut y = top + fpad + fhead;
            for e in &piece.entries {
                let h = c.entry_h(f, e);
                match *e {
                    Entry::Member { m, row } => {
                        let mem = &f.members[m];
                        let indent = mem.depth as f32 * INDENT * s;
                        let rect = Rect {
                            x: x + fpad + indent,
                            y,
                            w: cw - fpad * 2.0 - indent,
                            h,
                        };
                        out.cards.insert(
                            mem.id.clone(),
                            CardSlot {
                                rect,
                                mode: if row { CardMode::Row } else { CardMode::Full },
                                z: 0,
                                depth: mem.depth,
                            },
                        );
                        shape.members.push(ShapeMember {
                            id: mem.id.clone(),
                            parent: None,
                            rect,
                        });
                    }
                    Entry::More(n) => {
                        let indent = INDENT * s;
                        shape.more = Some((
                            Rect {
                                x: x + fpad + indent,
                                y,
                                w: cw - fpad * 2.0 - indent,
                                h,
                            },
                            n,
                        ));
                    }
                }
                y += h + CHILD_GAP * s;
            }
            // 親は、この枠に置いたメンバーの中にいるときだけ線の起点にする。
            let placed: Vec<String> = shape.members.iter().map(|m| m.id.clone()).collect();
            for sm in &mut shape.members {
                let parent = f
                    .members
                    .iter()
                    .find(|m| m.id == sm.id)
                    .and_then(|m| m.parent)
                    .map(|p| f.members[p].id.clone())
                    .filter(|p| placed.contains(p));
                sm.parent = parent;
            }
            if shape.more.is_some() {
                shape.hidden_ids = f
                    .members
                    .iter()
                    .filter(|m| m.unit.is_some_and(|u| fam.states[u] == UnitState::Hidden))
                    .map(|m| m.id.clone())
                    .collect();
            }
            out.families.push(shape);
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq)]
pub struct ShapeMember {
    pub id: String,
    /// 同じ枠に置いた親のメンバー ID。None なら枠の見出しから線を引く(親情報なし・続き)。
    pub parent: Option<String>,
    pub rect: Rect,
}

/// 配置した親子グループの枠 1 つ(列をまたぐ組は列ごとに 1 つ)。
#[derive(Clone, Debug, PartialEq)]
pub struct FamilyShape {
    /// `plan_board` に渡したプロジェクトの位置と、その中の組の位置。
    pub project: usize,
    pub family: usize,
    /// 次の列へ続いた 2 つ目以降の枠(親のカードは無い)。
    pub continued: bool,
    pub orphan: bool,
    /// この組の親のカードの ID。親情報なしの組は None。
    pub root: Option<String>,
    pub rect: Rect,
    /// 見出しの行。
    pub head: Rect,
    /// 枠の中のカードを上から順に。
    pub members: Vec<ShapeMember>,
    /// 「ほか N 件」の行の位置と件数。
    pub more: Option<(Rect, usize)>,
    /// 「ほか N 件」にまとめたスレッドの ID。
    pub hidden_ids: Vec<String>,
}

/// 枠・見出し・接続線の位置。カードの現在位置(動いている途中の位置)から毎フレーム求める。
#[derive(Clone, Debug, PartialEq)]
pub struct Decor {
    pub frame: Rect,
    pub head: Rect,
    pub more: Option<Rect>,
    /// 接続線(細い長方形)。
    pub links: Vec<Rect>,
}

/// `pose` が各カードの現在位置を返す(無ければ配置した位置)。
pub fn decor(shape: &FamilyShape, s: f32, pose: &dyn Fn(&str) -> Option<Rect>) -> Decor {
    let cur = |m: &ShapeMember| pose(&m.id).unwrap_or(m.rect);
    let (dx, dy) = shape.members.first().map_or((0.0, 0.0), |m| {
        let c = cur(m);
        (c.x - m.rect.x, c.y - m.rect.y)
    });
    let shift = |r: Rect| Rect {
        x: r.x + dx,
        y: r.y + dy,
        ..r
    };
    let pad = FAM_PAD * s;
    let mut frame = shift(shape.rect);
    let head = shift(shape.head);
    let more = shape.more.map(|(r, _)| shift(r));
    for m in &shape.members {
        frame.h = frame.h.max(cur(m).y + cur(m).h + pad - frame.y);
    }
    if let Some(m) = more {
        frame.h = frame.h.max(m.y + m.h + pad - frame.y);
    }
    let lw = LINK_W * s;
    let mut links = Vec::new();
    let mut link = |from: Rect, to: Rect, from_bottom: f32| {
        let x = from.x + LINK_INSET * s;
        let mid = to.y + to.h / 2.0;
        if mid > from_bottom {
            links.push(Rect {
                x: x - lw / 2.0,
                y: from_bottom,
                w: lw,
                h: mid - from_bottom + lw / 2.0,
            });
        }
        if to.x > x {
            links.push(Rect {
                x: x - lw / 2.0,
                y: mid - lw / 2.0,
                w: to.x - x + lw / 2.0,
                h: lw,
            });
        }
    };
    let anchor = head;
    let root = shape.root.as_deref();
    for m in &shape.members {
        if Some(m.id.as_str()) == root {
            continue;
        }
        let parent = m
            .parent
            .as_deref()
            .and_then(|p| shape.members.iter().find(|x| x.id == p))
            .map(cur);
        match parent {
            Some(p) => link(p, cur(m), p.y + p.h),
            None => link(anchor, cur(m), head.y + head.h),
        }
    }
    if let Some(m) = more {
        match shape
            .members
            .iter()
            .find(|x| Some(x.id.as_str()) == root)
            .map(cur)
        {
            Some(p) => link(p, m, p.y + p.h),
            None => link(anchor, m, head.y + head.h),
        }
    }
    Decor {
        frame,
        head,
        more,
        links,
    }
}

/// 全プロジェクトの組を、ページごとの配置にする。
/// `projects` はプロジェクトの表示順で、各プロジェクトの組は重要な順。ページは 1 枚以上。
/// 全部が 1 枚に収まれば 1 ページ。収まらなければ、要確認を持つ組を先にして組単位で分ける。
pub fn plan_board(area: Rect, scale: f32, projects: &[Vec<Family>]) -> Vec<Layout> {
    let s = scale;
    if area.w <= 0.0 || area.h <= 0.0 || projects.iter().all(Vec::is_empty) {
        return vec![Layout::default()];
    }
    if projects.iter().flatten().all(|f| !f.framed()) {
        let groups: Vec<Vec<String>> = projects
            .iter()
            .map(|p| p.iter().map(|f| f.members[0].id.clone()).collect())
            .collect();
        return vec![layout(area, s, &groups)];
    }
    let max_cols = (((area.w + LANE_GAP * s) / (MIN_W * s + LANE_GAP * s)).floor() as usize).max(1);
    let c = Ctx { s, area, max_cols };
    let mut order: Vec<(usize, usize)> = projects
        .iter()
        .enumerate()
        .flat_map(|(p, fams)| (0..fams.len()).map(move |i| (p, i)))
        .collect();
    order.sort_by_key(|&(p, i)| {
        let f = &projects[p][i];
        (!f.has_attention(), f.best(), p, i)
    });
    let mut pages: Vec<Vec<(usize, usize)>> = Vec::new();
    let mut cur: Vec<(usize, usize)> = Vec::new();
    for item in order {
        cur.push(item);
        if build_page(&c, projects, &cur, false).is_some() {
            continue;
        }
        cur.pop();
        if !cur.is_empty() {
            pages.push(std::mem::take(&mut cur));
        }
        cur.push(item);
        if build_page(&c, projects, &cur, false).is_none() {
            pages.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        pages.push(cur);
    }
    pages
        .iter()
        .filter_map(|items| build_page(&c, projects, items, true))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect {
        x: 32.0,
        y: 212.0,
        w: 2496.0,
        h: 788.0,
    };

    fn node(id: &str, parent: Option<&str>, tier: Tier) -> Node {
        Node {
            id: id.into(),
            parent: parent.map(str::to_string),
            tier,
        }
    }
    fn fams(nodes: &[Node]) -> Vec<Family> {
        build_families(nodes)
    }
    fn member<'a>(f: &'a Family, id: &str) -> &'a Member {
        f.members.iter().find(|m| m.id == id).unwrap()
    }
    /// 親 "p" の子 `a`(承認)・`r`(実行中)・`q0..q{n}`(静か)の組。
    fn family_with(n_quiet: usize, prefix: &str) -> Vec<Node> {
        let p = format!("{prefix}p");
        let mut v = vec![
            node(&p, None, Tier::Running),
            node(&format!("{prefix}a"), Some(&p), Tier::Attention),
            node(&format!("{prefix}r"), Some(&p), Tier::Running),
        ];
        for i in 0..n_quiet {
            v.push(node(&format!("{prefix}q{i}"), Some(&p), Tier::Quiet));
        }
        v
    }
    fn all_ids(l: &Layout) -> Vec<&String> {
        l.cards.keys().collect()
    }

    #[test]
    fn children_follow_their_parent_attention_then_running_then_quiet() {
        // nodes は重要な順(AgentScreen::rank の結果)で渡す。親は静かでも先頭。
        let nodes = vec![
            node("ask", Some("p"), Tier::Attention),
            node("run", Some("p"), Tier::Running),
            node("p", None, Tier::Quiet),
            node("done", Some("p"), Tier::Quiet),
            node("idle", Some("p"), Tier::Quiet),
            node("solo", None, Tier::Quiet),
        ];
        let f = fams(&nodes);
        assert_eq!(f.len(), 2);
        // 最も重要なメンバーを持つ組が先。
        assert_eq!(f[0].members[0].id, "p");
        assert_eq!(
            f[0].members
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>(),
            ["p", "ask", "run", "done", "idle"]
        );
        assert_eq!(f[0].members[0].depth, 0);
        assert!(f[0].members[1..].iter().all(|m| m.depth == 1));
        assert!(f[0].framed() && f[0].has_attention());
        assert!(!f[1].framed());
        assert_eq!(f[1].members[0].id, "solo");
    }

    #[test]
    fn grandchildren_are_indented_one_more_step_and_capped_at_two() {
        let nodes = vec![
            node("p", None, Tier::Running),
            node("c", Some("p"), Tier::Running),
            node("g", Some("c"), Tier::Running),
            node("gg", Some("g"), Tier::Running),
            node("ggg", Some("gg"), Tier::Quiet),
            node("c2", Some("p"), Tier::Quiet),
        ];
        let f = &fams(&nodes)[0];
        let ids: Vec<_> = f.members.iter().map(|m| (m.id.as_str(), m.depth)).collect();
        // 孫は親の子の直後。深さは 0, 1, 2, 2, 2。
        assert_eq!(
            ids,
            [
                ("p", 0),
                ("c", 1),
                ("g", 2),
                ("gg", 2),
                ("ggg", 2),
                ("c2", 1)
            ]
        );
        assert_eq!(f.members[2].parent, Some(1));
    }

    #[test]
    fn children_without_a_visible_parent_group_by_missing_parent() {
        let nodes = vec![
            node("x1", Some("gone-a"), Tier::Running),
            node("y1", Some("gone-b"), Tier::Quiet),
            node("x2", Some("gone-a"), Tier::Quiet),
            node("fork", None, Tier::Running),
        ];
        let f = fams(&nodes);
        assert_eq!(f.len(), 3);
        let a = f
            .iter()
            .find(|f| f.orphan_of.as_deref() == Some("gone-a"))
            .unwrap();
        assert_eq!(
            a.members.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["x1", "x2"]
        );
        assert!(a.members.iter().all(|m| m.depth == 1 && m.parent.is_none()));
        assert!(a.framed() && a.root_id().is_none());
        assert_eq!(a.lead, 0);
        // 親を持たない(fork など)スレッドは、親情報なしにも親子にもならず単独のまま。
        let solo = f.iter().find(|f| f.members[0].id == "fork").unwrap();
        assert!(!solo.framed() && solo.orphan_of.is_none());
    }

    #[test]
    fn parent_cycles_and_self_parents_do_not_hang() {
        let nodes = vec![
            node("a", Some("b"), Tier::Running),
            node("b", Some("a"), Tier::Running),
            node("s", Some("s"), Tier::Running),
        ];
        let f = fams(&nodes);
        assert_eq!(f.iter().map(|f| f.members.len()).sum::<usize>(), 3);
        let ab = f.iter().find(|f| f.members.len() == 2).unwrap();
        assert_eq!(ab.members[0].id, "a");
        let solo = f.iter().find(|f| f.members[0].id == "s").unwrap();
        assert!(!solo.framed());
    }

    #[test]
    fn only_all_quiet_subtrees_are_units() {
        let nodes = vec![
            node("p", None, Tier::Running),
            node("busy", Some("p"), Tier::Quiet),
            node("busy-g", Some("busy"), Tier::Attention),
            node("q1", Some("p"), Tier::Quiet),
            node("q1-g", Some("q1"), Tier::Quiet),
        ];
        let f = &fams(&nodes)[0];
        assert_eq!(member(f, "p").unit, None);
        assert_eq!(member(f, "busy").unit, None);
        assert_eq!(member(f, "busy-g").unit, None);
        assert!(member(f, "q1").unit.is_some());
        assert_eq!(member(f, "q1").unit, member(f, "q1-g").unit);
        assert_eq!(f.units, 1);
    }

    #[test]
    fn solo_only_boards_keep_the_classic_layout() {
        let nodes: Vec<Node> = (0..7)
            .map(|i| node(&format!("t{i}"), None, Tier::Running))
            .collect();
        let projects = vec![fams(&nodes[..5]), fams(&nodes[5..])];
        let ids: Vec<Vec<String>> = projects
            .iter()
            .map(|p| p.iter().map(|f| f.members[0].id.clone()).collect())
            .collect();
        let pages = plan_board(AREA, 1.0, &projects);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0], layout(AREA, 1.0, &ids));
        assert!(pages[0].families.is_empty());
    }

    #[test]
    fn a_family_fits_one_column_by_shrinking_quiet_children_to_rows() {
        let nodes = family_with(3, "");
        let projects = vec![fams(&nodes)];
        let pages = plan_board(AREA, 1.0, &projects);
        assert_eq!(pages.len(), 1);
        let l = &pages[0];
        assert_eq!(l.cards.len(), nodes.len());
        let slot = |id: &str| l.cards[id];
        // 親 > 子(字下げ)、承認待ちの子が実行中の子より上。
        assert_eq!(slot("p").depth, 0);
        assert_eq!(slot("a").depth, 1);
        assert!(slot("a").rect.x > slot("p").rect.x);
        assert!(slot("a").rect.y < slot("r").rect.y);
        assert!(slot("p").rect.y < slot("a").rect.y);
        // 親は大きいカード、静かな子は 1 行。要確認と実行中はカード。
        assert!(slot("p").rect.h > slot("a").rect.h);
        assert_eq!(slot("a").mode, CardMode::Full);
        assert_eq!(slot("r").mode, CardMode::Full);
        assert_eq!(slot("q0").mode, CardMode::Row);
        assert!(slot("q0").rect.h < 60.0);
        assert_eq!(l.families.len(), 1);
        assert!(l.families[0].more.is_none() && !l.families[0].continued);
        for c in l.cards.values() {
            assert!(c.rect.y + c.rect.h <= AREA.y + AREA.h + 0.5);
            assert!(c.rect.x >= AREA.x - 0.5 && c.rect.x + c.rect.w <= AREA.x + AREA.w + 0.5);
        }
    }

    #[test]
    fn a_family_taller_than_a_column_continues_in_the_next_column() {
        // 実行中の子が 6 人。静かな子がいないので縮められず、列を超える。
        let mut nodes = vec![node("p", None, Tier::Running)];
        nodes.extend((0..6).map(|i| node(&format!("c{i}"), Some("p"), Tier::Running)));
        let pages = plan_board(AREA, 1.0, &[fams(&nodes)]);
        assert_eq!(pages.len(), 1);
        let l = &pages[0];
        assert_eq!(l.cards.len(), 7);
        assert_eq!(l.families.len(), 2, "{:?}", l.families.len());
        assert!(!l.families[0].continued && l.families[1].continued);
        // 続きの枠には親のカードが無く、親の隣の列から始まる。
        assert!(l.families[1].members.iter().all(|m| m.id != "p"));
        assert!(l.families[1].rect.x > l.families[0].rect.x);
        assert_eq!(l.lanes.len(), 2);
        assert!(l.lanes[1].cont && !l.lanes[0].cont);
        // 続きの子は枠の見出しから線を引く。
        assert!(l.families[1].members.iter().all(|m| m.parent.is_none()));
        // 子の並びは保たれる。
        let ys: Vec<f32> = (0..6).map(|i| l.cards[&format!("c{i}")].rect.y).collect();
        assert!(ys[..3].windows(2).all(|w| w[0] < w[1]));
        assert!(l.cards["c5"].rect.x > l.cards["c0"].rect.x);
    }

    #[test]
    fn quiet_children_collapse_into_a_count_when_columns_run_out() {
        // 狭い画面(1 列)に、静かな子を多く持つ組が 2 つ。
        let area = Rect { w: 400.0, ..AREA };
        let nodes = family_with(8, "x");
        let projects = vec![fams(&nodes)];
        let pages = plan_board(area, 1.0, &projects);
        let l = &pages[0];
        assert_eq!(pages.len(), 1, "静かな子の集約で 1 ページに収まる");
        let hidden: Vec<&String> = l.families.iter().flat_map(|f| &f.hidden_ids).collect();
        assert!(!hidden.is_empty());
        // 要確認と実行中の子、親は必ずカードで出る。集約されるのは静かな子だけ。
        for id in ["xp", "xa", "xr"] {
            assert!(l.cards.contains_key(id), "{id}");
            assert!(!hidden.contains(&&id.to_string()), "{id}");
        }
        assert!(hidden.iter().all(|id| id.contains('q')));
        let counted: usize = l.families.iter().filter_map(|f| f.more.map(|m| m.1)).sum();
        assert_eq!(counted, hidden.len());
        assert_eq!(l.cards.len() + hidden.len(), nodes.len());
        // 集約した子の置き場は親のカード。
        let anchor = l.anchor(hidden[0]).unwrap();
        assert_eq!(anchor, l.cards["xp"].rect);
        assert!(l.anchor("nobody").is_none());
    }

    #[test]
    fn attention_children_are_never_hidden_or_shrunk() {
        let area = Rect { w: 400.0, ..AREA };
        let mut nodes = vec![node("p", None, Tier::Running)];
        nodes.extend((0..3).map(|i| node(&format!("a{i}"), Some("p"), Tier::Attention)));
        nodes.extend((0..12).map(|i| node(&format!("q{i}"), Some("p"), Tier::Quiet)));
        let pages = plan_board(area, 1.0, &[fams(&nodes)]);
        for l in &pages {
            for i in 0..3 {
                if let Some(c) = l.cards.get(&format!("a{i}")) {
                    assert_eq!(c.mode, CardMode::Full);
                }
            }
            assert!(
                l.families
                    .iter()
                    .flat_map(|f| &f.hidden_ids)
                    .all(|id| id.starts_with('q'))
            );
        }
        let placed: usize = pages
            .iter()
            .map(|l| {
                (0..3)
                    .filter(|i| l.cards.contains_key(&format!("a{i}")))
                    .count()
            })
            .sum();
        assert_eq!(placed, 3);
    }

    #[test]
    fn when_nothing_fits_pages_split_by_family_with_attention_first() {
        // 狭い画面に、実行中の子を 3 人ずつ持つ組を 9 つ。承認待ちの子がいる組は最後に渡す。
        let area = Rect { w: 400.0, ..AREA };
        let mut projects: Vec<Vec<Family>> = Vec::new();
        for g in 0..9 {
            let p = format!("g{g}");
            let mut nodes = vec![node(&p, None, Tier::Running)];
            let tier = if g == 8 {
                Tier::Attention
            } else {
                Tier::Running
            };
            nodes.extend((0..3).map(|i| node(&format!("{p}c{i}"), Some(&p), tier)));
            projects.push(fams(&nodes));
        }
        let pages = plan_board(area, 1.0, &projects);
        assert!(pages.len() > 1, "{}", pages.len());
        // 要確認の組は最初のページ。
        assert!(pages[0].cards.contains_key("g8c0"));
        // 全スレッドがどれかのページに 1 回ずつ載る。
        let mut seen: Vec<String> = pages.iter().flat_map(|l| l.cards.keys().cloned()).collect();
        seen.sort();
        let before = seen.len();
        seen.dedup();
        assert_eq!(before, seen.len());
        assert_eq!(seen.len(), 9 * 4);
        // 組はページをまたがない(親と子は同じページ)。
        for l in &pages {
            for g in 0..9 {
                let on = |id: String| l.cards.contains_key(&id);
                assert_eq!(on(format!("g{g}")), on(format!("g{g}c0")));
            }
        }
        // どのページも画面の中。
        for l in &pages {
            for c in l.cards.values() {
                assert!(c.rect.y + c.rect.h <= area.y + area.h + 0.5);
                assert!(c.rect.x + c.rect.w <= area.x + area.w + 0.5);
            }
        }
    }

    #[test]
    fn a_family_larger_than_the_screen_still_gets_a_bounded_page() {
        let area = Rect { w: 400.0, ..AREA };
        let mut nodes = vec![node("p", None, Tier::Running)];
        nodes.extend((0..40).map(|i| node(&format!("c{i}"), Some("p"), Tier::Attention)));
        let pages = plan_board(area, 1.0, &[fams(&nodes)]);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].cards.len(), 41);
    }

    #[test]
    fn orphan_groups_get_a_frame_without_a_parent_card() {
        let nodes = vec![
            node("o1", Some("gone"), Tier::Running),
            node("o2", Some("gone"), Tier::Quiet),
            node("s", None, Tier::Quiet),
        ];
        let pages = plan_board(AREA, 1.0, &[fams(&nodes)]);
        let l = &pages[0];
        assert_eq!(l.cards.len(), 3);
        assert_eq!(l.families.len(), 1);
        let shape = &l.families[0];
        assert!(shape.orphan && shape.root.is_none());
        assert_eq!(l.cards["o1"].depth, 1);
        assert!(shape.members.iter().all(|m| m.parent.is_none()));
        // 単独スレッドは枠なし・字下げなし。
        assert_eq!(l.cards["s"].depth, 0);
        assert!(
            l.cards["s"].rect.x < l.cards["o1"].rect.x
                || l.cards["s"].rect.y != l.cards["o1"].rect.y
        );
    }

    #[test]
    fn links_reach_the_cards_and_follow_the_given_positions() {
        let mut nodes = family_with(1, "");
        nodes.push(node("g", Some("r"), Tier::Quiet));
        let pages = plan_board(AREA, 1.0, &[fams(&nodes)]);
        let l = &pages[0];
        let shape = &l.families[0];
        let rect_of = |id: &str| Some(l.cards[id].rect);
        let d = decor(shape, 1.0, &rect_of);
        assert!(!d.links.is_empty());
        // 子ごとに、親の下から子の中央の高さまで縦線が届き、横線が子のカードの左端に届く。
        for id in ["a", "r", "g", "q0"] {
            let c = l.cards[id].rect;
            let mid = c.y + c.h / 2.0;
            let reaches_card = d
                .links
                .iter()
                .any(|k| (k.x + k.w - c.x).abs() < 0.01 && k.y <= mid && mid <= k.y + k.h);
            assert!(reaches_card, "{id}");
        }
        // 孫の縦線は、親の子(r)から出る。
        let r = l.cards["r"].rect;
        let g = l.cards["g"].rect;
        assert!(d.links.iter().any(|k| k.w < 4.0
            && k.x > r.x
            && k.x < r.x + 20.0
            && (k.y - (r.y + r.h)).abs() < 0.01
            && k.y + k.h >= g.y + g.h / 2.0));
        // 枠は全メンバーを含む。
        for id in ["p", "a", "r", "g", "q0"] {
            let c = l.cards[id].rect;
            assert!(c.x >= d.frame.x && c.y >= d.frame.y);
            assert!(
                c.x + c.w <= d.frame.x + d.frame.w + 0.01
                    && c.y + c.h <= d.frame.y + d.frame.h + 0.01
            );
        }
        // 動いている最中は、現在位置に線と枠が付いてくる。
        let moved = |id: &str| {
            l.cards.get(id).map(|c| Rect {
                y: c.rect.y + 100.0,
                ..c.rect
            })
        };
        let d2 = decor(shape, 1.0, &moved);
        assert!((d2.frame.y - d.frame.y - 100.0).abs() < 0.01);
        assert!((d2.links[0].y - d.links[0].y - 100.0).abs() < 0.01);
        let _ = all_ids(l);
    }
}
