//! 目录树的摊平、折叠、当前行与搜索：界面只认「此刻看得见的这几行」。
//!
//! 这里一个 gpui 元素都没有（同 [`crate::ui::features::reader::model::chrome`]）：目录这一屏最容易错的地方全在
//! 「哪几行该出现、第几行是现在这一行」，而不在画法。把它单独放出来，宿主
//! `cargo test` 就能把整棵树的形状跑一遍，不必上真机数行。
//!
//! 三条规矩（前两条照 Reeden 的目录量出来，第三条是读者 2026-10-15 改的）：
//!
//! 1. **折叠的是孩子，不是自己**：一个分支折起来之后它那一行还在，chevron 还指着
//!    「点开有东西」。
//! 2. **搜索命中要带祖先**：只留命中那一行的话，读者看不出它挂在第几章下面。
//! 3. **打开时整棵树全折，只有当前章那一条链例外**：一进来先看「这本书分几大块」，
//!    但读者同时得知道「我在第几章的哪一节下面」——所以当前行通往根的每一级自动展开
//!    （[`TocView::reveal`]），其余分支照旧折着。这一条 2026-10-16 由读者改的：原来
//!    写的是「位置只靠滚动交代、不靠展开交代」，真机上用下来是要看见那一条链，而不是
//!    只把列表滚到一条折着的分支上。滚动照旧跟着走（[`landing_row`] 那一条现在通常
//!    就直接落在当前行上）。

use std::collections::{BTreeSet, HashMap};

use folio_core::content::format::TocEntry;

/// 树里的一条，摊平成一行用的投影。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TocRow {
    /// **前序编号**。折叠集、当前行、搜索都按它记，所以它必须与「看得见几行」无关
    /// （见 [`build`]）—— 读者手动展开的那几格在重画之后还在原位。
    pub ix: usize,
    /// 父节点的 `ix`。根上的是 `None`。祖先链与「带祖先的搜索」都靠它往上走。
    pub parent: Option<usize>,
    /// 第几层，只用来算缩进。
    pub depth: usize,
    pub title: String,
    /// 有没有下级（决定这一行画不画 chevron）。
    pub branch: bool,
    /// 这一条对上的章。`None` = 目录指向的位置不在章节表里（坏 href、或指向没进
    /// spine 的文件）：画出来让读者看见书里有这一节，但不给点。
    pub chapter: Option<usize>,
    /// 章内的锚点 `id`。`None` = 章首。
    ///
    /// 不在这里换算成章内偏移：偏移要等那一章排完才查得到（`Document::id_offsets`），
    /// 而这一层是纯数据，够不着排版结果。落点在跳转那一刻才定。
    pub fragment: Option<String>,
}

/// 整棵树摊平成一份**不带折叠**的行表（文档序 / 前序）。
///
/// `hrefs` 是「容器路径 → 章号」的表，由 [`crate::ui::features::reader::model::book::Book`] 从章节表建出来：
/// `TocEntry::href` 与 `Chapter::spine_href` 同形（都已解码、去 fragment），所以是
/// 一次查表而不是模糊匹配。
pub fn build(entries: &[TocEntry], hrefs: &HashMap<String, usize>) -> Vec<TocRow> {
    let mut rows = Vec::new();
    walk(entries, None, 0, hrefs, &mut rows);
    rows
}

fn walk(
    entries: &[TocEntry],
    parent: Option<usize>,
    depth: usize,
    hrefs: &HashMap<String, usize>,
    out: &mut Vec<TocRow>,
) {
    for entry in entries {
        let ix = out.len();
        // 标题原样进表：截断与「最多几行」是画的那一层按读者设的档决定的，
        // 在这一层截掉就没法随设置改了。
        out.push(TocRow {
            ix,
            parent,
            depth,
            title: entry.title.clone(),
            branch: !entry.children.is_empty(),
            // 页号那一格**优先**：定版式（PDF 的书签）的目录条对不上任何一条容器路径，
            // 落点直接就是页。可重排的那一路永远没有这一格，所以查表那条一步都没变。
            chapter: entry.page.or_else(|| hrefs.get(&entry.href).copied()),
            fragment: entry.fragment.clone(),
        });
        walk(&entry.children, Some(ix), depth + 1, hrefs, out);
    }
}

