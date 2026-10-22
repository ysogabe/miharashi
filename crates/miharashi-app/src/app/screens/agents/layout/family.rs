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

/// 1 ページに載せる組。`states` は単位ごとの表示。
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
    let capacity = c.max_cols as f32 * (c.fresh() - c.overhead() + gap);
    let mut counted = false;
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

/// 1 ページ分(`items` は (プロジェクト, そのプロジェクトの組の位置))を詰める。
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
        return Ok(finish(c, &fams, packed));
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
        return Ok(finish(c, &fams, packed));
    }
    let order = hide_order(&fams);
    let rows: Vec<Vec<UnitState>> = fams.iter().map(|f| f.states.clone()).collect();
    match min_hidden(c, &mut fams, &rows, &order) {
        Some((k, packed)) => {
            hide(&mut fams, &rows, &order, k);
            Ok(finish(c, &fams, packed))
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

/// 1 画面に入らない組を、列の数ごとのページに分ける(2 ページ目以降の最初の枠は「続き」)。
fn split_pages(c: &Ctx, fams: &[PageFam<'_>]) -> Vec<Layout> {
    let packed = pack(c, fams, false, usize::MAX);
    let per = c.max_cols;
    let mut buckets: Vec<Vec<Seg>> = (0..packed.cols.div_ceil(per).max(1))
        .map(|_| Vec::new())
        .collect();
    for mut seg in packed.segs {
        let page = seg.col / per;
        seg.col %= per;
        buckets[page].push(seg);
    }
    buckets
        .into_iter()
        .map(|segs| {
            let cols = segs.iter().map(|s| s.col + 1).max().unwrap_or(0);
            finish(
                c,
                fams,
                Packed {
                    segs,
                    cols,
                    over: false,
                },
            )
        })
        .collect()
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
    // 同じ組の何枠目か(列をまたぐ組の見出しを区別する)。
    let mut pieces_of: HashMap<(usize, usize), usize> = HashMap::new();
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
            let nth = pieces_of.entry((fam.project, fam.idx)).or_insert(0);
            let piece_no = *nth;
            *nth += 1;
            let mut shape = FamilyShape {
                project: fam.project,
                family: fam.idx,
                piece: piece_no,
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
            let mut placed_at: Vec<usize> = Vec::new();
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
                y += h + CHILD_GAP * s;
            }
            // 親は、この枠に置いたメンバーの中にいるときだけ線の起点にする。
            for (sm, &m) in shape.members.iter_mut().zip(&placed_at) {
                sm.parent = f.members[m]
                    .parent
                    .filter(|p| placed_at.contains(p))
                    .map(|p| f.members[p].id.clone());
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
    /// 同じ組の何枠目か(0 から。ページが分かれても、そのページの中で数える)。
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
    let bases: Vec<Vec<Vec<UnitState>>> = projects
        .iter()
        .map(|fams| fams.iter().map(|f| base_states(&c, f)).collect())
        .collect();
    let mut order: Vec<(usize, usize)> = projects
        .iter()
        .enumerate()
        .flat_map(|(p, fams)| (0..fams.len()).map(move |i| (p, i)))
        .collect();
    order.sort_by_key(|&(p, i)| {
        let f = &projects[p][i];
        (!f.has_attention(), f.best(), p, i)
    });
    // 組を重要な順に足していき、入らなくなったら次のページへ。
    let mut pages: Vec<Layout> = Vec::new();
    let mut cur: Vec<(usize, usize)> = Vec::new();
    let mut cur_layout: Option<Layout> = None;
    for item in order {
        cur.push(item);
        let failed = match fit_page(&c, projects, &bases, &cur) {
            Ok(layout) => {
                cur_layout = Some(layout);
                continue;
            }
            Err(fams) => fams,
        };
        let alone = cur.len() == 1;
        cur.clear();
        pages.extend(cur_layout.take());
        if alone {
            // 1 組だけで画面に入らない。列の数ごとのページに分けて巡回する。
            pages.extend(split_pages(&c, &failed));
            continue;
        }
        match fit_page(&c, projects, &bases, &[item]) {
            Ok(layout) => {
                cur.push(item);
                cur_layout = Some(layout);
            }
            Err(fams) => pages.extend(split_pages(&c, &fams)),
        }
    }
    pages.extend(cur_layout);
    pages
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
    fn a_family_larger_than_the_screen_continues_on_more_pages_with_every_card_inside() {
        let area = Rect { w: 400.0, ..AREA };
        let mut nodes = vec![node("p", None, Tier::Running)];
        nodes.extend((0..40).map(|i| node(&format!("c{i}"), Some("p"), Tier::Attention)));
        let pages = plan_board(area, 1.0, &[fams(&nodes)]);
        assert!(pages.len() > 1, "{}", pages.len());
        let mut seen: Vec<&String> = pages.iter().flat_map(|l| l.cards.keys()).collect();
        seen.sort();
        let before = seen.len();
        seen.dedup();
        assert_eq!(before, seen.len(), "同じカードが 2 回出ない");
        assert_eq!(seen.len(), 41, "全カードがどこかのページに出る");
        for l in &pages {
            assert_inside(l, area);
            assert!(l.cards.values().all(|c| c.mode == CardMode::Full));
        }
        // 親は最初のページ、2 ページ目以降の最初の枠は「続き」。
        assert!(pages[0].cards.contains_key("p"));
        assert!(pages[1..].iter().all(|l| !l.cards.contains_key("p")));
        assert!(pages[1..].iter().all(|l| l.families[0].continued));
    }

    #[test]
    fn twenty_attention_children_of_one_parent_are_all_reachable_at_1920() {
        // 1920x1080 相当(領域 1856x788)。親 1 + 承認待ちの子 20。
        let area = Rect { w: 1856.0, ..AREA };
        let mut nodes = vec![node("p", None, Tier::Running)];
        nodes.extend((0..20).map(|i| node(&format!("c{i}"), Some("p"), Tier::Attention)));
        let pages = plan_board(area, 1.0, &[fams(&nodes)]);
        let mut seen: Vec<&String> = pages.iter().flat_map(|l| l.cards.keys()).collect();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), 21);
        for l in &pages {
            assert_inside(l, area);
        }
    }

    #[test]
    fn quiet_children_beyond_the_screen_are_counted_not_laid_out() {
        // 静かな子が 1 組に何千件あっても、並べるのは画面に入る分だけ。
        let mut nodes = vec![node("p", None, Tier::Running)];
        nodes.extend((0..2047).map(|i| node(&format!("q{i}"), Some("p"), Tier::Quiet)));
        let pages = plan_board(AREA, 1.0, &[fams(&nodes)]);
        assert_eq!(pages.len(), 1);
        let l = &pages[0];
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
        let pages = plan_board(area, 1.0, &projects);
        assert_eq!(pages.len(), 1);
        let hidden: usize = pages[0].families.iter().map(|f| f.hidden_ids.len()).sum();
        let total: usize = projects.iter().flatten().map(|f| f.members.len()).sum();
        assert_eq!(pages[0].cards.len() + hidden, total);
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
            let pages = plan_board(AREA, 1.0, projects);
            eprintln!(
                "bench {name}: {:?} pages={} cards(p1)={}",
                started.elapsed(),
                pages.len(),
                pages[0].cards.len()
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
        // 親 1 + 静かな子 20 の組が 100(1 ページ内の「ほか N 件」の割り振りが重い形)。
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
        let pages = plan_board(AREA, 1.0, &[fams(&nodes)]);
        let l = &pages[0];
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
