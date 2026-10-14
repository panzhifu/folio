//! 目录树的摊平、折叠、当前行与搜索：界面只认「此刻看得见的这几行」。
//!
//! 这里一个 gpui 元素都没有（同 [`super::chrome`]）：目录这一屏最容易错的地方全在
//! 「哪几行该出现、第几行是现在这一行」，而不在画法。把它单独放出来，宿主
//! `cargo test` 就能把整棵树的形状跑一遍，不必上真机数行。
//!
//! 形状照 Reeden 的目录量出来的三条规矩（见 `plans/facts.md` 那条「树跟着位置走」）：
//!
//! 1. **树的状态跟着阅读位置走**：打开时只把通往当前章的那条链展开，其余分支折起。
//! 2. **折叠的是孩子，不是自己**：一个分支折起来之后它那一行还在，chevron 还指着
//!    「点开有东西」。
//! 3. **搜索命中要带祖先**：只留命中那一行的话，读者看不出它挂在第几章下面。

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
/// `hrefs` 是「容器路径 → 章号」的表，由 [`super::book::Book`] 从章节表建出来：
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
            chapter: hrefs.get(&entry.href).copied(),
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

/// 除了 `keep` 这一行通往根的那条链，其余**分支**都折上。
///
/// `keep` 是 `None`（这本书没有目录、或当前章对不上任何一条）时全折：与其给一堵
/// 展开到第三层的墙，不如只露顶层。
pub fn collapse_except(rows: &[TocRow], keep: Option<usize>) -> BTreeSet<usize> {
    let open: BTreeSet<usize> = match keep {
        Some(ix) => ancestors_including(rows, ix).collect(),
        None => BTreeSet::new(),
    };
    rows.iter()
        .filter(|row| row.branch && !open.contains(&row.ix))
        .map(|row| row.ix)
        .collect()
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

/// 搜索：命中行**连同它们的祖先**留下，顺序不变。
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
}

impl TocView {
    /// 打开面板：折叠集按「只留通往 `current` 的那条链」算，于是树跟着阅读位置走。
    pub fn open(rows: Vec<TocRow>, current: Option<usize>) -> Self {
        let collapsed = collapse_except(&rows, current);
        Self {
            rows,
            collapsed,
            query: String::new(),
            reversed: false,
        }
    }

    /// 当前章对应的那一行的 `ix`（画标记条、算「已读 N」都要它）。
    pub fn current_ix(&self, chapter: usize) -> Option<usize> {
        row_of_chapter(&self.rows, chapter)
    }

    /// 这一枝现在是折着的还是展开的 —— 箭头朝哪边由它决定。
    pub fn is_collapsed(&self, ix: usize) -> bool {
        self.collapsed.contains(&ix)
    }

    pub fn set_query(&mut self, query: String) {
        self.query = query;
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

    /// 全折上：只留顶层那几条。
    ///
    /// 与 [`collapse_except`] 那条「打开时跟着阅读位置」不同 —— 那是读者一进来看到的样子，
    /// 这一条是他明确要「把树收起来」，不该再给他留一条展开着的分支。
    pub fn collapse_all(&mut self) {
        self.collapsed = self
            .rows
            .iter()
            .filter(|row| row.branch)
            .map(|row| row.ix)
            .collect();
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
            children,
        }
    }