/// 看得见的行：`collapsed` 里每一个 `ix` 的子树整块让出去，**它自己那一行留着**。
///
/// 判据是「祖先里有没有被折的」而不是「自己在不在集合里」：折叠集记的是分支，
/// 而一个分支下面可能还套着另一个展开过的分支，只有往上走才不会漏掉那一层。
pub fn visible<'a>(rows: &'a [TocRow], collapsed: &BTreeSet<usize>) -> Vec<&'a TocRow> {
    rows.iter()
        .filter(|row| !hidden_by_ancestor(rows, row, collapsed))
        .collect()
}

/// 往上走到根，看有没有哪一级被折上了。
fn hidden_by_ancestor(rows: &[TocRow], row: &TocRow, collapsed: &BTreeSet<usize>) -> bool {
    let mut parent = row.parent;
    while let Some(ix) = parent {
        if collapsed.contains(&ix) {
            return true;
        }
        parent = rows.get(ix).and_then(|ancestor| ancestor.parent);
    }
    false
}

/// 整棵树的分支都折上：只留顶层那几条。
///
/// 这是打开那一屏的**底子**，不是最终形状：当前章那条的祖先链随后由 [`TocView::reveal`]
/// 单独展开（模块注释第三条）。分开两步是因为折叠集要能整份重置，而「跟着位置留一条链」
/// 只在开屏那一次做 —— 中途重做会把读者手动折的格冲掉。
pub fn fold_all(rows: &[TocRow]) -> BTreeSet<usize> {
    rows.iter()
        .filter(|row| row.branch)
        .map(|row| row.ix)
        .collect()
}

/// 打开这一屏该把列表停在哪儿：`current` 那一行自己，或它往上**第一条看得见**的祖先。
///
/// 接上 [`TocView::reveal`] 之后当前行通常本来就看得见，这一条走的是第一支；留着第二支
/// 是因为它不依赖调用顺序 —— 拿一份没 reveal 过的折叠集来问，也要给出一个能滚到的行，
/// 而不是 `None`（看不见当前行不等于丢了位置，最近的那条看得见的祖先同样在说「你在这块里」）。
pub fn landing_row(
    rows: &[TocRow],
    current: Option<usize>,
    collapsed: &BTreeSet<usize>,
) -> Option<usize> {
    let mut ix = current?;
    loop {
        let row = rows.get(ix)?;
        if !hidden_by_ancestor(rows, row, collapsed) {
            return Some(ix);
        }
        ix = row.parent?;
    }
}

/// `ix` 自己加上它通往根的每一级。
fn ancestors_including<'a>(rows: &'a [TocRow], ix: usize) -> impl Iterator<Item = usize> + 'a {
    let mut next = Some(ix);
    std::iter::from_fn(move || {
        let ix = next?;
        next = rows.get(ix).and_then(|row| row.parent);
        Some(ix)
    })
}

/// 当前章对应的那一条：树里**第一条**指向 `chapter` 的行。
///
/// 一个 spine 文件里挂着好几条小节（《未来简史》一章文件对十个 nav 条目）时取最靠前
/// 的那一条：读者从目录跳进这一章，落点就是章首，高亮该跟他跳到的那一行一致，
/// 而不是指到本章末尾那条小节。
pub fn row_of_chapter(rows: &[TocRow], chapter: usize) -> Option<usize> {
    rows.iter()
        .find(|row| row.chapter == Some(chapter))
        .map(|row| row.ix)
}

