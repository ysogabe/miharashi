//! 親子グループ(subagent の親子)の組み立てと配置。描画にも時刻にも依存しない純粋な計算。
//!
//! 流れ: `build_families`(スレッドを親子のまとまりにする)→ `plan_board`(プロジェクトの枠に詰める)。
//! 親子グループが 1 つも無いときは、従来の単独カードの配置(`layout::layout`)にそのまま任せる。
//!
//! 詰め方(列は上から下へ流す):
//! 1. 親子グループは 1 列に収まる範囲で、静かな子(応答完了・その他)を 1 行表示に縮める。
//! 2. それでも 1 列を超える組は、次の列に「…の子・続き」として続ける。
//! 3. 列が足りなければ、静かな子を「ほか N 件」に集約する(要確認の子は集約しない)。
//! 4. それでも入らなければ、全部の組を 1 枚のまま scale を段階的に下げて 1〜3 をやり直す(`SHRINK_STEP`、
//!    下限 `SHRINK_FLOOR`)。小さい scale では最小幅も小さくなり、列が増える。
//! 5. 下限の scale でも入らないとき(要確認の子が非常に多いなど)の最後の手段は、列を縦に仮想的に伸ばして
//!    詰め、できた列を画面の高さに縮めること(`overlap_fit`)。カードは高さを保ったまま重なり、要確認は
//!    手前に出る。縮めるのはカードが連なる間隔だけで、見出しと「ほか N 件」の行は縮めない。
//!    ページには分けず、要確認のカードを画面の外に置くことも、欠落させることもしない。
//! 6. ただし GLES の合成の上限(面・テクスチャ)を超える枚数は 1 枚に出せないので、配置が上限を超えるときは
//!    重要でない方(その他 → 実行中 → 要確認)から「ほか N 件」にまとめてやり直す(`plan_board`、`cut_order`)。
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
    /// 表示の予算(`SurfaceBudget`)のために「ほか N 件」へ回したスレッドの ID。`build_families` は空で作り、
    /// `plan_board` が配置のためにだけ詰める。
    pub cut_ids: Vec<String>,
    /// `cut_ids` のうち、要確認のスレッドの数。
    pub cut_attention: usize,
}

impl Family {
    /// 枠と接続線を付ける組か。
    pub fn framed(&self) -> bool {
        self.members.len() > 1 || self.orphan_of.is_some() || !self.cut_ids.is_empty()
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
    let units = assign_units(&mut members, orphan_of.is_some());
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
        cut_ids: Vec::new(),
        cut_attention: 0,
    }
}

/// 静かな部分木(自分と子孫が全員 Quiet)に単位の番号を振り、単位の数を返す。親(深さ 0)は縮めも隠しもしない。
/// 親が単位に入っていない静かな子から新しい単位を始め、その子孫は同じ単位に入る。
fn assign_units(members: &mut [Member], orphan: bool) -> usize {
    let mut quiet: Vec<bool> = members.iter().map(|m| m.tier == Tier::Quiet).collect();
    for m in (0..members.len()).rev() {
        if !quiet[m]
            && let Some(p) = members[m].parent
        {
            quiet[p] = false;
        }
    }
    let mut units = 0;
    for m in 0..members.len() {
        members[m].unit = None;
        if !quiet[m] || (members[m].depth == 0 && !orphan) {
            continue;
        }
        let inherited = members[m].parent.and_then(|p| members[p].unit);
        members[m].unit = inherited.or_else(|| {
            units += 1;
            Some(units - 1)
        });
    }
    units
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
    /// 空の列に置ける高さ(列の見出しと下端を除く)。
    fn fresh(&self) -> f32 {
        self.area.h - self.head() - self.foot()
    }
    /// 枠 1 つの余白と見出しの高さ。
    fn overhead(&self) -> f32 {
        (FAM_PAD * 2.0 + FAM_HEAD) * self.s
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
    let mut hidden = f.cut_ids.len();
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
    over: bool,
}

struct Packer<'a> {
    c: &'a Ctx,
    fams: &'a [PageFam<'a>],
    segs: Vec<Seg>,
    col: usize,
    y: f32,
    open: Option<usize>,
    /// 使える列の数。これを超える列が要るとわかった時点で打ち切る。
    limit: usize,
    over: bool,
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
        if self.col >= self.limit {
            self.over = true;
            return;
        }
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

    /// 組を置く。収まらなければ次の列へ続ける。高さは前から 1 回なめて足すだけで決める。
    fn place(&mut self, project: usize, fam: usize) {
        let f = self.fams[fam].f;
        let framed = f.framed();
        let entries = entries_of(f, &self.fams[fam].states);
        let n = entries.len();
        if n == 0 {
            return;
        }
        let c = self.c;
        // pre[i] = 先頭 i 件の高さの和。a..b 番目を 1 枠にしたときの高さが O(1) で出る。
        let mut pre = Vec::with_capacity(n + 1);
        pre.push(0.0f32);
        for e in &entries {
            pre.push(pre[pre.len() - 1] + c.entry_h(f, e));
        }
        let child_gap = CHILD_GAP * c.s;
        let height = |a: usize, b: usize| {
            let sum = pre[b] - pre[a];
            if framed {
                sum + (b - a - 1) as f32 * child_gap + c.overhead()
            } else {
                sum
            }
        };
        let mut start = 0;
        let mut continued = false;
        loop {
            if self.over {
                return;
            }
            let i = self.open.expect("open segment");
            let remaining = c.area.h - self.y - c.foot();
            let whole = height(start, n);
            if whole <= remaining + 0.5 {
                self.push_piece(fam, entries[start..].to_vec(), framed, continued);
                return;
            }
            let can_gain = !self.segs[i].pieces.is_empty() || self.segs[i].top > 0.0;
            if can_gain && (whole <= c.fresh() + 0.5 || !framed) {
                self.next_col(project);
                continue;
            }
            if !framed {
                self.push_piece(fam, entries[start..].to_vec(), framed, continued);
                return;
            }
            let kmin = (if continued || f.orphan_of.is_some() {
                1
            } else {
                2
            })
            .min(n - start);
            // 入る最大の件数。高さは件数に対して増える一方なので、入らなくなったところで止める。
            let mut k = 0;
            for take in kmin..=n - start {
                if height(start, start + take) > remaining + 0.5 {
                    break;
                }
                k = take;
            }
            if k == 0 {
                if can_gain {
                    self.next_col(project);
                    continue;
                }
                k = kmin;
            }
            self.push_piece(fam, entries[start..start + k].to_vec(), true, continued);
            continued = true;
            start += k;
            if start >= n {
                return;
            }
            self.next_col(project);
        }
    }
}

/// 配置する組。`states` は単位ごとの表示。
struct PageFam<'a> {
    f: &'a Family,
    project: usize,
    idx: usize,
    states: Vec<UnitState>,
}

/// `limit` 列を超える配置になりそうなら、そこで打ち切って `over` を立てる。
fn pack(c: &Ctx, fams: &[PageFam<'_>], fresh_columns: bool, limit: usize) -> Packed {
    let mut p = Packer {
        c,
        fams,
        segs: Vec::new(),
        col: 0,
        y: 0.0,
        open: None,
        limit,
        over: false,
    };
    let mut projects: Vec<usize> = fams.iter().map(|f| f.project).collect();
    projects.dedup();
    'projects: for project in projects {
        if fresh_columns && p.y > 0.0 {
            p.col += 1;
            p.y = 0.0;
            if p.col >= p.limit {
                p.over = true;
                break;
            }
        }
        p.begin(project);
        for fi in (0..fams.len()).filter(|&fi| fams[fi].project == project) {
            p.place(project, fi);
            if p.over {
                break 'projects;
            }
        }
        p.end_seg();
    }
    p.end_seg();
    let cols = p.segs.iter().map(|s| s.col + 1).max().unwrap_or(0);
    Packed {
        segs: p.segs,
        cols,
        over: p.over,
    }
}