    fn entry_at(title: &str, href: &str, fragment: &str) -> TocEntry {
        TocEntry {
            title: title.into(),
            href: href.into(),
            fragment: Some(fragment.into()),
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
    /// 就会跑到别的行上去。当前章在顶层的「第2章」时，第1章 那三条小节全折，
    /// 剩下的行仍带着它们原来的号而不是重新从 0 数。
    #[test]
    fn the_numbering_does_not_shift_when_branches_collapse() {
        let (entries, hrefs) = book();
        let rows = build(&entries, &hrefs);
        let collapsed = collapse_except(&rows, row_of_chapter(&rows, 2));
        let shown: Vec<_> = visible(&rows, &collapsed)
            .iter()
            .map(|row| row.ix)
            .collect();
        assert_eq!(shown, vec![0, 1, 5, 6]);
        // 按「看得见的第几行」重新发号会得到 [0,1,2,3] —— 那正是这个测试要挡住的错：
        // 折叠集会指到别的行上去。
        assert_ne!(shown, (0..shown.len()).collect::<Vec<_>>());
    }

    /// 打开面板时的那一份折叠：只留通往当前章的那条链。
    #[test]
    fn opening_follows_the_reading_position() {
        let (entries, hrefs) = book();
        let rows = build(&entries, &hrefs);
        // 在第 1 章（chap1）里：第1章 展开，其它分支本来就没孩子。
        let current = row_of_chapter(&rows, 1).expect("chap1 在树里");
        assert_eq!(current, 1, "章首对应的是「第1章」那一条，不是它的小节");
        let collapsed = collapse_except(&rows, Some(current));
        let shown: Vec<_> = visible(&rows, &collapsed)
            .iter()
            .map(|row| row.title.as_str())
            .collect();
        assert_eq!(
            shown,
            vec![
                "扉页",
                "第1章",
                "第一节",
                "第二节",
                "丢的那节",
                "第2章",
                "版权页"
            ]
        );

        // 在第 2 章：第1章 该折起来，它的三条小节一行都不许出现。
        let collapsed = collapse_except(&rows, row_of_chapter(&rows, 2));
        let shown: Vec<_> = visible(&rows, &collapsed)
            .iter()
            .map(|row| row.title.as_str())
            .collect();
        assert_eq!(shown, vec!["扉页", "第1章", "第2章", "版权页"]);
    }

    /// 对不上任何一条时（定版式、或当前章没进目录）不猜，直接退到只看顶层。
    #[test]
    fn a_position_with_no_row_collapses_everything() {
        let (entries, hrefs) = book();
        let rows = build(&entries, &hrefs);
        let collapsed = collapse_except(&rows, row_of_chapter(&rows, 99));
        assert_eq!(collapsed.len(), 1, "只有第1章 一个分支");
        assert!(collapsed.contains(&1));
        let shown = visible(&rows, &collapsed);
        assert_eq!(shown.len(), 4, "顶层四条");
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

    /// 面板那份状态：打开跟着位置走，折上再点开要回到原样。
    #[test]
    fn the_panel_opens_at_the_position_and_toggles_back_and_forth() {
        let (entries, hrefs) = book();
        let rows = build(&entries, &hrefs);
        let current = row_of_chapter(&rows, 2);
        let mut view = TocView::open(rows, current);
        // 拿到 owned 的标题而不是 `&str`：`visible()` 借的是 `view`，而下面还要改它。
        let folded: Vec<String> = view.visible().iter().map(|row| row.title.clone()).collect();
        assert_eq!(
            folded,
            vec!["扉页", "第1章", "第2章", "版权页"],
            "打开时只留通往当前章的那条链"
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
        // 当前在第 2 章 → 第1章 打开时就是折起来的。
        let current = row_of_chapter(&rows, 2);
        let mut view = TocView::open(rows, current);
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
        let current = row_of_chapter(&rows, 2);
        let mut view = TocView::open(rows, current);
        let forward: Vec<usize> = view.visible().iter().map(|row| row.ix).collect();
        view.set_reversed(true);
        let backward: Vec<usize> = view.visible().iter().map(|row| row.ix).collect();
        assert_eq!(backward, forward.into_iter().rev().collect::<Vec<_>>());
        assert_eq!(view.current_ix(2), current, "当前行认的还是同一条");
        assert_eq!(
            view.read_count(view.current_ix(2)),
            (6, 7),
            "「已读 6 / 7 章」与摆法无关"
        );
    }

    /// 「展开目录 / 折叠目录」那一颗的两端：折上只留顶层，展开回到整棵树。
    #[test]
    fn expanding_and_collapsing_all_reaches_both_ends() {
        let (entries, hrefs) = book();
        let rows = build(&entries, &hrefs);
        // 当前章在「第1章」下面 → 打开时那条分支就是展开的。
        let current = row_of_chapter(&rows, 1);
        let mut view = TocView::open(rows, current);
        assert!(view.all_open());
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
        view.expand_all();
        assert!(view.all_open());
        assert_eq!(view.visible().len(), 7, "全展开是整棵树");
    }
}