/// 当前位置那一行：指到 `chapter` 的那几条里，**读者已经走过的那一条中最靠后的**。
///
/// 为什么不能直接用 [`row_of_chapter`]（先序第一条）：一个 spine 文件常常挂着一整批小节
/// （《未来简史》第 8 章那一份 `text00007.html` 挂了「第一部分」+ 十条小节），先序第一条
/// 永远是那个**父级行**本身。拿它去展开祖先等于什么都没展开 —— 父级自己还折着，链断在
/// 最上面那一格，症状就是「读到『祖先的需求』这一节，目录却还是全折」。
///
/// 挑法：每条的起点 = 它 `fragment` 在 `offsets` 里的章内偏移，父级行没有 fragment 就是
/// 章首 0。取「起点 ≤ 读者当前位置」里起点最大的那一条，于是读者往下走时这一行会一节一节
/// 往后挪；一条都没走过（偏移还在章首之前）就落回父级行，那正是它该在的位置。
/// 起点相同的多条（同一锚点写了两遍）取先序第一条，也就是更浅的那一条。
pub fn position_row(
    rows: &[TocRow],
    chapter: usize,
    offset: usize,
    offsets: &HashMap<String, usize>,
) -> Option<usize> {
    // `Option`：锚点在这一章的偏移表里查不到时给 `None`，让下面跳过那一条而不是拿 0 冒充章首。
    let start = |row: &TocRow| match &row.fragment {
        Some(id) => offsets.get(id).copied(),
        None => Some(0),
    };
    let mut best: Option<(usize, usize)> = None;
    let mut first: Option<usize> = None;
    for row in rows.iter().filter(|row| row.chapter == Some(chapter)) {
        first.get_or_insert(row.ix);
        // 锚点查不到（作者写了个这一章里不存在的 id）的那一条不参与「走过哪一节」的比较，
        // 父级行那条兜底还在。
        let Some(at) = start(row) else { continue };
        if at > offset {
            continue;
        }
        // 严格大于才换：同一起点时留住更靠前的那一条。
        if best.is_none_or(|(best_at, _)| at > best_at) {
            best = Some((at, row.ix));
        }
    }
    best.map(|(_, ix)| ix).or(first)
}

/// 搜索：命中行**连同它们的祖先**留下，顺序不变。/// 搜索：命中行**连同它们的祖先**留下，顺序不变。
///
/// 空串（或只按着空格）等于不过滤 —— 读者清掉输入框要看到原样，而不是「什么都没有」。
pub fn search<'a>(rows: &'a [TocRow], query: &str) -> Vec<&'a TocRow> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return rows.iter().collect();
    }
    let hit: BTreeSet<usize> = rows
        .iter()
        .filter(|row| row.title.to_lowercase().contains(&needle))
        .map(|row| row.ix)
        .collect();
    // 命中的往上补齐，否则「第三部分」下面的某一条命中时，读者只看到一条没有上文的字。
    let keep: BTreeSet<usize> = hit
        .iter()
        .flat_map(|ix| ancestors_including(rows, *ix))
        .collect();
    rows.iter().filter(|row| keep.contains(&row.ix)).collect()
}

/// 「已读 N / M 章」：M 是全展开的条目数，N 是当前行在**全展开序列**里的序号（1 起）。
///
/// 按位置数而不是按访问记录数：读者跳着读也要知道「读到全书哪儿了」，
/// 而访问过哪些章是另一件事（行的浅色只表达它）。
pub fn read_count(rows: &[TocRow], current: Option<usize>) -> (usize, usize) {
    let total = rows.len();
    let read = match current {
        Some(ix) => ix + 1,
        None => 0,
    };
    (read, total)
}

/// 面板开着的那一份状态：整棵树 + 折叠集 + 搜索词。
///
/// 行表存在这里，而不是每帧从会话重取：折叠集记的是 `ix`，一旦树在面板开着的时候变了
/// （换书、重新解析），那些号就指到别的行上去。与树同生共死才不会出现这种事。
pub struct TocView {
    rows: Vec<TocRow>,
    collapsed: BTreeSet<usize>,
    query: String,
    /// 倒着摆：从最后一条往上读。只改**显示顺序**，`ix`、当前行、「已读 N」都不跟着翻。
    reversed: bool,
    /// 开屏那一次算出的当前行：**(算自哪一章, 那一行的 ix)**。
    ///
    /// 记下章号是因为它可能失效：宽档那条栏开着时读者还能翻页，章一变，这个号就该重算
    /// 而不是继续认（`current_ix` 里那条判据）。
    current: Option<(usize, usize)>,
}

impl TocView {
    /// 打开这一屏：整棵树的分支全折，只露顶层；搜索词与摆法回到默认。
    pub fn open(rows: Vec<TocRow>) -> Self {
        let collapsed = fold_all(&rows);
        Self {
            rows,
            collapsed,
            query: String::new(),
            reversed: false,
            current: None,
        }
    }

    /// 按章内偏移定一次当前行（判据见 [`position_row`]），并记下来给 `current_ix` 用。
    /// 返回定到的那一行，调用方（`toc_panel::ensure_tree`）顺手拿它展开祖先链。
    pub fn locate(
        &mut self,
        chapter: usize,
        offset: usize,
        offsets: &HashMap<String, usize>,
    ) -> Option<usize> {
        let ix = position_row(&self.rows, chapter, offset, offsets);
        self.current = ix.map(|ix| (chapter, ix));
        ix
    }