fn try_pack(c: &Ctx, fams: &[PageFam<'_>]) -> Option<Packed> {
    [true, false]
        .into_iter()
        .map(|fresh| pack(c, fams, fresh, c.max_cols))
        .find(|packed| !packed.over)
}

/// 単位ごとの、含まれるスレッドの数。
fn unit_sizes(f: &Family) -> Vec<usize> {
    let mut sizes = vec![0; f.units];
    for u in f.members.iter().filter_map(|m| m.unit) {
        sizes[u] += 1;
    }
    sizes
}

/// 組の最初の表示。1 列に収まるまで静かな単位を後ろから 1 行表示にする。
/// 全部を 1 行にしても画面全体(`max_cols` 列)に入らない分は、先に「ほか N 件」にまとめる
/// (並べられる行数には画面の広さという上限があり、件数が多くても配置の計算量を抑えられる)。
fn base_states(c: &Ctx, f: &Family) -> Vec<UnitState> {
    let mut states = vec![UnitState::Card; f.units];
    if f.units == 0 {
        return states;
    }
    let sizes = unit_sizes(f);
    let s = c.s;
    let mut h = c.piece_h(f, f.framed(), &entries_of(f, &states));
    for u in (0..f.units).rev() {
        if h <= c.fresh() {
            break;
        }
        states[u] = UnitState::Row;
        h -= sizes[u] as f32 * (CHILD_H - ROW_H) * s;
    }
    let gap = CHILD_GAP * s;
    let mut total: f32 = f
        .members
        .iter()
        .map(|m| {
            gap + s * match (m.unit, m.depth) {
                (Some(_), _) => ROW_H,
                (None, 0) => CARD_H,
                (None, _) => CHILD_H,
            }
        })
        .sum();
    // 予算で「ほか N 件」にしたスレッドがいれば、その行はもう数えてある。
    let mut counted = !f.cut_ids.is_empty();
    if counted {
        total += MORE_LINE * s + gap;
    }
    let capacity = c.max_cols as f32 * (c.fresh() - c.overhead() + gap);
    for u in (0..f.units).rev() {
        if total <= capacity {
            break;
        }
        total -= sizes[u] as f32 * (ROW_H * s + gap);
        if !counted {
            total += MORE_LINE * s + gap;
            counted = true;
        }
        states[u] = UnitState::Hidden;
    }
    states
}

/// 隠す順(組, 単位)。隠せる単位が最も多い組から、後ろの単位を 1 つずつ。
fn hide_order(fams: &[PageFam<'_>]) -> Vec<(usize, usize)> {
    let mut heap: std::collections::BinaryHeap<(usize, usize)> = fams
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
        .collect();
    let mut order = Vec::new();
    while let Some((left, i)) = heap.pop() {
        order.push((i, left - 1));
        if left > 1 {
            heap.push((left - 1, i));
        }
    }
    order
}

/// `items`((プロジェクト, そのプロジェクトの組の位置))を 1 枚に詰める。
/// 入らなければ、全部を 1 行表示 → 静かな単位を隠していく(隠す数は二分探索で決める)、の順に縮める。
/// 隠せる静かな単位を全部隠しても入らないときは、縮めきった組を返す(要確認の子は縮めない)。
fn fit_page<'a>(
    c: &Ctx,
    projects: &'a [Vec<Family>],
    bases: &[Vec<Vec<UnitState>>],
    items: &[(usize, usize)],
) -> Result<Layout, Vec<PageFam<'a>>> {
    let mut items = items.to_vec();
    items.sort_unstable();
    let mut fams: Vec<PageFam<'a>> = items
        .iter()
        .map(|&(project, idx)| PageFam {
            f: &projects[project][idx],
            project,
            idx,
            states: bases[project][idx].clone(),
        })
        .collect();
    if let Some(packed) = try_pack(c, &fams) {
        return Ok(finish(c, &fams, packed, &[]));
    }
    let mut changed = false;
    for fam in &mut fams {
        for s in &mut fam.states {
            if *s == UnitState::Card {
                *s = UnitState::Row;
                changed = true;
            }
        }
    }
    if changed && let Some(packed) = try_pack(c, &fams) {
        return Ok(finish(c, &fams, packed, &[]));
    }
    let order = hide_order(&fams);
    let rows: Vec<Vec<UnitState>> = fams.iter().map(|f| f.states.clone()).collect();
    match min_hidden(c, &mut fams, &rows, &order) {
        Some((k, packed)) => {
            hide(&mut fams, &rows, &order, k);
            Ok(finish(c, &fams, packed, &[]))
        }
        None => Err(fams),
    }
}