    /// 当前章对应的那一行的 `ix`（画标记条、算「已读 N」都要它）。
    ///
    /// 优先给开屏时按偏移定到的那一条（一节一节跟着走），章号对不上（栏开着时翻了页）
    /// 就退回「先序第一条」那一档，至少还在这一章里。
    pub fn current_ix(&self, chapter: usize) -> Option<usize> {
        match self.current {
            Some((at_chapter, ix)) if at_chapter == chapter => Some(ix),
            _ => row_of_chapter(&self.rows, chapter),
        }
    }

    /// 打开这一屏时列表该停在哪一行的 `ix`：当前章那一行，或它往上第一条看得见的祖先。
    /// 全折之后当前行多半藏在某个分支里，这一条给的就是那条看得见的分支。
    pub fn landing_ix(&self, chapter: usize) -> Option<usize> {
        landing_row(&self.rows, self.current_ix(chapter), &self.collapsed)
    }

    /// 那一行在**此刻看得见的那一串**里排第几 —— 虚拟列表按这个数滚。
    /// 折着看不见的那一行给 `None`：递一个看不见的下标进去，列表会滚到不存在的位置。
    pub fn visible_position(&self, ix: usize) -> Option<usize> {
        self.visible().iter().position(|row| row.ix == ix)
    }

    /// 这一枝现在是折着的还是展开的 —— 箭头朝哪边由它决定。
    pub fn is_collapsed(&self, ix: usize) -> bool {
        self.collapsed.contains(&ix)
    }

    pub fn set_query(&mut self, query: String) {
        self.query = query;
    }

    /// 把 `ix` 通往根的每一级展开：`ix` 自己折不折不动。
    ///
    /// 开屏那一次做（`toc_panel::ensure_tree` 里），阅读中途不反复做 —— 反复做会把读者
    /// 自己折上的分支又悄悄掀开。只展开**祖先链**不展开 `ix` 自己：读者要看到的是
    /// 「我现在挂在第几章的哪一节下面」，而不是被塞进去一整章的目录。
    pub fn reveal(&mut self, ix: usize) {
        for ancestor in ancestors_including(&self.rows, ix).skip(1) {
            self.collapsed.remove(&ancestor);
        }
    }

    /// 点 chevron：折上还是展开。叶子没有得折。
    ///
    /// 展开时把整条祖先链一起打开 —— 只把 `ix` 从折叠集里摘掉不够：父级还折着的话
    /// 点一下「展开」屏幕上什么都不出现，读者只会以为这一格是坏的。
    pub fn toggle(&mut self, ix: usize) {
        if !self.rows.get(ix).is_some_and(|row| row.branch) {
            return;
        }
        if !self.collapsed.remove(&ix) {
            self.collapsed.insert(ix);
            return;
        }
        for ancestor in ancestors_including(&self.rows, ix) {
            self.collapsed.remove(&ancestor);
        }
    }

    /// 现在该画哪几行。
    ///
    /// 搜索词非空时**忽略折叠**：命中的那条可能正埋在一个折起来的分支下面，
    /// 让读者搜到了却看不见，等于搜索是坏的。
    pub fn visible(&self) -> Vec<&TocRow> {
        let rows = if !self.query.trim().is_empty() {
            search(&self.rows, &self.query)
        } else {
            visible(&self.rows, &self.collapsed)
        };
        if self.reversed {
            return rows.into_iter().rev().collect();
        }
        rows
    }

    /// 一个分支都没折上 = 全展开。菜单上那颗「展开目录 / 折叠目录」按这个决定文案。
    pub fn all_open(&self) -> bool {
        self.collapsed.is_empty()
    }

    /// 全展开。
    pub fn expand_all(&mut self) {
        self.collapsed.clear();
    }

    /// 全折上：只留顶层那几条。与 [`Self::open`] 时那一份折叠同一个形状。
    pub fn collapse_all(&mut self) {
        self.collapsed = fold_all(&self.rows);
    }

    /// 要不要倒着摆。
    pub fn set_reversed(&mut self, reversed: bool) {
        self.reversed = reversed;
    }

    pub fn reversed(&self) -> bool {
        self.reversed
    }

    /// 「已读 N / M 章」那两个数。M 是整棵树的条目数，不是当前看得见的行数 ——
    /// 后者会随着折叠和搜索变来变去，而读者要的是全书的分量。
    pub fn read_count(&self, current: Option<usize>) -> (usize, usize) {
        read_count(&self.rows, current)
    }
}

#[cfg(test)]
mod tests {
    //! 这些是目录这一屏唯一能在宿主上跑完的部分。曾经想直接在渲染里验，
    //! 结果「折叠集把当前行折掉了」这种错在屏幕上只表现为少了一行。

    use super::*;

    fn entry(title: &str, href: &str, children: Vec<TocEntry>) -> TocEntry {
        TocEntry {
            title: title.into(),
            href: href.into(),
            fragment: None,
            page: None,
            children,
        }
    }

    fn entry_at(title: &str, href: &str, fragment: &str) -> TocEntry {
        TocEntry {
            title: title.into(),
            href: href.into(),
            fragment: Some(fragment.into()),
            page: None,
            children: Vec::new(),
        }
    }

    /// 一本三层书：扉页 / 第1章（两小节，其中一节带 fragment）/ 第2章 / 版权页。
    /// 第1章那一节故意指到不在 spine 里的文件，用来验「画出来但不给点」。
    fn book() -> (Vec<TocEntry>, HashMap<String, usize>) {
        let entries = vec![
            entry("扉页", "cover.xhtml", vec![]),
            entry(
                "第1章",
                "chap1.xhtml",
                vec![
                    entry("第一节", "chap1.xhtml", vec![]),
                    entry_at("第二节", "chap1.xhtml", "sec2"),
                    entry("丢的那节", "gone.xhtml", vec![]),
                ],
            ),
            entry("第2章", "chap2.xhtml", vec![]),
            entry("版权页", "colophon.xhtml", vec![]),
        ];
        let hrefs = HashMap::from([
            ("cover.xhtml".to_string(), 0usize),
            ("chap1.xhtml".to_string(), 1),
            ("chap2.xhtml".to_string(), 2),
            ("colophon.xhtml".to_string(), 3),
        ]);
        (entries, hrefs)
    }

    #[test]
    fn the_flat_table_is_preorder_with_depth_and_parents() {
        let (entries, hrefs) = book();
        let rows = build(&entries, &hrefs);
        let shape: Vec<_> = rows
            .iter()
            .map(|row| (row.ix, row.depth, row.parent, row.title.as_str()))
            .collect();
        assert_eq!(
            shape,
            vec![
                (0, 0, None, "扉页"),
                (1, 0, None, "第1章"),
                (2, 1, Some(1), "第一节"),
                (3, 1, Some(1), "第二节"),
                (4, 1, Some(1), "丢的那节"),
                (5, 0, None, "第2章"),
                (6, 0, None, "版权页"),
            ]
        );
        assert!(rows[1].branch, "第1章有下级，才画 chevron");
        assert!(!rows[0].branch, "扉页是叶子");
        assert_eq!(rows[3].fragment.as_deref(), Some("sec2"));
        assert_eq!(rows[4].chapter, None, "指不到章节表的那条要留着，但不可点");
    }

    /// 前序编号必须与折没折无关：折叠集记的就是这些号，号一动读者手动展开的那几格
    /// 就会跑到别的行上去。全折之后剩下的行仍带着它们原来的号而不是重新从 0 数。
    #[test]
    fn the_numbering_does_not_shift_when_branches_collapse() {
        let (entries, hrefs) = book();
        let rows = build(&entries, &hrefs);
        let collapsed = fold_all(&rows);
        let shown: Vec<_> = visible(&rows, &collapsed)
            .iter()
            .map(|row| row.ix)
            .collect();
        assert_eq!(shown, vec![0, 1, 5, 6]);
        // 按「看得见的第几行」重新发号会得到 [0,1,2,3] —— 那正是这个测试要挡住的错：
        // 折叠集会指到别的行上去。
        assert_ne!(shown, (0..shown.len()).collect::<Vec<_>>());
    }