/// `rows`(全部 1 行表示)から、`order` の先頭 `k` 件の単位を隠した状態にする。
fn hide(fams: &mut [PageFam<'_>], rows: &[Vec<UnitState>], order: &[(usize, usize)], k: usize) {
    for (fam, base) in fams.iter_mut().zip(rows) {
        fam.states.clone_from(base);
    }
    for &(i, u) in &order[..k] {
        fams[i].states[u] = UnitState::Hidden;
    }
}

/// 二分探索で見つけた数より下を、1 つずつ確かめる回数の上限(これを超えるときは見つけた数のまま)。
const HIDE_SCAN_MAX: usize = 256;

/// 入る最小の隠す数(と、そのときの配置)。全部隠しても入らなければ None。
/// 隠す数が多いほど入りやすいのが普通だが、「組ごと次の列へ送る」判定のせいで、隠す数を増やすと
/// 入ったり入らなかったりすることがある(10 で入り、11 で入らず、12 で入る、など)。二分探索だけでは
/// 余分に隠してしまうので、次のようにする。
/// 1. 二分探索で、入る数 `hi` を見つける(入る数が見つかれば、これ以上隠す必要は無い)。
/// 2. 面積から決まる下限(列の数 × 列の高さに、残るカードの高さと枠の余白の和が入る最小の数)`lb` を求める。
///    これは隠す数に対して単調で、`lb` より少ない数では入らない。
/// 3. `lb` から `hi` の手前まで、小さい方から 1 つずつ確かめ、最初に入った数を採る。
///
/// 確かめるのは最大 `HIDE_SCAN_MAX` 回なので、計算量は O((log n + HIDE_SCAN_MAX) · 1 回の詰め)。
fn min_hidden(
    c: &Ctx,
    fams: &mut Vec<PageFam<'_>>,
    rows: &[Vec<UnitState>],
    order: &[(usize, usize)],
) -> Option<(usize, Packed)> {
    hide(fams, rows, order, order.len());
    let mut best = try_pack(c, fams)?;
    let (mut lo, mut hi) = (1, order.len());
    while lo < hi {
        let mid = (lo + hi) / 2;
        hide(fams, rows, order, mid);
        match try_pack(c, fams) {
            Some(packed) => {
                hi = mid;
                best = packed;
            }
            None => lo = mid + 1,
        }
    }
    // 面積の下限。1 組は少なくとも枠 1 つ分の余白と、カード同士の隙間を持つ。隠す単位が 1 つ減るたびに、
    // その単位のスレッドの数 × (行の高さ + 隙間)が減る(「ほか N 件」の行は数えない)。
    let child_gap = CHILD_GAP * c.s;
    let sizes: Vec<Vec<usize>> = fams.iter().map(|fam| unit_sizes(fam.f)).collect();
    let mut left: f32 = fams
        .iter()
        .zip(rows)
        .map(|(fam, states)| {
            let shown: Vec<Entry> = entries_of(fam.f, states)
                .into_iter()
                .filter(|e| !matches!(e, Entry::More(_)))
                .collect();
            let sum: f32 = shown.iter().map(|e| c.entry_h(fam.f, e)).sum();
            if fam.f.framed() {
                sum + shown.len().saturating_sub(1) as f32 * child_gap + c.overhead()
            } else {
                sum
            }
        })
        .sum();
    let capacity = c.max_cols as f32 * c.fresh() + 0.5;
    let mut lb = 0;
    while left > capacity && lb < order.len() {
        let (i, u) = order[lb];
        let gap = if fams[i].f.framed() { child_gap } else { 0.0 };
        left -= sizes[i][u] as f32 * (ROW_H * c.s + gap);
        lb += 1;
    }
    let from = lb.max(1);
    for k in from..hi.min(from + HIDE_SCAN_MAX) {
        hide(fams, rows, order, k);
        if let Some(packed) = try_pack(c, fams) {
            return Some((k, packed));
        }
    }
    Some((hi, best))
}

/// 重ねて詰めた列で、要確認のカードを手前に出すための z の段の幅(1 列に置くカードの数より大きい)。
const TIER_Z: u32 = 1_000_000;

/// 列を仮想的に伸ばす倍率(`overlap_fit`)。
const TALL_GROWTH: f32 = 1.25;

/// 入りきらない盤面を、重ねて必ず 1 枚に収める最後の手段。
/// 列の高さを仮想的に伸ばして(`max_cols` 列に収まるまで)通常どおり詰め、列ごとに縦の位置を
/// 画面の高さに縮める。カードの高さは変えないので、縮めた列ではカードが上下に重なる。
/// 縮めるのはカードが連なっている間隔だけで、プロジェクトと組の見出しの領域は縮めない(`Squash`)。
/// 重なったカードは `Strip`(上端の 1 行だけが見える)にし、要確認 → 実行中 → その他の順に手前に出す。
/// 組の枠・見出し・「ほか N 件」も同じ縮め方で置くので、隠した子の置き場(`Layout::anchor`)も残る。
fn overlap_fit(c: &Ctx, fams: &[PageFam<'_>]) -> Layout {
    let first = pack(c, fams, false, usize::MAX);
    let mut tall = c.area.h * first.cols.div_ceil(c.max_cols).max(1) as f32;
    let (packed, squash) = loop {
        let virt = Ctx {
            s: c.s,
            area: Rect { h: tall, ..c.area },
            max_cols: c.max_cols,
        };
        let packed = pack(&virt, fams, false, c.max_cols);
        if !packed.over || tall > 1.0e15 {
            let squash = squash_columns(c, fams, &packed);
            break (packed, squash);
        }
        tall *= TALL_GROWTH;
    };
    finish(c, fams, packed, &squash)
}

/// 重ねた列の、仮想の縦の位置(列の上端から)と画面の位置(領域の上端から)の対応。
/// `runs` は縮める間隔(仮想の位置の区間)で、1 つの組の最初のカードの上端から最後のカードの上端まで、
/// または見出しを挟まずに並ぶ単独カードの最初から最後まで。それ以外(見出し・余白・各区間の最後のカードの高さ)は
/// 縮めないので、見出しは前の区間のカードに覆われず、組の最初のカードは自分の見出しの下に出る。
struct Squash {
    f: f32,
    runs: Vec<(f32, f32)>,
}

impl Squash {
    fn squashed(&self) -> bool {
        self.f < 1.0
    }

    /// 仮想の縦の位置 `y` の画面での位置。
    fn at(&self, y: f32) -> f32 {
        y - (1.0 - self.f) * self.span(y)
    }

    /// `y` より上にある、縮める間隔の長さの合計。
    fn span(&self, y: f32) -> f32 {
        self.runs
            .iter()
            .map(|&(a, b)| (y.min(b) - a).max(0.0))
            .sum()
    }

    /// どのカード(仮想の上端と高さ)の下端も `limit` に収まる最大の縮め率。縮める間隔だけでは足りなければ None。
    fn solve(&self, cards: &[(f32, f32)], limit: f32) -> Option<f32> {
        let mut f = 1.0f32;
        for &(y, h) in cards {
            let excess = y + h - limit;
            if excess <= 0.0 {
                continue;
            }
            let span = self.span(y);
            if span <= 0.0 {
                return None;
            }
            f = f.min(1.0 - excess / span);
        }
        (f >= 0.0).then_some(f)
    }
}

/// 列ごとの縮め方。どのカードの下端も画面の内側に収める(収まる列は縮めない)。
/// 見出しの領域を除いても収まらない列は、見出しも含めて一様に縮める(カードを領域の外へ出さないことを優先する)。
fn squash_columns(c: &Ctx, fams: &[PageFam<'_>], packed: &Packed) -> Vec<Squash> {
    let limit = c.area.h - c.foot();
    let mut runs: Vec<Vec<(f32, f32)>> = vec![Vec::new(); packed.cols];
    let mut cards: Vec<Vec<(f32, f32)>> = vec![Vec::new(); packed.cols];
    for seg in &packed.segs {
        let col = seg.col.min(packed.cols.saturating_sub(1));
        // 見出しを挟まずに並ぶ単独カードの、最初と最後の上端。
        let mut solo: Option<(f32, f32)> = None;
        for piece in &seg.pieces {
            let f = fams[piece.fam].f;
            if !piece.framed {
                cards[col].push((piece.y, piece.h));
                solo = Some(solo.map_or((piece.y, piece.y), |(a, _)| (a, piece.y)));
                continue;
            }
            runs[col].extend(solo.take());
            let mut y = piece.y + (FAM_PAD + FAM_HEAD) * c.s;
            let (first, mut last) = (y, y);
            for e in &piece.entries {
                let h = c.entry_h(f, e);
                cards[col].push((y, h));
                // 「ほか N 件」の行は縮めない(直前のカードがかぶらないよう、区間は最後のメンバーまで)。
                if matches!(e, Entry::Member { .. }) {
                    last = y;
                }
                y += h + CHILD_GAP * c.s;
            }
            runs[col].push((first, last));
        }
        runs[col].extend(solo.take());
    }
    (0..packed.cols)
        .map(|col| {
            let mut sq = Squash {
                f: 1.0,
                runs: std::mem::take(&mut runs[col]),
            };
            sq.runs.retain(|&(a, b)| b > a);
            sq.f = sq.solve(&cards[col], limit).unwrap_or_else(|| {
                sq.runs = vec![(0.0, f32::MAX)];
                sq.solve(&cards[col], limit).unwrap_or(0.0)
            });
            sq
        })
        .collect()
}

/// 重ねた列で、上に別のカードが載って隠れるカードを `Strip` にする(`placed` は (ID, 列))。
fn mark_covered(out: &mut Layout, placed: &[(String, usize)]) {
    let mut by_col: HashMap<usize, Vec<&str>> = HashMap::new();
    for (id, col) in placed {
        by_col.entry(*col).or_default().push(id);
    }
    for ids in by_col.values_mut() {
        ids.sort_by_key(|id| out.cards[*id].z);
        for (i, id) in ids.iter().enumerate() {
            let r = out.cards[*id].rect;
            let covered = ids[i + 1..].iter().any(|other| {
                let o = out.cards[*other].rect;
                o.x < r.x + r.w - 1.0
                    && r.x < o.x + o.w - 1.0
                    && o.y < r.y + r.h - 1.0
                    && r.y < o.y + o.h - 1.0
            });
            if covered
                && let Some(slot) = out.cards.get_mut(*id)
                && slot.mode == CardMode::Full
            {
                slot.mode = CardMode::Strip;
            }
        }
    }
}

/// 詰めた結果を座標にする。`squash` は列ごとの縦の縮め方(`overlap_fit` だけが縮める列を渡す)。
fn finish(c: &Ctx, fams: &[PageFam<'_>], packed: Packed, squash: &[Squash]) -> Layout {
    let s = c.s;
    let mut out = Layout {
        scale: s,
        ..Layout::default()
    };
    let cols = packed.cols.clamp(1, c.max_cols);
    let lane_gap = LANE_GAP * s;
    let cw = ((c.area.w - (cols - 1) as f32 * lane_gap) / cols as f32)
        .min(MAX_W * s)
        .max(1.0);
    let used = cols as f32 * cw + (cols - 1) as f32 * lane_gap;
    let x0 = c.area.x + (c.area.w - used) / 2.0;
    let pad = LANE_PAD * s;
    let (fpad, fhead) = (FAM_PAD * s, FAM_HEAD * s);
    // 同じ組の何枠目か(列をまたぐ組の見出しを区別する)。
    let mut pieces_of: HashMap<(usize, usize), usize> = HashMap::new();
    // 重ねて置いた列のカード(ID と列)。手前に出す順の z を振り、隠れるものを `Strip` にする。
    let mut squashed: Vec<(String, usize)> = Vec::new();
    for (si, seg) in packed.segs.iter().enumerate() {
        let col = seg.col.min(cols - 1);
        let sq = squash.get(seg.col);
        let squashed_col = sq.is_some_and(Squash::squashed);
        let sy = |y: f32| c.area.y + sq.map_or(y, |q| q.at(y));
        let x = x0 + col as f32 * (cw + lane_gap);
        let last_in_col = !packed.segs[si + 1..].iter().any(|o| o.col == seg.col);
        let lane_top = sy(seg.top);
        let h = if last_in_col {
            c.area.y + c.area.h - lane_top
        } else {
            sy(seg.bottom) - lane_top
        };
        let lane_at = out.lanes.len();
        out.lanes.push(Lane {
            group: seg.project,
            rect: Rect {
                x: x - pad,
                y: lane_top,
                w: cw + pad * 2.0,
                h,
            },
            head: Rect {
                x,
                y: lane_top + 10.0 * s,
                w: cw,
                h: c.head() - 16.0 * s,
            },
            more: Vec::new(),
            cont: seg.cont,
        });
        // この枠の中身の下端。縮めた列では、重ねたカードが枠の下端を超えて伸びる。
        let mut content_bottom = lane_top + c.head();
        for piece in &seg.pieces {
            let fam = &fams[piece.fam];
            let f_ = fam.f;
            let top = sy(piece.y);
            if !piece.framed {
                let Entry::Member { m, .. } = piece.entries[0] else {
                    continue;
                };
                let mem = &f_.members[m];
                let z = if squashed_col {
                    squashed.push((mem.id.clone(), col));
                    tier_z(mem.tier) + squashed.len() as u32
                } else {
                    0
                };
                content_bottom = content_bottom.max(top + piece.h);
                out.cards.insert(
                    mem.id.clone(),
                    CardSlot {
                        rect: Rect {
                            x,
                            y: top,
                            w: cw,
                            h: piece.h,
                        },
                        mode: CardMode::Full,
                        z,
                        depth: 0,
                    },
                );
                continue;
            }
            let nth = pieces_of.entry((fam.project, fam.idx)).or_insert(0);
            let piece_no = *nth;
            *nth += 1;
            let mut shape = FamilyShape {
                project: fam.project,
                family: fam.idx,
                piece: piece_no,
                continued: piece.continued,
                orphan: f_.orphan_of.is_some(),
                root: f_.root_id().map(str::to_string),
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
                more_attention: 0,
                hidden_ids: Vec::new(),
            };
            let mut at = piece.y + fpad + fhead;
            let mut bottom = top + fpad + fhead;
            let mut placed_at: Vec<usize> = Vec::new();
            for e in &piece.entries {
                let h = c.entry_h(f_, e);
                let y = sy(at);
                bottom = bottom.max(y + h);
                match *e {
                    Entry::Member { m, row } => {
                        let mem = &f_.members[m];
                        let indent = mem.depth as f32 * INDENT * s;
                        let rect = Rect {
                            x: x + fpad + indent,
                            y,
                            w: cw - fpad * 2.0 - indent,
                            h,
                        };
                        let z = if squashed_col {
                            squashed.push((mem.id.clone(), col));
                            tier_z(mem.tier) + squashed.len() as u32
                        } else {
                            0
                        };
                        out.cards.insert(
                            mem.id.clone(),
                            CardSlot {
                                rect,
                                mode: if row { CardMode::Row } else { CardMode::Full },
                                z,
                                depth: mem.depth,
                            },
                        );
                        placed_at.push(m);
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
                at += h + CHILD_GAP * s;
            }
            if squashed_col {
                shape.rect.h = bottom + fpad - top;
            }
            content_bottom = content_bottom.max(shape.rect.y + shape.rect.h);
            // 親は、この枠に置いたメンバーの中にいるときだけ線の起点にする。
            for (sm, &m) in shape.members.iter_mut().zip(&placed_at) {
                sm.parent = f_.members[m]
                    .parent
                    .filter(|p| placed_at.contains(p))
                    .map(|p| f_.members[p].id.clone());
            }
            if shape.more.is_some() {
                shape.hidden_ids = f_
                    .members
                    .iter()
                    .filter(|m| m.unit.is_some_and(|u| fam.states[u] == UnitState::Hidden))
                    .map(|m| m.id.clone())
                    .chain(f_.cut_ids.iter().cloned())
                    .collect();
                shape.more_attention = f_.cut_attention;
            }
            out.families.push(shape);
        }
        if squashed_col {
            // 枠は中身を含み、最後の枠は列の下端まで伸ばす(画面の外へは出さない)。
            let area_bottom = c.area.y + c.area.h;
            let bottom = if last_in_col {
                area_bottom
            } else {
                (content_bottom + c.foot()).min(area_bottom)
            };
            out.lanes[lane_at].rect.h = (bottom - lane_top).max(0.0);
        }
    }
    if !squashed.is_empty() {
        mark_covered(&mut out, &squashed);
    }
    out
}

/// 重ねた列で手前に出す順(要確認が最も手前)。
fn tier_z(tier: Tier) -> u32 {
    match tier {
        Tier::Attention => 2 * TIER_Z,
        Tier::Running => TIER_Z,
        Tier::Quiet => 0,
    }
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
    /// 同じ組の何枠目か(0 から)。
    pub piece: usize,
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
    /// 「ほか N 件」のうち、要確認のスレッドの数(表示の予算で集約したときだけ 0 でない)。
    pub more_attention: usize,
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
    let pad = FAM_PAD * s;
    // 枠は、全メンバー(と「ほか N 件」)の現在位置を含む大きさにする。止まっているときは配置した枠そのもの。
    // 左右は各メンバーが配置の枠からどれだけ動いたかの最大・最小で、上下は現在位置の上端・下端に余白を足して決める。
    // 「ほか N 件」の行は面を持たないので、直前のメンバーの動きに付いていく。
    let more = shape.more.map(|(r, _)| {
        shape.members.last().map_or(r, |m| {
            let now = cur(m);
            Rect {
                x: r.x + now.x - m.rect.x,
                y: r.y + now.y - m.rect.y,
                ..r
            }
        })
    });
    let mut tracked: Vec<(Rect, Rect)> = shape.members.iter().map(|m| (m.rect, cur(m))).collect();
    if let (Some((rest, _)), Some(now)) = (shape.more, more) {
        tracked.push((rest, now));
    }
    let moved = tracked.iter().any(|(rest, now)| rest != now);
    let (frame, head) = if !moved {
        (shape.rect, shape.head)
    } else {
        let left = tracked
            .iter()
            .map(|(rest, now)| now.x - rest.x)
            .fold(f32::INFINITY, f32::min);
        let right = tracked
            .iter()
            .map(|(rest, now)| now.x + now.w - (rest.x + rest.w))
            .fold(f32::NEG_INFINITY, f32::max);
        let top = tracked
            .iter()
            .map(|(_, now)| now.y)
            .fold(f32::INFINITY, f32::min)
            - pad
            - FAM_HEAD * s;
        let bottom = tracked
            .iter()
            .map(|(_, now)| now.y + now.h)
            .fold(f32::NEG_INFINITY, f32::max)
            + pad;
        let x = shape.rect.x + left;
        let frame = Rect {
            x,
            y: top,
            w: (shape.rect.x + shape.rect.w + right - x).max(1.0),
            h: bottom - top,
        };
        let head = Rect {
            x: frame.x + pad,
            y: frame.y + pad,
            ..shape.head
        };
        (frame, head)
    };
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

/// 1 枚に収めるために scale を下げる 1 段ごとの倍率。
const SHRINK_STEP: f32 = 0.9;
/// scale を下げる下限(渡された scale に対する倍率)。カードの文字は `render_card` が 0.6 を下限に
/// 縮めるので、それより小さくしても文字は小さくならず、遠くから読めなくなるだけ。
const SHRINK_FLOOR: f32 = 0.6;

/// GLES の合成(`miharashi-gpu` の `compose.rs`)が 1 フレームに描く面の数とテクスチャの種類の上限。
/// 超えると描く順の前(枠・線、次にカード)から捨てられる。`miharashi-gpu` は Linux だけの依存なので値を写してあり、
/// Linux では `compose.rs` の定数と同じことをテストで確かめる。
pub const GPU_AGENT_DRAWS: usize = 2048;
pub const GPU_AGENT_TEXTURES: usize = 512;
/// 上限から差し引く余裕(枠・線・見出しの数え違いと、将来足す面のため)。
const DRAWS_RESERVE: usize = 64;
const TEXTURES_RESERVE: usize = 16;
/// 親子グループの枠の単色の面(塗り・枠線・要確認の枠線・接続線)。同じ色は 1 枚のテクスチャを共有する。
const SOLID_TEXTURES: usize = 4;

/// 1 つの配置が GLES の合成に渡す面の数(上限の見積もり)。`AgentScreen::decor_layers` の作り方に合わせてある。
/// 枠 1 つ = 塗り 1 + 枠線 4 + 接続線(メンバーと「ほか N 件」ごとに最大 2)+ 見出し 1 + 要確認の札 1 + 「ほか N 件」1。
/// 面(描く数)と、固有のテクスチャ(カード、見出し、札、「ほか」)の数を返す。
pub fn surface_cost(l: &Layout) -> SurfaceCost {
    let mut cost = SurfaceCost {
        draws: l.cards.len(),
        textures: l.cards.len() + SOLID_TEXTURES,
    };
    for f in &l.families {
        let more = usize::from(f.more.is_some());
        let labels = 2 + more;
        cost.draws += 5 + 2 * (f.members.len() + more) + labels;
        cost.textures += labels;
    }
    cost
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SurfaceCost {
    pub draws: usize,
    pub textures: usize,
}

impl SurfaceCost {
    /// 上限(余裕を引いた値)を超えた分を、減らすべきカードの数に直した値。収まっていれば 0。
    /// カード 1 枚を減らすと、面が少なくとも 1、テクスチャが 1 減る(枠の線が付くカードは面がもっと減る)。
    fn excess_cards(self) -> usize {
        let draws = self.draws.saturating_sub(GPU_AGENT_DRAWS - DRAWS_RESERVE);
        let textures = self
            .textures
            .saturating_sub(GPU_AGENT_TEXTURES - TEXTURES_RESERVE);
        textures.max(draws.div_ceil(3))
    }
}

/// 集約の順に並べた「ほか N 件」の候補(組の位置、メンバーの位置)。
/// 先頭ほど先に集約する: その他 → 実行中 → 要確認の順で、同じ段では重要でない順(`rank` が大きい順)。
/// 段は、自分と子孫のうち最も重要なものの段で決める(要確認の子を持つ親は、要確認の段で集約する)。
/// 子孫は必ず親より前に並ぶので、先頭からどこで切っても、残ったスレッドの親は残る。
/// 親(各組の先頭のメンバー)は候補に入れない。親のカードの無い置き場は無いため。
fn cut_order(projects: &[Vec<Family>]) -> Vec<(usize, usize, usize)> {
    let level = |t: Tier| match t {
        Tier::Attention => 0u8,
        Tier::Running => 1,
        Tier::Quiet => 2,
    };
    // (段, 子孫を含めた最も良い rank, 字下げ) と、(プロジェクト, 組, メンバー)の位置。
    type Key = (u8, usize, u8);
    type At = (usize, usize, usize);
    let mut keyed: Vec<(Key, At)> = Vec::new();
    for (p, fams) in projects.iter().enumerate() {
        for (i, f) in fams.iter().enumerate() {
            let mut own: Vec<(u8, usize)> =
                f.members.iter().map(|m| (level(m.tier), m.rank)).collect();
            for m in (1..f.members.len()).rev() {
                if let Some(parent) = f.members[m].parent {
                    let child = own[m];
                    own[parent] = (own[parent].0.min(child.0), own[parent].1.min(child.1));
                }
            }
            for (m, member) in f.members.iter().enumerate().skip(1) {
                keyed.push(((own[m].0, own[m].1, member.depth), (p, i, m)));
            }
        }
    }
    keyed.sort_unstable_by(|a, b| b.cmp(a));
    keyed.into_iter().map(|(_, at)| at).collect()
}

/// `cut` の先頭 `n` 件を「ほか N 件」に回した組(`projects` と同じ位置)。変わらない組はそのまま写す。
fn cut_families(
    projects: &[Vec<Family>],
    order: &[(usize, usize, usize)],
    n: usize,
) -> Vec<Vec<Family>> {
    let mut cut: HashMap<(usize, usize), Vec<usize>> = HashMap::new();
    for &(p, i, m) in &order[..n] {
        cut.entry((p, i)).or_default().push(m);
    }
    projects
        .iter()
        .enumerate()
        .map(|(p, fams)| {
            fams.iter()
                .enumerate()
                .map(|(i, f)| match cut.get(&(p, i)) {
                    None => f.clone(),
                    Some(gone) => cut_family(f, gone),
                })
                .collect()
        })
        .collect()
}

fn cut_family(f: &Family, gone: &[usize]) -> Family {
    let mut removed = vec![false; f.members.len()];
    for &m in gone {
        removed[m] = true;
    }
    let mut new_at = vec![usize::MAX; f.members.len()];
    let mut members: Vec<Member> = Vec::with_capacity(f.members.len() - gone.len());
    for (m, member) in f.members.iter().enumerate() {
        if removed[m] {
            continue;
        }
        new_at[m] = members.len();
        members.push(Member {
            parent: member.parent.map(|p| new_at[p]),
            ..member.clone()
        });
    }
    let mut out = f.clone();
    out.cut_ids = f.cut_ids.clone();
    out.cut_attention = f.cut_attention;
    for &m in gone {
        out.cut_ids.push(f.members[m].id.clone());
        out.cut_attention += usize::from(f.members[m].tier == Tier::Attention);
    }
    out.units = assign_units(&mut members, f.orphan_of.is_some());
    if f.orphan_of.is_some() {
        out.lead = members.iter().map(|m| m.rank).min().unwrap_or(f.lead);
    }
    out.members = members;
    out
}

/// 最初に集約するカードの数の見積もり。どの組も枠 1 つ(見出しと札の 2 面)で済む最も少ない場合の値なので、
/// 実際に必要な数より小さい。足りなければ `plan_board` が配置を測り直して増やす。
fn first_cut(projects: &[Vec<Family>], cuttable: usize) -> usize {
    let cards: usize = projects.iter().flatten().map(|f| f.members.len()).sum();
    let framed = projects.iter().flatten().filter(|f| f.framed()).count();
    let by_textures =
        (GPU_AGENT_TEXTURES - TEXTURES_RESERVE).saturating_sub(SOLID_TEXTURES + 2 * framed);
    let by_draws = (GPU_AGENT_DRAWS - DRAWS_RESERVE).saturating_sub(7 * framed) / 3;
    cards
        .saturating_sub(by_textures.min(by_draws))
        .min(cuttable)
}

/// 全プロジェクトの組を、常に 1 枚の配置にする。
/// `projects` はプロジェクトの表示順で、各プロジェクトの組は重要な順。収まらなければ次の順に縮める。
/// 1. 静かな単位を 1 行表示 → 全部 1 行 → 「ほか N 件」に隠す(`fit_page`)。
/// 2. 全部の組を 1 枚のまま、scale を `SHRINK_STEP` ずつ `SHRINK_FLOOR` まで下げて、1. をやり直す。
/// 3. それでも入らなければ、下限の scale で列を重ねる(`overlap_fit`)。
///
/// 重ねた配置は数百〜数千枚のカードを 1 枚に入れられるが、GLES の合成には面とテクスチャの上限がある
/// (`GPU_AGENT_DRAWS`、`GPU_AGENT_TEXTURES`)。配置が `surface_cost` で上限を超えるときは、
/// 重要でないスレッドから「ほか N 件」に回してやり直す(`cut_order`)。収まる盤面は何も省かない。
pub fn plan_board(area: Rect, scale: f32, projects: &[Vec<Family>]) -> Layout {
    if area.w <= 0.0 || area.h <= 0.0 || projects.iter().all(Vec::is_empty) {
        return Layout::default();
    }
    if projects.iter().flatten().all(|f| !f.framed()) {
        let groups: Vec<Vec<String>> = projects
            .iter()
            .map(|p| p.iter().map(|f| f.members[0].id.clone()).collect())
            .collect();
        return layout(area, scale, &groups);
    }
    let order = cut_order(projects);
    let mut cut = first_cut(projects, order.len());
    let mut rounds = 0u32;
    loop {
        let layout = if cut == 0 {
            plan_all(area, scale, projects)
        } else {
            plan_all(area, scale, &cut_families(projects, &order, cut))
        };
        let excess = surface_cost(&layout).excess_cards();
        if excess == 0 || cut >= order.len() {
            return layout;
        }
        // 超えた分のカードを減らして測り直す。配置し直すと枠が増えて面が増えることもあるので、
        // 3 回を過ぎたら進める幅を倍にしていき、必ず終わらせる。
        rounds += 1;
        let step = excess.max(1) << rounds.saturating_sub(3).min(16);
        cut = (cut + step).min(order.len());
    }
}

/// 全部の組を 1 枚に配置する(省かない)。
fn plan_all(area: Rect, scale: f32, projects: &[Vec<Family>]) -> Layout {
    let items: Vec<(usize, usize)> = projects
        .iter()
        .enumerate()
        .flat_map(|(p, fams)| (0..fams.len()).map(move |i| (p, i)))
        .collect();
    let floor = scale * SHRINK_FLOOR;
    let mut s = scale;
    loop {
        let max_cols =
            (((area.w + LANE_GAP * s) / (MIN_W * s + LANE_GAP * s)).floor() as usize).max(1);
        let c = Ctx { s, area, max_cols };
        let bases: Vec<Vec<Vec<UnitState>>> = projects
            .iter()
            .map(|fams| fams.iter().map(|f| base_states(&c, f)).collect())
            .collect();
        match fit_page(&c, projects, &bases, &items) {
            Ok(layout) => return layout,
            Err(fams) if s <= floor => return overlap_fit(&c, &fams),
            Err(_) => s = (s * SHRINK_STEP).max(floor),
        }
    }
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
        assert!(f[0].framed() && f[0].members.iter().any(|m| m.tier == Tier::Attention));
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
        let board = plan_board(AREA, 1.0, &projects);
        assert_eq!(board, layout(AREA, 1.0, &ids));
        assert!(board.families.is_empty());
    }

    #[test]
    fn a_family_fits_one_column_by_shrinking_quiet_children_to_rows() {
        let nodes = family_with(3, "");
        let projects = vec![fams(&nodes)];
        let l = &plan_board(AREA, 1.0, &projects);
        assert_eq!(l.cards.len(), nodes.len());
        assert_eq!(l.scale, 1.0, "収まるときは縮めない");
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
        let l = &plan_board(AREA, 1.0, &[fams(&nodes)]);
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
        let l = &plan_board(area, 1.0, &projects);
        assert_eq!(l.scale, 1.0, "静かな子の集約で収まるので縮めない");
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
    fn attention_children_are_never_hidden_or_shrunk_to_a_row() {
        let area = Rect { w: 400.0, ..AREA };
        let mut nodes = vec![node("p", None, Tier::Running)];
        nodes.extend((0..3).map(|i| node(&format!("a{i}"), Some("p"), Tier::Attention)));
        nodes.extend((0..12).map(|i| node(&format!("q{i}"), Some("p"), Tier::Quiet)));
        let l = plan_board(area, 1.0, &[fams(&nodes)]);
        assert_inside(&l, area);
        for i in 0..3 {
            let c = l.cards[&format!("a{i}")];
            assert_ne!(c.mode, CardMode::Row);
        }
        assert!(
            l.families
                .iter()
                .flat_map(|f| &f.hidden_ids)
                .all(|id| id.starts_with('q'))
        );
        let hidden: usize = l.families.iter().map(|f| f.hidden_ids.len()).sum();
        assert_eq!(l.cards.len() + hidden, nodes.len(), "欠落なし");
    }

    /// 親 1 + 子 `n` の組を持つプロジェクトを `g` 個。`tier` は子の段。
    fn crowd(g: usize, n: usize, tier: Tier) -> Vec<Vec<Family>> {
        (0..g)
            .map(|g| {
                let p = format!("g{g}");
                let mut nodes = vec![node(&p, None, Tier::Running)];
                nodes.extend((0..n).map(|i| node(&format!("{p}c{i}"), Some(&p), tier)));
                fams(&nodes)
            })
            .collect()
    }
    fn count(projects: &[Vec<Family>]) -> usize {
        projects.iter().flatten().map(|f| f.members.len()).sum()
    }
    fn hidden_in(l: &Layout) -> usize {
        l.families.iter().map(|f| f.hidden_ids.len()).sum()
    }

    #[test]
    fn when_nothing_fits_the_whole_board_shrinks_to_one_page() {
        // 狭い画面(400 幅)に、実行中の子を 3 人ずつ持つ組を 9 つ。承認待ちの子がいる組は最後に渡す。
        let area = Rect { w: 400.0, ..AREA };
        let mut projects = crowd(9, 3, Tier::Running);
        let mut nodes = vec![node("g8", None, Tier::Running)];
        nodes.extend((0..3).map(|i| node(&format!("g8c{i}"), Some("g8"), Tier::Attention)));
        projects[8] = fams(&nodes);
        let l = plan_board(area, 1.0, &projects);
        // 1 枚の中に、全スレッドが 1 回ずつ。どれも領域の内側。
        assert_eq!(l.cards.len(), 9 * 4);
        assert_eq!(hidden_in(&l), 0);
        assert_inside(&l, area);
        // 要確認のカードは、重なっても手前に出る(z が大きい)。
        let z = |id: &str| l.cards[id].z;
        assert!(z("g8c0") > z("g0c0"), "{} {}", z("g8c0"), z("g0c0"));
        // 組の枠もすべて 1 枚の配置に入っている。
        assert_eq!(l.families.len(), 9);
    }

    #[test]
    fn shrinking_the_scale_adds_columns_and_keeps_every_card_full() {
        // 1856 幅では、1.0 の最小幅 380 で 4 列、0.9 で 5 列。実行中の子が 6 人の組が 5 つ(縮めずに並べると 5 列が要る)。
        let area = Rect { w: 1856.0, ..AREA };
        let projects = crowd(5, 6, Tier::Running);
        let l = plan_board(area, 1.0, &projects);
        assert!(l.scale >= SHRINK_FLOOR && l.scale < 1.0, "{}", l.scale);
        assert_eq!(l.cards.len(), count(&projects));
        assert_eq!(hidden_in(&l), 0);
        assert!(l.cards.values().all(|c| c.mode == CardMode::Full));
        assert!(l.cards.values().all(|c| c.z == 0), "重ねていない");
        assert_inside(&l, area);
        // 下げた scale に合わせて、カードも小さい。
        assert!(l.cards["g0"].rect.h < CARD_H);
    }

    #[test]
    fn three_families_that_fit_one_page_stay_on_one_page_at_1920() {
        // PR #90 で見送った、特定の並び(親 3 組)で、1 枚に入る盤面を 2 ページに分けていた問題。
        let area = Rect { w: 1856.0, ..AREA };
        for kids in [3usize, 5, 7, 9] {
            let projects = crowd(3, kids, Tier::Running);
            let l = plan_board(area, 1.0, &projects);
            assert_eq!(l.cards.len(), count(&projects), "kids={kids}");
            assert_eq!(hidden_in(&l), 0, "kids={kids}");
            assert_inside(&l, area);
            if kids <= 3 {
                assert_eq!(l.scale, 1.0, "kids={kids}");
            }
        }
    }

    /// どのカードも表示領域の中にある。
    fn assert_inside(l: &Layout, area: Rect) {
        for (id, c) in &l.cards {
            assert!(
                c.rect.x >= area.x - 0.5
                    && c.rect.x + c.rect.w <= area.x + area.w + 0.5
                    && c.rect.y >= area.y - 0.5
                    && c.rect.y + c.rect.h <= area.y + area.h + 0.5,
                "{id} {:?} is outside {area:?}",
                c.rect
            );
        }
    }

    #[test]
    fn a_family_larger_than_the_screen_is_overlapped_on_one_page_with_every_card_inside() {
        let area = Rect { w: 400.0, ..AREA };
        let mut nodes = vec![node("p", None, Tier::Running)];
        nodes.extend((0..40).map(|i| node(&format!("c{i}"), Some("p"), Tier::Attention)));
        let l = plan_board(area, 1.0, &[fams(&nodes)]);
        assert_eq!(l.cards.len(), 41, "全カードが 1 枚に出る");
        assert_inside(&l, area);
        assert!((l.scale - SHRINK_FLOOR).abs() < 1e-6, "{}", l.scale);
        assert_eq!(hidden_in(&l), 0);
        // 重ねたので、隠れるカードは 1 行(Strip)になり、要確認は親より手前に出る。
        assert!(l.cards.values().any(|c| c.mode == CardMode::Strip));
        assert!(l.cards["c0"].z > l.cards["p"].z);
        assert!(l.cards["c39"].z > l.cards["c0"].z, "下のカードが手前");
        // 一番手前の(最後の)カードは、全体が見える。
        assert_eq!(l.cards["c39"].mode, CardMode::Full);
        // 親から数えて、列の上から順に並ぶ。
        assert!(l.cards["c0"].rect.y < l.cards["c20"].rect.y);
        assert!(l.cards["c20"].rect.y < l.cards["c39"].rect.y);
    }

    #[test]
    fn overlap_keeps_frames_lanes_and_hidden_anchors_for_many_projects() {
        // 16 のプロジェクトそれぞれに、要確認 5 人の組と静かな子 3 人。最小の scale でも 1 画面に入らない。
        let area = Rect { w: 1856.0, ..AREA };
        let projects: Vec<Vec<Family>> = (0..16)
            .map(|g| {
                let p = format!("g{g}");
                let mut nodes = vec![node(&p, None, Tier::Running)];
                nodes.extend((0..5).map(|i| node(&format!("{p}a{i}"), Some(&p), Tier::Attention)));
                nodes.extend((0..3).map(|i| node(&format!("{p}q{i}"), Some(&p), Tier::Quiet)));
                fams(&nodes)
            })
            .collect();
        let l = plan_board(area, 1.0, &projects);
        assert!((l.scale - SHRINK_FLOOR).abs() < 1e-6);
        assert_eq!(l.cards.len() + hidden_in(&l), count(&projects));
        assert_inside(&l, area);
        for g in 0..16 {
            for i in 0..5 {
                assert!(l.cards.contains_key(&format!("g{g}a{i}")), "g{g}a{i}");
            }
            // 隠した静かな子の置き場(親のカード)が残る。
            assert!(l.anchor(&format!("g{g}q2")).is_some());
        }
        assert_eq!(l.families.len(), 16);
        // 枠・プロジェクトの枠も領域の内側。
        for lane in &l.lanes {
            assert!(lane.rect.y >= area.y - 0.5);
            assert!(lane.rect.y + lane.rect.h <= area.y + area.h + 0.5);
        }
        for f in &l.families {
            assert!(f.rect.y >= area.y - 0.5, "{:?}", f.rect);
            assert!(
                f.rect.y + f.rect.h <= area.y + area.h + 40.0,
                "{:?}",
                f.rect
            );
        }
    }

    #[test]
    fn layout_work_stays_bounded_by_the_number_of_scale_steps() {
        // 2047 人の要確認の子。縮めても入らず、重ねて 1 枚にする。GLES の上限に収まらない分は「ほか N 件」にまとめる。
        // 時間は縮める段数と、予算で測り直す回数の倍数程度。
        let mut nodes = vec![node("p", None, Tier::Running)];
        nodes.extend((0..2047).map(|i| node(&format!("a{i}"), Some("p"), Tier::Attention)));
        let projects = [fams(&nodes)];
        let started = std::time::Instant::now();
        let l = plan_board(AREA, 1.0, &projects);
        let took = started.elapsed();
        assert_eq!(l.cards.len() + hidden_in(&l), 2048);
        assert_within_budget(&l);
        assert_inside(&l, AREA);
        assert!(took < std::time::Duration::from_secs(5), "{took:?}");
    }

    /// 配置が、GLES の合成に渡す面とテクスチャの上限(余裕を引いた値)に収まる。
    fn assert_within_budget(l: &Layout) {
        let cost = surface_cost(l);
        assert_eq!(cost.excess_cards(), 0, "{cost:?}");
        assert!(cost.draws <= GPU_AGENT_DRAWS && cost.textures <= GPU_AGENT_TEXTURES);
    }

    #[test]
    fn a_board_within_the_budget_keeps_every_card() {
        // 数百枚でも、上限に収まる盤面は何も「ほか N 件」に回さない。
        let nodes = family_with(0, "a")
            .into_iter()
            .chain((0..200).map(|i| node(&format!("c{i}"), Some("a"), Tier::Attention)))
            .collect::<Vec<_>>();
        let projects = [fams(&nodes)];
        let l = plan_board(AREA, 1.0, &projects);
        assert_eq!(l.cards.len(), count(&projects));
        assert!(l.families.iter().all(|f| f.more_attention == 0));
        assert_within_budget(&l);
    }

    #[test]
    fn over_the_budget_quiet_go_first_then_running_then_attention() {
        // 親 1 + 要確認 300 + 実行中 300 + その他 300。全部は GLES の上限に入らない。
        let mut nodes = vec![node("p", None, Tier::Running)];
        nodes.extend((0..300).map(|i| node(&format!("a{i}"), Some("p"), Tier::Attention)));
        nodes.extend((0..300).map(|i| node(&format!("r{i}"), Some("p"), Tier::Running)));
        nodes.extend((0..300).map(|i| node(&format!("q{i}"), Some("p"), Tier::Quiet)));
        let projects = [fams(&nodes)];
        let l = plan_board(AREA, 1.0, &projects);
        assert_within_budget(&l);
        assert_inside(&l, AREA);
        // 要確認は全部カードで出る。その他は 1 枚も残らず、実行中が代わりに削られる。
        assert!((0..300).all(|i| l.cards.contains_key(&format!("a{i}"))));
        assert!(!(0..300).any(|i| l.cards.contains_key(&format!("q{i}"))));
        let running = (0..300)
            .filter(|i| l.cards.contains_key(&format!("r{i}")))
            .count();
        assert!(running > 0 && running < 300, "{running}");
        // 実行中の中では重要な順(先頭)から残る。
        assert!(l.cards.contains_key("r0") && !l.cards.contains_key("r299"));
        // 件数が合う: 「ほか N 件」の N は、まとめたスレッドの数と同じ。要確認は含まない。
        assert_eq!(l.cards.len() + hidden_in(&l), 901);
        let more: usize = l
            .families
            .iter()
            .filter_map(|f| f.more)
            .map(|(_, n)| n)
            .sum();
        assert_eq!(more, hidden_in(&l));
        assert!(l.families.iter().all(|f| f.more_attention == 0));
        // まとめたスレッドの置き場は親のカード。
        assert_eq!(l.anchor("q299"), Some(l.cards["p"].rect));
    }

    #[test]
    fn attention_beyond_the_budget_is_counted_in_the_more_line() {
        // 3 プロジェクトに親 1 + 要確認 600 ずつ。要確認までまとめないと上限に入らない。
        let projects: Vec<Vec<Family>> = (0..3)
            .map(|g| {
                let p = format!("g{g}");
                let mut nodes = vec![node(&p, None, Tier::Running)];
                nodes
                    .extend((0..600).map(|i| node(&format!("{p}a{i}"), Some(&p), Tier::Attention)));
                fams(&nodes)
            })
            .collect();
        let l = plan_board(AREA, 1.0, &projects);
        assert_within_budget(&l);
        assert_inside(&l, AREA);
        let total = count(&projects);
        assert_eq!(l.cards.len() + hidden_in(&l), total);
        assert!(hidden_in(&l) > 0);
        assert_headings_uncovered(&l);
        // 集約したのは全部要確認。「ほか N 件」の N と、そのうちの要確認の数が合う。
        for f in &l.families {
            if let Some((_, n)) = f.more {
                assert_eq!(n, f.hidden_ids.len());
                assert_eq!(f.more_attention, n);
            }
        }
        let attention_hidden: usize = l.families.iter().map(|f| f.more_attention).sum();
        assert_eq!(attention_hidden, hidden_in(&l));
        // 親は必ず残り、重要な順(先頭)から残る。
        assert!((0..3).all(|g| l.cards.contains_key(&format!("g{g}"))));
        assert!((0..3).all(|g| l.cards.contains_key(&format!("g{g}a0"))));
        assert!(
            (0..3).all(|g| l.anchor(&format!("g{g}a599")) == Some(l.cards[&format!("g{g}")].rect))
        );
    }

    #[test]
    fn a_parent_with_an_attention_child_is_cut_with_the_attention_not_with_the_quiet() {
        // 親 P の子 C(静か)に孫 G(要確認)。C を先に消すと G の親がいなくなるので、G と同じ段まで残る。
        let mut nodes = vec![
            node("p", None, Tier::Running),
            node("c", Some("p"), Tier::Quiet),
        ];
        nodes.push(node("g", Some("c"), Tier::Attention));
        nodes.extend((0..700).map(|i| node(&format!("q{i}"), Some("p"), Tier::Quiet)));
        let l = plan_board(AREA, 1.0, &[fams(&nodes)]);
        assert_within_budget(&l);
        assert!(l.cards.contains_key("g"));
        assert!(l.cards.contains_key("c"), "孫の親は残る");
    }

    #[test]
    fn cutting_keeps_a_card_for_every_group() {
        // 親だけは集約しない。親子グループが数百あって予算を超えても、各組の親のカードは残る。
        let projects: Vec<Vec<Family>> = (0..40)
            .map(|g| {
                let p = format!("g{g}");
                let mut nodes = vec![node(&p, None, Tier::Running)];
                nodes.extend((0..30).map(|i| node(&format!("{p}a{i}"), Some(&p), Tier::Attention)));
                fams(&nodes)
            })
            .collect();
        let l = plan_board(AREA, 1.0, &projects);
        assert_within_budget(&l);
        assert!((0..40).all(|g| l.cards.contains_key(&format!("g{g}"))));
        assert_eq!(l.cards.len() + hidden_in(&l), count(&projects));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn gpu_limits_match_the_compositor() {
        assert_eq!(GPU_AGENT_DRAWS, miharashi_gpu::compose::AGENT_DRAWS);
        assert_eq!(GPU_AGENT_TEXTURES, miharashi_gpu::compose::AGENT_TEXTURES);
    }

    fn overlaps(a: Rect, b: Rect) -> bool {
        a.x < b.x + b.w - 0.5
            && b.x < a.x + a.w - 0.5
            && a.y < b.y + b.h - 0.5
            && b.y < a.y + a.h - 0.5
    }

    /// 組の見出しもプロジェクトの見出しも、どのカードにも覆われない。組の最初のカードは自分の見出しの下にある。
    fn assert_headings_uncovered(l: &Layout) {
        for shape in &l.families {
            for (id, card) in &l.cards {
                assert!(
                    !overlaps(shape.head, card.rect),
                    "{id} {:?} covers a group heading {:?}",
                    card.rect,
                    shape.head
                );
            }
            if let Some((more, _)) = shape.more {
                for (id, card) in &l.cards {
                    assert!(
                        !overlaps(more, card.rect),
                        "{id} {:?} covers a more line {more:?}",
                        card.rect
                    );
                }
            }
            if let Some(first) = shape.members.first() {
                assert!(
                    first.rect.y >= shape.head.y + shape.head.h - 0.5,
                    "{:?} starts above its heading {:?}",
                    first.rect,
                    shape.head
                );
            }
        }
        for lane in &l.lanes {
            for (id, card) in &l.cards {
                assert!(
                    !overlaps(lane.head, card.rect),
                    "{id} {:?} covers a project heading {:?}",
                    card.rect,
                    lane.head
                );
            }
            for shape in &l.families {
                assert!(!overlaps(lane.head, shape.head));
            }
        }
    }

    #[test]
    fn overlapped_columns_keep_every_heading_clear_of_the_cards() {
        // 16 のプロジェクトそれぞれに、親 1 + 要確認の子 5。1920x1080(領域 1856x788)で重ねる。
        for w in [1856.0, 2496.0] {
            let area = Rect { w, ..AREA };
            let projects: Vec<Vec<Family>> = (0..16)
                .map(|g| {
                    let p = format!("g{g}");
                    let mut nodes = vec![node(&p, None, Tier::Running)];
                    nodes.extend(
                        (0..5).map(|i| node(&format!("{p}a{i}"), Some(&p), Tier::Attention)),
                    );
                    fams(&nodes)
                })
                .collect();
            let l = plan_board(area, 1.0, &projects);
            assert!((l.scale - SHRINK_FLOOR).abs() < 1e-6, "重ねる盤面のはず");
            assert!(l.cards.values().any(|c| c.mode == CardMode::Strip));
            assert_headings_uncovered(&l);
            assert_inside(&l, area);
        }
    }

    #[test]
    fn headings_stay_clear_of_cards_when_groups_stack_in_one_column() {
        // 狭い領域(2 列)に、親子グループと単独スレッドが混ざって積まれ、重なる。
        let area = Rect { w: 800.0, ..AREA };
        let mut projects: Vec<Vec<Family>> = Vec::new();
        for g in 0..4 {
            let p = format!("g{g}");
            let mut nodes = vec![node(&p, None, Tier::Running)];
            nodes.extend((0..4).map(|i| node(&format!("{p}a{i}"), Some(&p), Tier::Attention)));
            nodes.extend((0..3).map(|i| node(&format!("{p}s{i}"), None, Tier::Running)));
            projects.push(fams(&nodes));
        }
        let l = plan_board(area, 1.0, &projects);
        assert!(l.cards.values().any(|c| c.mode == CardMode::Strip));
        assert_headings_uncovered(&l);
        assert_inside(&l, area);
        assert_eq!(l.cards.len() + hidden_in(&l), count(&projects));
    }

    #[test]
    fn a_column_whose_headings_alone_do_not_fit_still_keeps_the_cards_inside() {
        // 見出しだけで列の高さを超える極端な盤面では、見出しも一様に縮めて、カードは領域の内側に収める。
        let area = Rect {
            w: 400.0,
            h: 300.0,
            ..AREA
        };
        let projects: Vec<Vec<Family>> = (0..12)
            .map(|g| {
                let p = format!("g{g}");
                let mut nodes = vec![node(&p, None, Tier::Running)];
                nodes.extend((0..3).map(|i| node(&format!("{p}a{i}"), Some(&p), Tier::Attention)));
                fams(&nodes)
            })
            .collect();
        let l = plan_board(area, 1.0, &projects);
        assert_inside(&l, area);
        assert_eq!(l.cards.len() + hidden_in(&l), count(&projects));
    }

    #[test]
    fn twenty_attention_children_of_one_parent_are_all_reachable_at_1920() {
        // 1920x1080 相当(領域 1856x788)。親 1 + 承認待ちの子 20。
        let area = Rect { w: 1856.0, ..AREA };
        let mut nodes = vec![node("p", None, Tier::Running)];
        nodes.extend((0..20).map(|i| node(&format!("c{i}"), Some("p"), Tier::Attention)));
        let l = plan_board(area, 1.0, &[fams(&nodes)]);
        assert_eq!(l.cards.len(), 21);
        assert_inside(&l, area);
    }

    #[test]
    fn quiet_children_beyond_the_screen_are_counted_not_laid_out() {
        // 静かな子が 1 組に何千件あっても、並べるのは画面に入る分だけ。
        let mut nodes = vec![node("p", None, Tier::Running)];
        nodes.extend((0..2047).map(|i| node(&format!("q{i}"), Some("p"), Tier::Quiet)));
        let l = &plan_board(AREA, 1.0, &[fams(&nodes)]);
        assert_inside(l, AREA);
        let hidden: usize = l.families.iter().map(|f| f.hidden_ids.len()).sum();
        assert_eq!(l.cards.len() + hidden, 2048);
        assert!(l.cards.len() < 200, "{}", l.cards.len());
        // 静かな子の中では重要な順(先頭)から残る。
        assert!(l.cards.contains_key("q0"));
        assert!(!l.cards.contains_key("q2046"));
        assert_eq!(l.anchor("q2046"), Some(l.cards["p"].rect));
    }

    /// 親 `{prefix}p` に実行中の子 `running` 件と静かな子 `quiet` 件の組。
    fn project_with(prefix: &str, running: usize, quiet: usize) -> Vec<Family> {
        let p = format!("{prefix}p");
        let mut v = vec![node(&p, None, Tier::Running)];
        for i in 0..running {
            v.push(node(&format!("{prefix}r{i}"), Some(&p), Tier::Running));
        }
        for i in 0..quiet {
            v.push(node(&format!("{prefix}q{i}"), Some(&p), Tier::Quiet));
        }
        fams(&v)
    }

    fn ctx_for(area: Rect) -> Ctx {
        Ctx {
            s: 1.0,
            area,
            max_cols: (((area.w + LANE_GAP) / (MIN_W + LANE_GAP)).floor() as usize).max(1),
        }
    }

    /// 全部を 1 行表示にしてから、隠す数を 1 から順に確かめて、入る最小の数を求める(遅いが確実な基準)。
    fn brute_min_hidden(c: &Ctx, projects: &[Vec<Family>]) -> Option<usize> {
        let mut fams: Vec<PageFam> = Vec::new();
        for (project, list) in projects.iter().enumerate() {
            for (idx, f) in list.iter().enumerate() {
                fams.push(PageFam {
                    f,
                    project,
                    idx,
                    states: base_states(c, f)
                        .into_iter()
                        .map(|s| {
                            if s == UnitState::Card {
                                UnitState::Row
                            } else {
                                s
                            }
                        })
                        .collect(),
                });
            }
        }
        // 隠さなくても入るなら、fit_page は隠す段まで来ない。
        if try_pack(c, &fams).is_some() {
            return None;
        }
        let order = hide_order(&fams);
        let rows: Vec<Vec<UnitState>> = fams.iter().map(|f| f.states.clone()).collect();
        let found = min_hidden(c, &mut fams, &rows, &order).map(|(k, _)| k);
        let brute = (1..=order.len()).find(|&k| {
            hide(&mut fams, &rows, &order, k);
            try_pack(c, &fams).is_some()
        });
        assert_eq!(found, brute, "二分探索の結果が最小の数と違う");
        brute
    }

    #[test]
    fn hiding_stops_at_the_smallest_count_that_fits_even_when_fitting_is_not_monotone() {
        // 1920x1080 の盤面に、静かな子の多い 3 つのプロジェクト。隠す数が 10 で入り、11 で入らず、12 で入る形。
        let area = Rect {
            x: 32.0,
            y: 212.0,
            w: 1856.0,
            h: 788.0,
        };
        let projects = vec![
            project_with("a", 0, 12),
            project_with("b", 0, 18),
            project_with("c", 2, 4),
        ];
        let c = ctx_for(area);
        let brute = brute_min_hidden(&c, &projects).expect("隠せば入る");
        let board = plan_board(area, 1.0, &projects);
        assert_eq!(board.scale, 1.0);
        let hidden: usize = board.families.iter().map(|f| f.hidden_ids.len()).sum();
        let total: usize = projects.iter().flatten().map(|f| f.members.len()).sum();
        assert_eq!(board.cards.len() + hidden, total);
        // 静かな子は 1 件ずつが単位なので、隠したスレッドの数は隠した単位の数と同じ。
        assert_eq!(hidden, brute);
    }

    #[test]
    fn hiding_matches_the_brute_force_minimum_on_varied_boards() {
        let mut seed = 12345u64;
        let mut rnd = |n: u64| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) % n
        };
        let mut checked = 0;
        for w in [1856.0f32, 2496.0, 900.0, 400.0] {
            let area = Rect { w, ..AREA };
            let c = ctx_for(area);
            for _ in 0..300 {
                let projects: Vec<Vec<Family>> = (0..1 + rnd(8))
                    .map(|i| project_with(&format!("p{i}"), rnd(6) as usize, rnd(100) as usize))
                    .collect();
                // brute_min_hidden の中で、探索の結果が 1 つずつ確かめた最小の数と同じか確かめる。
                if brute_min_hidden(&c, &projects).is_some() {
                    checked += 1;
                }
            }
        }
        assert!(checked > 200, "{checked}");
    }

    /// 件数に対する配置計算の時間を測る。
    /// `cargo test --release -p miharashi-app family_scale -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn family_scale_bench() {
        let time = |name: &str, projects: &[Vec<Family>]| {
            let started = std::time::Instant::now();
            let board = plan_board(AREA, 1.0, projects);
            eprintln!(
                "bench {name}: {:?} scale={:.2} cards={}",
                started.elapsed(),
                board.scale,
                board.cards.len()
            );
        };
        for n in [250usize, 500, 1000, 2047] {
            let mut nodes = vec![node("p", None, Tier::Running)];
            nodes.extend((0..n).map(|i| node(&format!("q{i}"), Some("p"), Tier::Quiet)));
            time(&format!("quiet children={n}"), &[fams(&nodes)]);
        }
        let mut nodes = vec![node("p", None, Tier::Running)];
        nodes.extend((0..2047).map(|i| node(&format!("a{i}"), Some("p"), Tier::Attention)));
        time("attention children=2047", &[fams(&nodes)]);
        // 親 1 + 子 1 の小さな組が 1000。
        let mut nodes = Vec::new();
        for g in 0..1000 {
            let p = format!("g{g}");
            nodes.push(node(&p, None, Tier::Running));
            nodes.push(node(&format!("{p}c"), Some(&p), Tier::Quiet));
        }
        time("families=1000 (2 threads each)", &[fams(&nodes)]);
        let mut projects = Vec::new();
        for g in 0..64 {
            let mut nodes = Vec::new();
            for f in 0..16 {
                let p = format!("g{g}f{f}");
                nodes.push(node(&p, None, Tier::Running));
                nodes.extend((0..1).map(|i| node(&format!("{p}q{i}"), Some(&p), Tier::Quiet)));
            }
            projects.push(fams(&nodes));
        }
        time("projects=64 x families=16", &projects);
        // 親 1 + 静かな子 20 の組が 100(1 枚の中の「ほか N 件」の割り振りが重い形)。
        let mut nodes = Vec::new();
        for g in 0..100 {
            let p = format!("g{g}");
            nodes.push(node(&p, None, Tier::Running));
            nodes.extend((0..20).map(|i| node(&format!("{p}q{i}"), Some(&p), Tier::Quiet)));
        }
        time("families=100 x quiet children=20", &[fams(&nodes)]);
    }

    #[test]
    fn frame_follows_every_member_when_children_swap_places() {
        let nodes = family_with(1, "");
        let l = &plan_board(AREA, 1.0, &[fams(&nodes)]);
        let shape = &l.families[0];
        // 先頭の子(a)と末尾の子(q0)が入れ替わる途中: a は q0 の位置、q0 は a の位置にいる。
        let (a, q) = (l.cards["a"].rect, l.cards["q0"].rect);
        let pose = |id: &str| match id {
            "a" => Some(Rect {
                x: a.x - 300.0,
                ..q
            }),
            "q0" => Some(a),
            _ => None,
        };
        let d = decor(shape, 1.0, &pose);
        for id in ["p", "a", "r", "q0"] {
            let c = pose(id).unwrap_or(l.cards[id].rect);
            assert!(c.x >= d.frame.x - 0.01 && c.y >= d.frame.y - 0.01, "{id}");
            assert!(
                c.x + c.w <= d.frame.x + d.frame.w + 0.01
                    && c.y + c.h <= d.frame.y + d.frame.h + 0.01,
                "{id}"
            );
        }
        // 枠はメンバーの外へ無駄に広がらない: 上は親カードの上、見出しは枠の上端にある。
        let p = l.cards["p"].rect;
        assert!((d.frame.y - shape.rect.y).abs() < 0.01 && d.frame.y < p.y);
        assert!((d.head.y - (d.frame.y + FAM_PAD)).abs() < 0.01);
        assert!(d.frame.x < a.x - 300.0 - FAM_PAD + 0.01);
        // 止まっているときは配置した枠そのもの。
        let rest = decor(shape, 1.0, &|_| None);
        assert_eq!((rest.frame, rest.head), (shape.rect, shape.head));
    }

    #[test]
    fn orphan_groups_get_a_frame_without_a_parent_card() {
        let nodes = vec![
            node("o1", Some("gone"), Tier::Running),
            node("o2", Some("gone"), Tier::Quiet),
            node("s", None, Tier::Quiet),
        ];
        let l = &plan_board(AREA, 1.0, &[fams(&nodes)]);
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
        let l = &plan_board(AREA, 1.0, &[fams(&nodes)]);
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