    /// 打开这一屏：不管读到哪一章，看见的都是顶层那一排。
    ///
    /// 这是改掉的那条旧规矩 —— 从前打开会把通往当前章那条链展开，读者一进来就被塞进
    /// 第三层里。现在位置靠滚动交代（见下面两条落点的门），不靠展开交代。
    #[test]
    fn the_tree_opens_folded_to_the_top_level() {
        let (entries, hrefs) = book();
        for chapter in [0, 1, 2, 3] {
            let view = TocView::open(build(&entries, &hrefs));
            let shown: Vec<_> = view
                .visible()
                .iter()
                .map(|row| row.title.as_str())
                .collect();
            assert_eq!(
                shown,
                vec!["扉页", "第1章", "第2章", "版权页"],
                "当前在第 {chapter} 章时打开也该只露顶层"
            );
            assert_eq!(view.current_ix(chapter).is_some(), chapter < 4);
        }
    }

    /// 当前行看不见时，落点是它往上第一条看得见的祖先；本来就看得见时就是它自己。
    /// 一个 spine 文件挂着一整批小节的那本书（《未来简史》的形状）：当前行要跟着读者
    /// 走到哪一节挪，而不是永远钉在那个父级行上。
    ///
    /// 这条钉的是「展开祖先没反应」那个真 bug：先序第一条永远是父级行，拿它去展开祖先
    /// 等于什么都没展开（父级自己还折着）。所以先验挑行，再验挑对了之后链能不能露出来。
    #[test]
    fn the_current_row_follows_the_section_being_read() {
        let entries = vec![entry(
            "第一部分",
            "part.xhtml",
            vec![
                entry_at("祖先的需求", "part.xhtml", "anc"),
                entry_at("神性的火花", "part.xhtml", "spark"),
            ],
        )];
        let hrefs = HashMap::from([("part.xhtml".to_string(), 7usize)]);
        let rows = build(&entries, &hrefs);
        // 章内偏移是排版产物，这里直接给一张表当那份产物。
        let offsets = HashMap::from([
            ("anc".to_string(), 100usize),
            ("spark".to_string(), 900usize),
        ]);

        // 还没走过第一节：落回父级行（章首），那是它该在的位置。
        assert_eq!(position_row(&rows, 7, 40, &offsets), Some(0));
        assert_eq!(
            position_row(&rows, 7, 300, &offsets),
            Some(1),
            "走进第一节了"
        );
        assert_eq!(
            position_row(&rows, 7, 1_200, &offsets),
            Some(2),
            "走进第二节了"
        );
        // 这一章根本没有的行：不给当前行，而不是乱指一条。
        assert_eq!(position_row(&rows, 8, 300, &offsets), None);
        // 锚点查不到（作者写了个这一章里不存在的 id）不许把那条当「已经走过」。
        let broken = HashMap::from([("anc".to_string(), 100usize)]);
        assert_eq!(position_row(&rows, 7, 5_000, &broken), Some(1));

        // 挑对了行之后，链才露得出来：读者在第一节下面，父级要展开。
        let mut toc = TocView::open(rows.clone());
        assert_eq!(toc.locate(7, 300, &offsets), Some(1));
        toc.reveal(1);
        assert!(!toc.is_collapsed(0), "父级那一枝要展开");
        assert!(
            toc.visible_position(1).is_some(),
            "读者正读的那一节要看得见"
        );
        assert_eq!(toc.current_ix(7), Some(1), "标记条与「已读 N」读的是同一行");
        // 章号对不上（那条栏开着时翻了页）就不认这份缓存，退回按章号现算。
        assert_eq!(toc.current_ix(9), None);
    }

    /// 开屏那一次：当前章那条埋在多深的分支里都要露出来，其余分支照旧折着。
    ///
    /// 这条钉的是「跟着位置留一条链」。判据不能只看当前行在不在 `visible()` 里 ——
    /// 把整棵树都展开也能过，所以要同时断「别的那条分支还折着」。
    #[test]
    fn opening_reveals_the_ancestors_of_the_current_row_only() {
        let (entries, hrefs) = book();
        let rows = build(&entries, &hrefs);
        // 当前章是 chap1（第1章），树里第一条指到它的是「第1章」那一行本身；
        // 拿它最里那条小节当当前位置来验，才是「祖先链」这件事。
        let deep = rows
            .iter()
            .find(|row| row.title == "第二节")
            .expect("夹具里有那一行")
            .ix;
        let mut toc = TocView::open(rows.clone());
        // 全折的底子：当前那一行现在看不见。
        assert!(toc.landing_ix(1).is_some());
        assert!(
            toc.visible_position(deep).is_none(),
            "没 reveal 之前那条应该还折着"
        );

        toc.reveal(deep);
        assert!(
            toc.visible_position(deep).is_some(),
            "祖先链展开后当前行要看得见"
        );
        assert!(!toc.is_collapsed(1), "第1章那一枝要展开");
        // 别处的分支不许被顺手掀开：扉页与版权页本来就没孩子，
        // 拿有孩子的比是比不出来的，所以这里断「除第1章之外没有分支被展开」。
        let opened: Vec<_> = rows
            .iter()
            .filter(|row| row.branch && !toc.is_collapsed(row.ix))
            .map(|row| row.title.as_str())
            .collect();
        assert_eq!(opened, vec!["第1章"], "只留当前行那一条链");
    }

    #[test]
    fn the_landing_row_is_the_nearest_visible_ancestor() {
        // 三层：第一部分 → 第1章 → 第1节，只有顶层那条看得见。
        let entries = vec![entry(
            "第一部分",
            "p1.xhtml",
            vec![entry(
                "第1章",
                "c1.xhtml",
                vec![entry_at("第1节", "c1.xhtml", "s1")],
            )],
        )];
        let hrefs = HashMap::from([("p1.xhtml".to_string(), 0usize)]);
        let rows = build(&entries, &hrefs);
        let collapsed = fold_all(&rows);
        assert_eq!(rows.len(), 3);

        // 埋得最深的那一条（第1节）：往上走两级才碰到看得见的那一行。
        assert_eq!(landing_row(&rows, Some(2), &collapsed), Some(0));
        // 顶层那条自己看得见：落点就是它，不多走。
        assert_eq!(landing_row(&rows, Some(0), &collapsed), Some(0));
        // 对不上任何一条（定版式、或当前章没进目录）时没有落点可滚。
        assert_eq!(landing_row(&rows, None, &collapsed), None);
    }

    /// 滚过去用的是「看得见的那一串里的第几行」，不是前序编号：全折时顶层某条的前序号
    /// 可能是 5，而它在可见的那四条里只排第 2 —— 拿 5 去滚就滚到不存在的行上。
    #[test]
    fn the_landing_index_counts_only_visible_rows() {
        let (entries, hrefs) = book();
        let rows = build(&entries, &hrefs);
        let view = TocView::open(rows);
        let ix = view.landing_ix(2).expect("第2章 在树里");
        assert_eq!(ix, 5, "记的仍是前序号");
        assert_eq!(
            view.visible_position(ix),
            Some(2),
            "看得见的四条里它排第三（下标 2）"
        );
        assert_eq!(
            view.visible_position(3),
            None,
            "折着看不见的那一行没有位置，不能拿去滚"
        );
    }

    /// 折叠的是孩子不是自己：分支折上之后它那一行仍然在，读者才知道还能点开。
    #[test]
    fn a_collapsed_branch_keeps_its_own_row() {
        let (entries, hrefs) = book();
        let rows = build(&entries, &hrefs);
        let collapsed = BTreeSet::from([1]);
        let shown: Vec<_> = visible(&rows, &collapsed)
            .iter()
            .map(|row| row.ix)
            .collect();
        assert!(shown.contains(&1), "第1章 自己该还在");
        assert!(!shown.contains(&2), "它的孩子一个都不该在");
    }

    #[test]
    fn searching_keeps_the_ancestors_of_a_hit() {
        let (entries, hrefs) = book();
        let rows = build(&entries, &hrefs);
        let hits: Vec<_> = search(&rows, "那节")
            .iter()
            .map(|row| row.title.as_str())
            .collect();
        assert_eq!(hits, vec!["第1章", "丢的那节"], "命中那条要带着它爹");

        // 大小写与首尾空格不该把搜索变成另一回事。
        assert_eq!(search(&rows, "  扉页  ").len(), 1);
        // 空串是「没在搜」，不是「什么都没搜到」。
        assert_eq!(search(&rows, "").len(), rows.len());
        assert_eq!(search(&rows, "   ").len(), rows.len());
        assert!(search(&rows, "没有这一节").is_empty());
    }

    #[test]
    fn the_read_count_counts_position_not_visits() {
        let (entries, hrefs) = book();
        let rows = build(&entries, &hrefs);
        assert_eq!(read_count(&rows, row_of_chapter(&rows, 1)), (2, 7));
        assert_eq!(read_count(&rows, row_of_chapter(&rows, 2)), (6, 7));
        assert_eq!(read_count(&rows, None), (0, 7), "没读过与读了一点是两件事");
    }

    /// 面板那份状态：打开就是全折的那一份，折上再点开要回到原样。
    #[test]
    fn the_panel_opens_folded_and_toggles_back_and_forth() {
        let (entries, hrefs) = book();
        let rows = build(&entries, &hrefs);
        let mut view = TocView::open(rows);
        // 拿到 owned 的标题而不是 `&str`：`visible()` 借的是 `view`，而下面还要改它。
        let folded: Vec<String> = view.visible().iter().map(|row| row.title.clone()).collect();
        assert_eq!(
            folded,
            vec!["扉页", "第1章", "第2章", "版权页"],
            "打开时只露顶层"
        );

        // 点开设第1章：它的三条小节要真的出来 —— 只把 `ix` 摘出折叠集是不够的，
        // 这一条测的就是「点了没反应」那个错。
        view.toggle(1);
        let opened: Vec<usize> = view.visible().iter().map(|row| row.ix).collect();
        assert_eq!(opened, vec![0, 1, 2, 3, 4, 5, 6]);
        // 再点一下折回去，回到打开时那一份。
        view.toggle(1);
        let back: Vec<String> = view.visible().iter().map(|row| row.title.clone()).collect();
        assert_eq!(back, folded);
        // 叶子没有得折：点它不该改变任何东西。
        view.toggle(0);
        let still: Vec<String> = view.visible().iter().map(|row| row.title.clone()).collect();
        assert_eq!(still, folded);
    }

    /// 搜索压倒折叠：命中的那条正埋在一个折起来的分支下面时也必须看得见。
    #[test]
    fn a_search_shows_hits_that_are_currently_folded_away() {
        let (entries, hrefs) = book();
        let rows = build(&entries, &hrefs);
        let mut view = TocView::open(rows);
        assert!(!view.visible().iter().any(|row| row.title == "第二节"));

        view.set_query("第二节".into());
        let hits: Vec<_> = view
            .visible()
            .iter()
            .map(|row| row.title.as_str())
            .collect();
        assert_eq!(
            hits,
            vec!["第1章", "第二节"],
            "命中的那条要连着它的祖先一起出来，否则读者看不出这条挂在第几章下面"
        );

        // 清空搜索词回到**折叠态**，不是回到全展开：读者手动折起来的东西不该被搜一次就冲掉。
        view.set_query(String::new());
        assert!(!view.visible().iter().any(|row| row.title == "第二节"));
    }

    /// 倒着摆只改显示顺序：`ix` 是前序编号，折叠集、当前行、「已读 N」全记在它上面，
    /// 跟着一起翻就会在读者倒过来的那一屏上指错行。
    #[test]
    fn reversing_flips_only_the_display_order() {
        let (entries, hrefs) = book();
        let rows = build(&entries, &hrefs);
        let mut view = TocView::open(rows);
        let forward: Vec<usize> = view.visible().iter().map(|row| row.ix).collect();
        view.set_reversed(true);
        let backward: Vec<usize> = view.visible().iter().map(|row| row.ix).collect();
        assert_eq!(backward, forward.into_iter().rev().collect::<Vec<_>>());
        assert_eq!(view.current_ix(2), Some(5), "当前行认的还是同一条");
        assert_eq!(
            view.read_count(view.current_ix(2)),
            (6, 7),
            "「已读 6 / 7 章」与摆法无关"
        );
    }

    /// 「展开目录 / 折叠目录」那一颗的两端来回摆得开，而打开时就在**折叠那一端** ——
    /// 那颗按钮的文案按 `all_open()` 决定，起点若变了，它第一下就会写成反话。
    #[test]
    fn expanding_and_collapsing_all_reaches_both_ends() {
        let (entries, hrefs) = book();
        let rows = build(&entries, &hrefs);
        let mut view = TocView::open(rows);
        assert!(!view.all_open(), "打开时就是全折的那一端");
        view.expand_all();
        assert!(view.all_open());
        assert_eq!(view.visible().len(), 7, "全展开是整棵树");
        view.collapse_all();
        assert!(!view.all_open());
        let top: Vec<&str> = view
            .visible()
            .iter()
            .map(|row| row.title.as_str())
            .collect();
        assert_eq!(
            top,
            vec!["扉页", "第1章", "第2章", "版权页"],
            "全折上只剩顶层"
        );
    }
}
