//! 整机扫描的候选清单那一屏。
//!
//! 扫描**只找不导**（`folio_core::ingest::scan`）：找到的候选摆成一张按目录分组的清单，
//! 读者勾选之后走与「选文件导入」同一条 [`ShelfView::dispatch_import`] —— 勾选是
//! 读者亲手挑的，所以是 Picked 语义（重名算更新）。已在架上的那几条**默认不勾**，并且
//! 卡面垫一层灰底 + 写着「已在书架」：灰底是扫一眼就分得出来的那一档，字是说得出那一档的
//! 那一档 —— 只靠颜色不算交代（色盲、深色模式、截图给别人看，三头都会丢）。
//!
//! 每一张卡只有两行：**上面是书名，下面是类型与大小**。从前这里还多摆一件——开容器取
//! 真书名、真作者、真封面（一路单独的作业，一本一次 IO，`find` 只列目录不碰内容），
//! 几千本的机器上那一趟比扫描本身还慢，而它换到的只是清单更好看。现在一行都不开：
//! 类型就是扩展名大写，大小就是 `find` 已经报上来的那个数，书名就是文件名削掉扩展名。
//! **代价写清楚**：文件名起得不好的那本（`12345.epub`、带一堆站点后缀的那一种），
//! 这儿说的名字就不是书名 —— 点进「信息」那一页才看得到库里那个真名。
//!
//! 清单是虚拟列表：一次整机扫描找到几千本是正常的，只画视口内那几十行，行高定死。
//!
//! 卡内的滚动**不该带着身后的架面一起滚**：那张卡盖在哪一片格子上面，滚的就是卡里那一列。
//! 两处一起管这件事：
//!
//! - 卡根那一格 `.occlude()`：命中测试走到它就 `break`（`window.rs` 的 `hit_test`），
//!   身后的 hitbox 不进 `mouse_hit_test.ids`。它管的是**点击**（也顺手让
//!   `should_handle_scroll` 判假），但它是命中测试上的事，不是「滚动只归一处」的保证。
//! - 卡里的 `scroll_guard`：清单处理完这一笔就 `stop_propagation`，身后的架面、弹层外框
//!   一律收不到 —— 与 Flutter 的 gesture arena 同一条语义（内层 `Scrollable` 赢，外层
//!   不滚）。判据与用法见 `crate::ui::components::scroll_guard`。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use folio_core::{
    Cancel, Candidate, Priority, ScanFilter, ScanReport, TaskEvent, TaskManager, find, format_of,
};
use gpui_kit::base::{VirtualListScrollHandle, v_virtual_list};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::Input;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, App, AppContext as _, Context, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    TestSupportExt as _, WeakEntity, Window, div, point, px, relative, size,
};

use gpui_kit::assets::IconName;

use crate::ui::components::{muted_label, scroll_guard};
use crate::ui::services::app_store;
use crate::ui::services::config::ConfigHolder;
use crate::ui::services::l10n::{tf, tr};
use crate::ui::services::library::{scan_noise_exempt, scan_roots, target_dir};

use super::ShelfView;
use super::status::split_display_name;

/// 那一行读数：类型 · 大小 ·（在架上时）那句「已在书架」。
///
/// 每格都是**有值才摆**：文件名没有扩展名就不打头那个点，大小没报上来就少一格。三段各自
/// 是独立的一格而不是一句话，所以九门语言在这儿不会对不上（拼接的句子是这份文案表的老规矩）。
/// 那句「已在书架」由调用方取好递进来 —— 这一层只负责拼形状，测试才钉得住形状本身。
fn fact_line(ext: Option<&str>, size: Option<u64>, on_shelf: Option<&str>) -> String {
    let mut facts: Vec<String> = Vec::new();
    if let Some(ext) = ext {
        facts.push(ext.to_uppercase());
    }
    if let Some(bytes) = size {
        facts.push(fmt_size(bytes));
    }
    facts.extend(on_shelf.map(str::to_string));
    facts.join(" · ")
}

/// 筛选浮层的大致高（含它自己的内边距）：居中要往上抬掉半张。报的是估计值 ——
/// 与实际差几像素只让卡偏几像素，与 `overlay` 那几张居中卡同一条取舍。
const FILTER_CARD_HEIGHT: f32 = 176.0;

/// 一行的高度：名字一行 + 类型与大小一行 + 卡的上下内缩各一档。虚拟列表按它画视口，
/// 改哪一处都要回来改这一个，否则字会被裁在半行上。
const ROW_HEIGHT: f32 = 64.0;
/// 一张目录分组头的高度。
const HEADER_HEIGHT: f32 = 28.0;
/// 卡的上下内缩 = 行间距，同一档（`py_2`）。行高那一个数由它算出来。
const ROW_PAD: f32 = 8.0;

/// 清单里的一行：一个候选 + 它**在不在架上**（在的话带着库里那一行的主键）。
///
/// 存主键而不是一个布尔：在架上的那本不必再开容器 —— 库里有现成的书目与封面文件，
/// 那一路省下来的正是整屏里最贵的一次 IO。
pub(crate) struct ScanRow {
    pub candidate: Candidate,
    pub shelf: Option<i64>,
}

impl ScanRow {
    /// 这一行的本地路径（勾选与开容器都用它当身份）。远端候选没有 —— 扫描只交本地文件，
    /// 真来了远端的就空一行，别让它在清单里冒充一本。
    fn path(&self) -> Option<&Path> {
        match &self.candidate.origin {
            folio_core::Origin::Local(path) => Some(path),
            folio_core::Origin::Remote { .. } => None,
            folio_core::Origin::Cloud { .. } => None,
        }
    }
}

/// 虚拟列表里的一项：一条目录分组头，或一行候选（下标指回 `rows`）。
pub(in crate::ui::features::shelf) enum ListEntry {
    Header(String),
    Row(usize),
}

/// 扫描面板的两种时刻。
pub(crate) enum ScanPhase {
    /// 正在扫。`found` 来自后端的不定进度读数（`item(已找到, 0)`）。
    Scanning { found: u64, cancel: Cancel },
    /// 扫完了，等勾选。
    Results {
        rows: Vec<ScanRow>,
        /// 勾中的行按**本地路径**记（候选的身份）。
        checked: HashSet<PathBuf>,
        /// 通过筛选的行（`rows` 的下标，按原顺序）。分组头、高度表与「全选」都按它算 ——
        /// 读者眼前没摆出来的那几本不该被一起勾上。
        visible: Rc<Vec<usize>>,
        /// 按目录分组摊平的列表项，与每项的高度 —— 条件一变就按 `visible` 重算一次。
        entries: Rc<Vec<ListEntry>>,
        sizes: Rc<Vec<gpui_kit::Size<gpui_kit::Pixels>>>,
        /// 清单的滚动句柄。**必须跨帧持有**（与目录那屏同一条，见 `toc_panel` 的
        /// `scroll` 字段）：`v_virtual_list` 每次调用都新建一份内部句柄，不拿 `.track_scroll`
        /// 顶掉它，偏移就每帧归零 —— 手指滑了、下一帧弹回原处，屏幕上就是「滑不动」。
        scroll: VirtualListScrollHandle,
    },
}

/// 把一轮整机扫描包成后台作业，走**共用的池**（§1.1「一条队列」），按**预热档**排队：
/// 扫描能与导入并行（池有两条 worker），但让在跑的交互/预热作业排在前面。`exclude`
/// 至少含书库目录 —— 扫到自己拷的副本再导一遍是无意义的循环。
fn submit_scan(
    manager: &TaskManager,
    roots: Vec<PathBuf>,
    exclude: Vec<PathBuf>,
    noise_exempt: Vec<PathBuf>,
) -> folio_core::infra::task::TaskHandle<ScanReport> {
    manager.submit_with(Priority::Prewarm, move |ctx| {
        Ok(find(&roots, &exclude, &noise_exempt, ctx))
    })
}

impl ShelfView {
    /// 点「扫描」。书库位置没确认过就先摆确认面板 —— 勾选导入要往那里拷。
    ///
    /// Android 上多一道门：先在**后台线程**上要「所有文件访问」（请求会把调用
    /// 线程阻塞到读者从系统设置页回来，见 [`crate::ui::platform`]::
    /// request_storage_access），要不到就退回只扫应用自己目录的小范围扫描。
    pub(in crate::ui::features::shelf) fn start_scan(&mut self, cx: &mut Context<Self>) {
        if self.scan_open || self.importing.is_some() {
            // 正在扫或正在导：同一时刻一条线，那张状态卡就是「等一下再点」的提示。
            return;
        }
        if target_dir(&ConfigHolder::library(cx)).is_none() {
            self.confirming = true;
            cx.notify();
            return;
        }
        if cfg!(target_os = "android") {
            // 阻塞的授权请求丢给后台线程；回来后 `begin_scan` 接手。
            let requested =
                cx.background_spawn(async move { crate::ui::platform::request_storage_access() });
            self._scan = Some(cx.spawn(async move |this: WeakEntity<Self>, cx| {
                let all_files = requested.await;
                let _ = this.update(cx, |this, cx| this.begin_scan(all_files, cx));
            }));
            cx.notify();
            return;
        }
        self.begin_scan(true, cx);
    }

    /// 授权尘埃落定后的实际起扫。`all_files` 只在 Android 上不是 `true`。
    fn begin_scan(&mut self, all_files: bool, cx: &mut Context<Self>) {
        let roots = scan_roots(all_files);
        if roots.is_empty() {
            tracing::error!("ShelfView: 找不到可扫描的起点（HOME 没设？）");
            return;
        }
        let exclude = vec![target_dir(&ConfigHolder::library(cx)).expect("start_scan 刚确认过")];
        let noise_exempt = scan_noise_exempt();
        // 与导入共用同一个池（§1.1）：扫描走预热档，导入走落库档。
        let handle = submit_scan(
            &crate::ui::services::resident::pool(),
            roots,
            exclude,
            noise_exempt,
        );
        let cancel = handle.cancel.clone();
        self.scan = Some(ScanPhase::Scanning { found: 0, cancel });
        self.scan_open = true;
        self._scan = Some(cx.spawn(async move |this: WeakEntity<Self>, cx| {
            // 与导入同款的桥：mpsc 会阻塞读它的线程，转发进能 await 的通道。
            let (forward, events) = async_channel::unbounded::<TaskEvent<ScanReport>>();
            cx.background_spawn(async move {
                for event in handle.events.iter() {
                    if forward.send_blocking(event).is_err() {
                        break;
                    }
                }
            })
            .detach();
            while let Ok(event) = events.recv().await {
                if this
                    .update(cx, |this, cx| this.scan_event(event, cx))
                    .is_err()
                {
                    return;
                }
            }
            let _ = this.update(cx, |this, cx| this.scan_stream_closed(cx));
        }));
        cx.notify();
    }

    /// 后端扫描的一条事件 → 面板的一次变化。
    fn scan_event(&mut self, event: TaskEvent<ScanReport>, cx: &mut Context<Self>) {
        match event {
            TaskEvent::Started => {}
            TaskEvent::Progress(progress) => {
                if let Some(ScanPhase::Scanning { found, .. }) = &mut self.scan {
                    // 并行遍历下几条 worker 的读数会交错到达：只许往前，不许让屏幕上
                    // 那句「已找到 N 本」在 40 与 25 之间来回跳。
                    *found = (*found).max(progress.done);
                    cx.notify();
                }
            }
            TaskEvent::Bytes(_) => {}
            TaskEvent::Finished(Ok(report)) => {
                // 上一批的读数还没开完就换了新清单：旧路径的读数落在新清单上是白做的。
                // 「已在书架」按副本路径判：候选落进书库时的那个路径。顺带把库里那一行的
                // 主键拿住 —— 开容器那一路据此决定这一本要不要真的去开文件。
                //
                // **一次把库里的路径读成表**，不逐本 `book_by_path`：候选可能有几千条，
                // 而结果到手这一下本来就在界面线程上重活；几千次 SQL 会把读者刚要开始
                // 勾选的清单钉住好几秒。架上的书通常只有几百条，一次 `list_books` 便宜。
                let book_dir = target_dir(&ConfigHolder::library(cx));
                let store = app_store::handle(cx);
                let on_shelf: HashMap<String, i64> = match (&store, &book_dir) {
                    (Some(store), Some(_)) => store
                        .list_books()
                        .map(|books| books.into_iter().map(|row| (row.path, row.id)).collect())
                        .unwrap_or_else(|err| {
                            tracing::warn!(
                                "ShelfView: 读架上的路径表失败，逐本按未上架处理：{err:#}"
                            );
                            HashMap::new()
                        }),
                    _ => HashMap::new(),
                };
                let rows: Vec<ScanRow> = report
                    .candidates
                    .into_iter()
                    .map(|candidate| {
                        let shelf = book_dir.as_ref().and_then(|dir| {
                            let path = dir.join(&candidate.name);
                            on_shelf.get(path.to_string_lossy().as_ref()).copied()
                        });
                        ScanRow { candidate, shelf }
                    })
                    .collect();
                self.scan = Some(ScanPhase::Results {
                    checked: HashSet::new(),
                    rows,
                    visible: Rc::new(Vec::new()),
                    entries: Rc::new(Vec::new()),
                    sizes: Rc::new(Vec::new()),
                    scroll: VirtualListScrollHandle::new(),
                });
                // 上一轮留下的搜索词与筛选条件照旧生效（读者可能是「再扫一次找同一本」）；
                // 分组头与高度表按它们现算。
                self.rebuild_scan_list();
                // 清单到手 = 有东西可看了，这一屏就抬起来。读者若在扫的途中关掉过它，
                // 结果会把它再开一遍 —— 这是迁浮层**之前**就有的行为（那时判据就是
                // `scan.is_some()`），改的是容器不是这条。真要「扫完不打扰」，那是另一个
                // 决定：得让关掉那一下顺手把作业也停了。
                self.scan_open = true;
                cx.notify();
            }
            TaskEvent::Finished(Err(err)) => {
                tracing::error!("ShelfView: 扫描作业失败 {err:#}");
                self.scan = None;
                self.scan_open = false;
                cx.notify();
            }
            TaskEvent::Cancelled => {
                // 扫描可取消、没有副作用：取消就是收摊，不留半个面板。
                tracing::info!("ShelfView: 扫描被取消");
                self.scan = None;
                self.scan_open = false;
                cx.notify();
            }
        }
    }

    /// 扫描那条事件流走到头了。`Finished` / `Cancelled` 自己会收摊；走到这里还停在
    /// `Scanning` 只有一种来路：作业被丢掉、没交终局事件 —— 那时把这个壳收掉，免得
    /// 「正在扫」停在屏幕上。
    ///
    /// **只收 `Scanning`**：`Finished(Ok)` 已经把清单换进 [`Self::scan`] 并把面板抬起来，
    /// 顺手把它一起 take 掉，读者看到的就是「扫完了，弹层里一片空白」。
    fn scan_stream_closed(&mut self, cx: &mut Context<Self>) {
        if matches!(self.scan, Some(ScanPhase::Scanning { .. })) {
            self.scan = None;
            self.scan_open = false;
            cx.notify();
        }
    }

    /// 面板顶上那颗「停止扫描」。
    fn cancel_scan(&mut self, cx: &mut Context<Self>) {
        if let Some(ScanPhase::Scanning { cancel, .. }) = &self.scan {
            cancel.trigger();
        }
        cx.notify();
    }

    /// 勾 / 取消一行。
    fn set_row_checked(&mut self, row: usize, checked: bool, cx: &mut Context<Self>) {
        if let Some(ScanPhase::Results {
            rows, checked: set, ..
        }) = &mut self.scan
            && let Some(ScanRow {
                candidate: Candidate { origin, .. },
                ..
            }) = rows.get(row)
            && let folio_core::Origin::Local(path) = origin
        {
            if checked {
                set.insert(path.clone());
            } else {
                set.remove(path);
            }
        }
        cx.notify();
    }

    /// 按当前的搜索词与筛选条件重算「摆出来的那几行」。
    ///
    /// 两条筛法的判据都在 `folio_core::ScanFilter`（格式怎么认、大小怎么切由 core 说），
    /// 这一层只负责把留下来的行摊成分组头与高度表。滚动归零：换了一份清单还留着上一份
    /// 的偏移，读者会落在半截上。
    fn rebuild_scan_list(&mut self) {
        let filter = &self.scan_filter;
        let Some(ScanPhase::Results {
            rows,
            visible,
            entries,
            sizes,
            scroll,
            ..
        }) = &mut self.scan
        else {
            return;
        };
        let kept: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| filter.matches(&row.candidate, row.shelf.is_some()))
            .map(|(ix, _)| ix)
            .collect();
        let (next_entries, next_sizes) = flatten(rows, &kept);
        *visible = Rc::new(kept);
        *entries = next_entries;
        *sizes = next_sizes;
        scroll.set_offset(point(px(0.), px(0.)));
    }

    /// 搜索框里变了一个字：清单是内存里那份，逐字筛得动，不必像架面那样等安静。
    pub(in crate::ui::features::shelf) fn set_scan_query(
        &mut self,
        query: SharedString,
        cx: &mut Context<Self>,
    ) {
        if self.scan_filter.query == query {
            return;
        }
        self.scan_filter.query = query.to_string();
        self.rebuild_scan_list();
        cx.notify();
    }

    /// 点一格格式（EPUB / PDF …）：开或关。一条都没勾 = 不按格式筛。
    fn toggle_scan_format(&mut self, format: &str, cx: &mut Context<Self>) {
        if !self.scan_filter.formats.remove(format) {
            self.scan_filter.formats.insert(format.to_string());
        }
        self.rebuild_scan_list();
        cx.notify();
    }

    /// 「未在书架 / 已在书架」两格互斥：再点一次同一格就回到「都算」。
    fn toggle_scan_shelf(&mut self, on_shelf: bool, cx: &mut Context<Self>) {
        self.scan_filter.shelf = if self.scan_filter.shelf == Some(on_shelf) {
            None
        } else {
            Some(on_shelf)
        };
        self.rebuild_scan_list();
        cx.notify();
    }

    /// 大小上限变了：`text` 是框里那行字，按 MB 读，只留**小于**它的候选。
    ///
    /// 读不懂（空、负号、字母）就当不按大小筛 —— 读者正在打字的中间态不该把清单卡住。
    pub(in crate::ui::features::shelf) fn set_scan_size(
        &mut self,
        text: &str,
        cx: &mut Context<Self>,
    ) {
        let bytes = scan_size_bytes(text);
        if self.scan_filter.under == bytes {
            return;
        }
        self.scan_filter.under = bytes;
        self.rebuild_scan_list();
        cx.notify();
    }

    /// 清掉全部条件，三个框里那几行字也跟着空 —— 两处说的必须是同一件事。
    fn clear_scan_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.scan_filter.is_empty() {
            return;
        }
        self.scan_filter = ScanFilter::default();
        self.rebuild_scan_list();
        self.scan_search
            .update(cx, |boxed, cx| boxed.set_value("", window, cx));
        self.scan_size
            .update(cx, |input, cx| input.set_value("", window, cx));
        cx.notify();
    }

    /// 筛选浮层开合（读者点漏斗、点外面、按返回都归这一句）。
    fn set_scan_filter_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.scan_filter_open != open {
            self.scan_filter_open = open;
            cx.notify();
        }
    }

    /// 全选 / 清除（已在架上的不参与 —— 见模块注释）。
    ///
    /// 名字与架面那本 `ShelfView::toggle_select_all` 分开：那一颗管的是**多选的书**，
    /// 这一颗管的是扫描清单上的**勾**，两者都在 `impl ShelfView` 里，同名就是两条定义撞车。
    fn toggle_all_checked(&mut self, cx: &mut Context<Self>) {
        let Some(ScanPhase::Results {
            rows,
            visible,
            checked,
            ..
        }) = &mut self.scan
        else {
            return;
        };
        // 只作用在**摆出来的**那几行上：被筛掉的那几本读者没看见，替他勾上就是替他做决定。
        let eligible: Vec<PathBuf> = visible
            .iter()
            .filter_map(|ix| rows.get(*ix))
            .filter(|row| row.shelf.is_none())
            .filter_map(|row| match &row.candidate.origin {
                folio_core::Origin::Local(path) => Some(path.clone()),
                _ => None,
            })
            .collect();
        let all = !eligible.is_empty() && eligible.iter().all(|p| checked.contains(p));
        if all {
            // 逐本 `retain` 在几千行时是平方级；一张集合表是线性的。
            let eligible: HashSet<&PathBuf> = eligible.iter().collect();
            checked.retain(|path| !eligible.contains(path));
        } else {
            checked.extend(eligible);
        }
        cx.notify();
    }

    /// 「导入所选」：收掉那一屏，把勾中的路径按 Picked 语义交进导入管线。
    fn import_checked(&mut self, cx: &mut Context<Self>) {
        // 清单是**读**出来的而不是 `take()` 走的：take 会把内容当场抽掉，手机上那一屏
        // 就在退场那半程空给读者看。收的只是意图。
        let paths: Vec<PathBuf> = match self.scan.as_ref() {
            Some(ScanPhase::Results { rows, checked, .. }) => rows
                .iter()
                .filter_map(|row| match &row.candidate.origin {
                    folio_core::Origin::Local(path) if checked.contains(path) => Some(path.clone()),
                    _ => None,
                })
                .collect(),
            _ => return,
        };
        if paths.is_empty() {
            cx.notify();
            return;
        }
        tracing::info!("ShelfView: 从扫描清单导入 {} 本", paths.len());
        self.scan_open = false;
        // 导入那条线要走落库档了，为这一批候选开容器的路就到此为止：管线自己会把这些
        // 文件解析一遍，再开一次是同一件事做两遍。
        self.dispatch_import(Box::new(folio_core::PathsSource::picked(paths)), cx);
    }

    /// 收掉那一屏回架面。**只**改意图，理由同 [`ShelfView::close_url_panel`]。
    pub(in crate::ui::features::shelf) fn close_scan(&mut self, cx: &mut Context<Self>) {
        self.scan_open = false;
        // 卡片收了，挂在它上面那张筛选浮层也得收：不然下次打开会先弹一张没人点的浮层。
        self.scan_filter_open = false;
        // 屏收了，为它开的那一路也就没用了（清单留着，退场那半程还要画它已经亮起的那些）。
        cx.notify();
    }

    /// 喂一条扫描事件。只在跑回归门的那一端编译（与 `feed_import_event` 同理：
    /// 这条链的起点是后台线程，测试只能从「事件已经上了界面线程」这一处进）。
    #[cfg(all(test, not(mobile)))]
    pub(crate) fn feed_scan_event(&mut self, event: TaskEvent<ScanReport>, cx: &mut Context<Self>) {
        self.scan_event(event, cx);
    }

    /// 回归门用：勾 / 取消清单里第 `ix` 行（真正的入口是清单上的复选框）。
    #[cfg(all(test, not(mobile)))]
    pub(crate) fn check_scan_row(&mut self, ix: usize, checked: bool, cx: &mut Context<Self>) {
        self.set_row_checked(ix, checked, cx);
    }

    /// 回归门用：按下「导入所选」（真正的入口是清单底部那颗 primary 按钮）。
    #[cfg(all(test, not(mobile)))]
    pub(crate) fn import_checked_for_test(&mut self, cx: &mut Context<Self>) {
        self.import_checked(cx);
    }

    /// 回归门用：把搜索词直接灌进去（真入口是清单顶上那颗框）。
    #[cfg(all(test, not(mobile)))]
    pub(crate) fn set_scan_query_for_test(&mut self, query: &str, cx: &mut Context<Self>) {
        self.set_scan_query(query.into(), cx);
    }

    /// 回归门用：点一格格式（真入口是筛选面板里那颗格子）。
    #[cfg(all(test, not(mobile)))]
    pub(crate) fn toggle_scan_format_for_test(&mut self, format: &str, cx: &mut Context<Self>) {
        self.toggle_scan_format(format, cx);
    }

    /// 回归门用：直接给大小上限（真入口是浮层顶上那个框）。
    #[cfg(all(test, not(mobile)))]
    pub(crate) fn set_scan_size_for_test(&mut self, under: Option<u64>, cx: &mut Context<Self>) {
        self.scan_filter.under = under;
        self.rebuild_scan_list();
        cx.notify();
    }

    /// 回归门用：开合筛选浮层（真入口是那颗漏斗）。
    #[cfg(all(test, not(mobile)))]
    pub(crate) fn set_scan_filter_open_for_test(&mut self, open: bool, cx: &mut Context<Self>) {
        self.set_scan_filter_open(open, cx);
    }

    /// 回归门用：清掉全部条件（真入口是筛选面板右下那颗）。
    #[cfg(all(test, not(mobile)))]
    pub(crate) fn clear_scan_filter_for_test(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.clear_scan_filter(window, cx);
    }

    /// 回归门用：现在摆出来的那几行，按原顺序报名。
    #[cfg(all(test, not(mobile)))]
    pub(crate) fn visible_scan_names_for_test(&self) -> Vec<String> {
        match &self.scan {
            Some(ScanPhase::Results { rows, visible, .. }) => visible
                .iter()
                .filter_map(|ix| rows.get(*ix))
                .map(|row| row.candidate.name.clone())
                .collect(),
            _ => Vec::new(),
        }
    }

    /// 回归门用：清单那一列现在滚了多远（正数 = 往下滚过）。
    #[cfg(all(test, not(mobile)))]
    pub(crate) fn scan_offset_for_test(&self) -> f32 {
        match &self.scan {
            Some(ScanPhase::Results { scroll, .. }) => -f32::from(scroll.offset().y),
            _ => 0.0,
        }
    }

    /// 回归门用：模拟扫描事件流收尾（真实的收尾在 `begin_scan` 那条 spawn 的末尾）。
    #[cfg(all(test, not(mobile)))]
    pub(crate) fn close_scan_stream_for_test(&mut self, cx: &mut Context<Self>) {
        self.scan_stream_closed(cx);
    }

    /// 回归门用：现在勾中了几本。门要的是「点卡面这一下**净**勾上了一本」——
    /// 勾选框若嵌在卡面的 hitbox 里，gpui 会把同一次点击也发给它，两下相抵就成了
    /// 「按了没反应」，而那只差一个嵌套层级的事在屏幕上最难查。
    #[cfg(all(test, not(mobile)))]
    pub(crate) fn checked_rows_for_test(&self) -> usize {
        match &self.scan {
            Some(ScanPhase::Results { checked, .. }) => checked.len(),
            _ => 0,
        }
    }

    /// 扫描那一屏的内容：正在扫是读数 + 停止；扫完了是清单 + 勾选 + 导入所选。
    ///
    /// 容器不在这儿 —— 桌面是一张居中的浮层、手机是底部弹层（见 [`super::overlay`]），
    /// 这一屏只交内容。两支都用 `flex_1` / `size_full` 撑满给到的那块高，所以换容器
    /// 不必改这里。
    pub(crate) fn scan_body(&self, cx: &mut Context<Self>) -> AnyElement {
        match &self.scan {
            Some(ScanPhase::Scanning { found, .. }) => {
                let entity = cx.entity().clone();
                let stop_label = tr(cx, "shelf.scan_stop");
                let text: SharedString = if *found == 0 {
                    tr(cx, "shelf.scan_running")
                } else {
                    tf(cx, "shelf.scan_found", &[("n", found)]).into()
                };
                v_flex()
                    .id("shelf-scan-panel")
                    // 回归门按 id 认这一屏画没画出来（与架面那张状态卡同一套）。
                    .test_support()
                    // 扫的那几秒也一样挡着架面：两档同一个停靠，别一路挡一路不挡。
                    .occlude()
                    // 这一格还没得滚，但手指压上去也不该带动身后的架面 —— 浮层上的一笔
                    // 滚动到此为止（见 `components::scroll_guard`）。
                    .child(scroll_guard("shelf-scan-scroll"))
                    .size_full()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(tr(cx, "shelf.scan_title")),
                    )
                    .child(muted_label(text, cx))
                    .child(
                        Button::new("shelf-scan-stop")
                            .ghost()
                            .label(stop_label.clone().to_string())
                            .accessibility_label(stop_label)
                            .on_click(move |_, _, cx| {
                                entity.update(cx, |this, cx| this.cancel_scan(cx))
                            }),
                    )
                    .into_any_element()
            }
            Some(ScanPhase::Results {
                rows,
                checked,
                visible,
                sizes,
                scroll,
                ..
            }) => {
                let count = checked.len();
                // 可勾的与勾中的都只数**摆出来的**那几行（被筛掉的读者看不见，替他数进去
                // 只会让「全选」这颗按钮说得不对）。
                let eligible: usize = visible
                    .iter()
                    .filter_map(|ix| rows.get(*ix))
                    .filter(|row| row.shelf.is_none())
                    .count();
                // 判「全选」要看**可勾的那几本**是不是都勾了，而不是「勾了几本 ≥ 几本」：
                // 读者手点了一本已在架上的，一样会让两个数相等，那时按钮不该变成「清除」。
                let eligible_checked = visible
                    .iter()
                    .filter_map(|ix| rows.get(*ix))
                    .filter(|row| row.shelf.is_none())
                    .filter(|row| row.path().is_some_and(|path| checked.contains(path)))
                    .count();
                let all_checked = eligible > 0 && eligible_checked == eligible;
                let entity = cx.entity().clone();
                let close_entity = entity.clone();
                let select_entity = entity.clone();
                let import_entity = entity.clone();

                let select_label = if all_checked {
                    tr(cx, "shelf.scan_clear")
                } else {
                    tr(cx, "shelf.scan_select_all")
                };
                let import_label: SharedString =
                    tf(cx, "shelf.scan_import", &[("n", &count)]).into();

                let list_entity = entity.clone();
                v_flex()
                    .id("shelf-scan-panel")
                    .test_support()
                    // 这一格 `.occlude()` 挡的是**身后的架面**：命中测试走到这一格就 `break`
                    // （`window.rs` 的 `hit_test`），身后的 hitbox 不进 `mouse_hit_test.ids`。
                    // 卡外面那一片照旧归浮层自己的点外关闭：那颗 catcher 在更后面，只有落在
                    // 卡内的点击才会被这一格挡掉，而那本来就该挡。
                    //
                    // 它挡点击，**不担保滚动**：`should_handle_scroll` 判真与否还看 hitbox 有没有
                    // 把这一格算进去。所以滚动归属另有一条闸 —— 下面那颗 `scroll_guard` 在清单
                    // 处理完这一笔之后 `stop_propagation`，身后的架面与弹层外框都收不到（Flutter
                    // 的 arena 里「内层 Scrollable 赢、外层收不到」）。
                    .occlude()
                    .flex_1()
                    .min_h_0()
                    // **必须排在清单之前**：鼠标监听按绘制顺序登记、冒泡相倒着走 —— 先画才
                    // 让它在清单自己的滚动处理之后跑（见 `components::scroll_guard`）。
                    .child(scroll_guard("shelf-scan-scroll"))
                    // 顶上那一行：左边按文件名搜，右边那颗漏斗点开是一张筛选浮层。
                    .child(self.scan_toolbar(cx))
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            // 有两种「这里没东西」：扫过去真的一本没有，与筛没了 ——
                            // 说的话必须不一样，否则读者会以为书被筛丢了。
                            .when(rows.is_empty(), |list| {
                                list.child(scan_empty(tr(cx, "shelf.scan_none"), cx))
                            })
                            .when(!rows.is_empty() && visible.is_empty(), |list| {
                                list.child(scan_empty(tr(cx, "shelf.scan_empty_filtered"), cx))
                            })
                            .when(!visible.is_empty(), |list| {
                                list.child(
                                    v_virtual_list(
                                        list_entity,
                                        "shelf-scan-rows",
                                        sizes.clone(),
                                        move |this, range, _window, cx| scan_rows(this, range, cx),
                                    )
                                    // 句柄由 `ScanPhase::Results` 跨帧持有：不顶掉这一句，清单
                                    // 每帧把偏移抹回 0，手机上就上下滑不动。
                                    .track_scroll(scroll)
                                    .flex_1(),
                                )
                            }),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .justify_end()
                            .gap_2()
                            .px(px(crate::ui::features::shelf::view::GUTTER))
                            .py_2()
                            .child(
                                Button::new("shelf-scan-close")
                                    .ghost()
                                    .label(tr(cx, "shelf.scan_close").to_string())
                                    .accessibility_label(tr(cx, "shelf.scan_close"))
                                    .on_click(move |_, _, cx| {
                                        close_entity.update(cx, |this, cx| this.close_scan(cx))
                                    }),
                            )
                            .when(!visible.is_empty(), |foot| {
                                foot.child(
                                    Button::new("shelf-scan-select")
                                        .ghost()
                                        // 全屏都是「已在架上」时这两颗都没有对象可作用：
                                        // 一颗禁用的按钮比一次「按了没反应」说得清楚。
                                        .disabled(eligible == 0)
                                        .label(select_label.to_string())
                                        .accessibility_label(select_label)
                                        .on_click(move |_, _, cx| {
                                            select_entity
                                                .update(cx, |this, cx| this.toggle_all_checked(cx))
                                        }),
                                )
                                .child(
                                    Button::new("shelf-scan-import")
                                        .primary()
                                        .disabled(count == 0)
                                        .label(import_label.to_string())
                                        .accessibility_label(import_label)
                                        .on_click(move |_, _, cx| {
                                            import_entity
                                                .update(cx, |this, cx| this.import_checked(cx))
                                        }),
                                )
                            }),
                    )
                    // 筛选浮层：绝对定位铺满卡片，不参与布局，只把触发器摆到正中。
                    .child(self.scan_filter_popover(cx))
                    .into_any_element()
            }
            // 面板只在 scan 有值时被调；这条分支是给类型闭合的。
            None => div().into_any_element(),
        }
    }

    /// 清单顶上那一行：**搜索在左**，筛选那颗漏斗在右。
    ///
    /// 搜索占满整行剩下的宽度 —— 手机上它要够宽，才打得开软键盘不误触；筛选只留一个
    /// 图标：它是低频动作，占一个字的宽度不如把那一格还给输入框。
    fn scan_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let active = !self.scan_filter.is_empty();
        let open = self.scan_filter_open;
        let entity = cx.entity().clone();
        let label = tr(cx, "shelf.scan_filter");
        h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .px(px(crate::ui::features::shelf::view::GUTTER))
            .pt_2()
            .pb_1()
            .child(div().flex_1().min_w_0().child(self.scan_search.clone()))
            .child(
                Button::new("shelf-scan-filter")
                    // 正筛着（或正开着浮层）时压成实心：读者一眼看得出「现在摆的不是全部」。
                    .when(active || open, |button| button.secondary())
                    .when(!(active || open), |button| button.ghost())
                    .small()
                    .icon(IconName::ListFilter)
                    .tooltip(label.clone().to_string())
                    .accessibility_label(label)
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |this, cx| this.set_scan_filter_open(!open, cx))
                    }),
            )
            .into_any_element()
    }

    /// 筛选那张浮层：**钉在卡片正中**，由漏斗那颗按钮开关。
    ///
    /// 触发器是一颗零尺寸的按钮摆在卡片正中，再往上抬半张卡 —— `Popover` 只会钉在
    /// 触发器的某个角上，想让它居中就得把触发器摆到中心去（与 `overlay::centered`
    /// 同一手法）。高度是**报上去**的估计值，差几像素只让卡偏几像素，不难看。
    fn scan_filter_popover(&self, cx: &mut Context<Self>) -> AnyElement {
        // 收着的时候连体都不建：`scan_filter_body` 要数一遍格式（几千行就是几千次），
        // 而它只服务于「开着」那一档。
        if !self.scan_filter_open {
            return div().into_any_element();
        }
        let entity = cx.entity().clone();
        let body = self.scan_filter_body(cx);
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div().size(px(0.)).child(
                    Popover::new("shelf-scan-filter-popover")
                        .anchor(Anchor::TopCenter)
                        .offset(px(-FILTER_CARD_HEIGHT / 2.))
                        .open(self.scan_filter_open)
                        .on_open_change(move |open, _, cx| {
                            let open = *open;
                            entity.update(cx, |this, cx| this.set_scan_filter_open(open, cx));
                        })
                        .trigger(
                            Button::new("shelf-scan-filter-trigger")
                                .ghost()
                                .p_0()
                                .w(px(0.))
                                .h(px(0.))
                                .min_w(px(0.))
                                .min_h(px(0.)),
                        )
                        .child(body),
                ),
            )
            .into_any_element()
    }

    /// 筛选浮层里那一块：格式 / 大小 / 在架状态。
    ///
    /// 一条都不勾 = 这一格不筛，所以不摆「全部」那颗：它和「一颗都不勾」是同一件事，
    /// 多摆一个只会让人怀疑默认状态。格式只摆**结果里真有的** —— 这台机器上只有 epub
    /// 与 pdf 时，摆一排 mobi 的格子只是让读者多点一次「筛完什么都没有」。格子上只写
    /// 格式本身，不缀本数：那是一句会随筛选变的读数，格子要说的是「点它筛什么」。
    ///
    /// 宽度按 260 给：手机上弹层也不会顶到屏幕边；两行格子 + 一行「清除」正好。
    fn scan_filter_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(ScanPhase::Results { rows, .. }) = &self.scan else {
            return div().into_any_element();
        };
        let entity = cx.entity().clone();
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for row in rows {
            if let Some(format) = format_of(&row.candidate.name) {
                *counts.entry(format).or_default() += 1;
            }
        }
        // 数只用来**排序**：多的排前面（读者最可能点的就是它），同数按字母 ——
        // 顺序在两次开合之间不变。格子上只写格式，不写本数。
        let mut formats: Vec<(String, usize)> = counts.into_iter().collect();
        formats.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        let mut format_chips = Vec::new();
        for (format, _) in formats {
            let selected = self.scan_filter.formats.contains(&format);
            let chip_entity = entity.clone();
            let key = format.clone();
            let text = format.to_uppercase();
            format_chips.push(
                Button::new(SharedString::from(format!("shelf-scan-format-{format}")))
                    .when(selected, |button| button.secondary())
                    .when(!selected, |button| button.ghost())
                    .small()
                    .label(text.clone())
                    .accessibility_label(text)
                    .on_click(move |_, _, cx| {
                        chip_entity.update(cx, |this, cx| this.toggle_scan_format(&key, cx))
                    })
                    .into_any_element(),
            );
        }

        let mut state_chips = Vec::new();
        for on_shelf in [false, true] {
            let selected = self.scan_filter.shelf == Some(on_shelf);
            let chip_entity = entity.clone();
            let label = if on_shelf {
                tr(cx, "shelf.scan_on_shelf")
            } else {
                tr(cx, "shelf.scan_filter_not_on_shelf")
            };
            state_chips.push(
                Button::new(SharedString::from(if on_shelf {
                    "shelf-scan-state-on"
                } else {
                    "shelf-scan-state-off"
                }))
                .when(selected, |button| button.secondary())
                .when(!selected, |button| button.ghost())
                .small()
                .label(label.clone().to_string())
                .accessibility_label(label)
                .on_click(move |_, _, cx| {
                    chip_entity.update(cx, |this, cx| this.toggle_scan_shelf(on_shelf, cx))
                })
                .into_any_element(),
            );
        }

        // 一个框：填多少就是「只留小于这个数（MB）的」。宽 96 是「123.5」加清除钮
        // 还放得下的那一档。
        let size_inputs = vec![
            div()
                .w(px(96.))
                .child(Input::new(&self.scan_size).small().cleanable(true))
                .into_any_element(),
        ];
        let clear_entity = entity.clone();
        let mut panel = v_flex()
            // 回归门按 id 认这张浮层开没开（它只在开着的那几帧里在树上）。
            .id("shelf-scan-filter-panel")
            .test_support()
            .w(px(260.))
            .gap_2()
            // 大小放**最上面**：它是这一屏最常用的那一刀，也是唯一要动手打字的一格。
            .child(facet_row(tr(cx, "shelf.scan_filter_size"), size_inputs, cx));
        if !format_chips.is_empty() {
            panel = panel.child(facet_row(
                tr(cx, "shelf.scan_filter_format"),
                format_chips,
                cx,
            ));
        }
        panel = panel.child(facet_row(
            tr(cx, "shelf.scan_filter_state"),
            state_chips,
            cx,
        ));
        if !self.scan_filter.is_empty() {
            panel = panel.child(
                h_flex().w_full().justify_end().child(
                    Button::new("shelf-scan-filter-clear")
                        .text()
                        .small()
                        .label(tr(cx, "shelf.scan_filter_clear").to_string())
                        .on_click(move |_, window, cx| {
                            clear_entity.update(cx, |this, cx| this.clear_scan_filter(window, cx))
                        }),
                ),
            );
        }
        panel.into_any_element()
    }
}

/// `[range.start, range.end)` 这几行清单。分组头与候选卡都在这里画。
fn scan_rows(
    this: &mut ShelfView,
    range: std::ops::Range<usize>,
    cx: &mut Context<ShelfView>,
) -> Vec<AnyElement> {
    let Some(ScanPhase::Results {
        rows,
        checked,
        entries,
        ..
    }) = &this.scan
    else {
        return Vec::new();
    };
    let foreground = cx.theme().foreground;
    let muted = cx.theme().muted_foreground;
    let border = cx.theme().border;
    let primary = cx.theme().primary;
    let card_radius = cx.theme().radius_lg;
    let placeholder = cx.theme().muted;
    let entity = cx.entity().clone();
    let mut out = Vec::new();
    for ix in range {
        match entries.get(ix) {
            Some(ListEntry::Header(dir)) => {
                out.push(
                    div()
                        .h(px(HEADER_HEIGHT))
                        .w_full()
                        .px(px(super::GUTTER))
                        // 真机路径是 `/storage/emulated/0/Download/…` 这一种长度：
                        // 分组头只报「这批在哪个目录」，截断比折行好 —— 行高是定死的。
                        .truncate()
                        .text_color(muted)
                        .child(dir.clone())
                        .into_any_element(),
                );
            }
            Some(ListEntry::Row(ix)) => {
                let ix = *ix;
                let Some(row) = rows.get(ix) else { continue };
                // 名字与类型都从文件名来：这一屏不开容器（模块头写了这一刀换来什么，
                // 以及文件名起得不好的那本去哪儿看真名）。
                let (stem, ext) = split_display_name(&row.candidate.name);
                let name: SharedString = stem.into();
                let on_shelf = row.shelf.is_some();
                let picked = row.path().is_some_and(|path| checked.contains(path));
                let on_shelf_label = on_shelf.then(|| tr(cx, "shelf.scan_on_shelf").to_string());
                let facts = fact_line(ext, row.candidate.size, on_shelf_label.as_deref());
                // 两块命中区各有各的一份句柄：`move` 闭包会把抓进去的值吃掉。
                let row_entity = entity.clone();
                let check_entity = entity.clone();
                out.push(
                    div()
                        .h(px(ROW_HEIGHT))
                        .w_full()
                        .px(px(super::GUTTER))
                        // 行间距归这一层（卡自己的 `py` 是卡内缩），于是卡与卡之间正好
                        // 是 `ROW_PAD` 那一条缝，而不是一圈描边叠一圈留白。
                        .py(px(ROW_PAD))
                        .child(
                            // 一张候选卡：描边立住、不靠填色也不靠投影 —— 与架上那张卡、
                            // 笔记那一列卡同一套（浮层里再垫一层投影就是三层表面在打架）。
                            h_flex()
                                .w_full()
                                .h_full()
                                .items_center()
                                .gap_3()
                                .px_3()
                                .rounded(card_radius)
                                .overflow_hidden()
                                .border_1()
                                .border_color(border)
                                // 已在架上的那本垫一层灰底：一屏十几行时扫一眼就分得出哪几本
                                // 不必再导。灰底之外那句「已在书架」照旧摆着 —— 只靠颜色不算
                                // 交代（色觉、深色模式、把截图发给别人看，三头都会丢掉这一档）。
                                .when(on_shelf, |this| this.bg(placeholder))
                                // 勾中这件事要落在**卡自己的表面**上，不能只靠右边那颗
                                // 小小的勾选框：底色压得很薄，不与卡片本身的层次打架。
                                .when(picked, |this| {
                                    this.border_color(primary).bg(primary.opacity(0.06))
                                })
                                .child(
                                    // 点得着的那半张卡：上面名字，下面那一行读数。勾选框是它的
                                    // **兄弟**而不是孩子 —— gpui 把一次点击发给每一个 hitbox
                                    // 含按下点的 handler，套在里面的话点一下会先勾上再被外层取消掉。
                                    v_flex()
                                        .id(format!("shelf-scan-pick-{ix}"))
                                        .test_support()
                                        .flex_1()
                                        .min_w_0()
                                        .gap_0p5()
                                        .overflow_hidden()
                                        .cursor_pointer()
                                        .active(|style| style.opacity(0.75))
                                        .on_click(move |_, _, cx| {
                                            row_entity.update(cx, |this, cx| {
                                                this.set_row_checked(ix, !picked, cx)
                                            })
                                        })
                                        .child(
                                            div()
                                                .w_full()
                                                .truncate()
                                                .text_color(foreground)
                                                .text_sm()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .child(name),
                                        )
                                        .child(
                                            div()
                                                .w_full()
                                                .truncate()
                                                .text_xs()
                                                .text_color(muted)
                                                .child(facts),
                                        ),
                                )
                                .child(
                                    Checkbox::new(format!("shelf-scan-check-{ix}"))
                                        .checked(picked)
                                        .on_click(move |checked: &bool, _, cx| {
                                            check_entity.update(cx, |this, cx| {
                                                this.set_row_checked(ix, *checked, cx)
                                            })
                                        }),
                                ),
                        )
                        .into_any_element(),
                );
            }
            None => {}
        }
    }
    out
}

/// 框里那行字（MB）→ 字节。空、负号、读不懂的都给 `None`（= 不按大小筛）。
///
/// 收逗号当小数点：欧洲那几门语言里读者会打「5,5」。这一句与 `fmt_size` 的除法
/// 同一套 1024 进制，读者按「12 MB」打进去，筛出来的也是显示成 12 MB 的那几本。
fn scan_size_bytes(text: &str) -> Option<u64> {
    let trimmed = text.trim().replace(',', ".");
    if trimmed.is_empty() {
        return None;
    }
    let mb: f64 = trimmed.parse().ok()?;
    if !mb.is_finite() || mb < 0.0 {
        return None;
    }
    Some((mb * 1024.0 * 1024.0).round() as u64)
}

/// 筛选面板里的一行：左边一格名字，右边一串能换行的格子。
///
/// 名字那一格固定宽：三行的格子从同一条竖线上起，读起来才是一张表而不是三句飘着的标签。
fn facet_row(label: SharedString, chips: Vec<AnyElement>, cx: &App) -> AnyElement {
    h_flex()
        .w_full()
        .items_start()
        .gap_2()
        .child(
            div()
                // 52 是这几门语言里最长那个词（ru「Состояние」）折成两行还能看的宽度：
                // 再窄一点德语/俄语就要在一格中间断字，再宽一点手机上的格子就只剩两列。
                .w(px(52.))
                .flex_shrink_0()
                // 与格子里的字对齐：`.small()` 的按钮上下留白大约就是这 6 像素。
                .pt(px(6.))
                .text_xs()
                .line_height(relative(1.1))
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .flex_wrap()
                .gap_1()
                .children(chips),
        )
        .into_any_element()
}

/// 清单那一格里的「这里没东西」：一句灰字，居中。
fn scan_empty(text: SharedString, cx: &App) -> AnyElement {
    v_flex()
        // 回归门按 id 认「这一格说的是哪一句空话」（筛没了 / 一本没扫到）。
        .id("shelf-scan-empty")
        .test_support()
        .size_full()
        .items_center()
        .justify_center()
        .px(px(crate::ui::features::shelf::view::GUTTER))
        .child(
            div()
                .text_sm()
                .text_align(gpui_kit::TextAlign::Center)
                .text_color(cx.theme().muted_foreground)
                .child(text),
        )
        .into_any_element()
}

/// 把**留下来的**候选摊平成「分组头 + 行」的列表项与对应高度表。
///
/// `visible` 是 `rows` 的下标（筛选之后那一串，顺序照原样），分组头按这些行的目录算 ——
/// 某一组被筛空了就整条头都不摆，不会留下一行只有目录名的空组。
fn flatten(
    rows: &[ScanRow],
    visible: &[usize],
) -> (
    Rc<Vec<ListEntry>>,
    Rc<Vec<gpui_kit::Size<gpui_kit::Pixels>>>,
) {
    let mut entries = Vec::with_capacity(visible.len() * 2);
    let mut sizes = Vec::with_capacity(visible.len() * 2);
    let mut last_dir: Option<String> = None;
    for &ix in visible {
        let Some(row) = rows.get(ix) else { continue };
        let dir = row
            .path()
            .and_then(|path| path.parent())
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        if last_dir.as_deref() != Some(dir.as_str()) {
            entries.push(ListEntry::Header(dir.clone()));
            sizes.push(size(px(10000.), px(HEADER_HEIGHT)));
            last_dir = Some(dir);
        }
        entries.push(ListEntry::Row(ix));
        sizes.push(size(px(10000.), px(ROW_HEIGHT)));
    }
    (Rc::new(entries), Rc::new(sizes))
}

/// 字节数 → 人读的大小。一本书正文几 MB，单位给到 MB 就够；扫描清单、卡片长按菜单点开的
/// 「信息」那一页（[`super::book_info`]），以及阅读页书头那一行，共用这一条。
pub(crate) fn fmt_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024 * 1024) as f64)
    } else if bytes >= 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    //! 这一屏现在只说三件事：名字、类型、大小（外加「已在书架」那一格）。开容器那一路
    //! 已经整条摘掉 —— 它换到的是更好看的清单，付出的是「几千本 = 几千次 IO」，而扫描
    //! 本身只列目录、根本不碰内容。所以这里的门钉的是**形状**：有值才摆那一格，
    //! 没有值就不许留下一个孤零零的分隔点。
    //!
    //! 名字那一格用的是文件名削掉扩展名 —— 起得不好的文件名（`12345.epub`）在这儿说的
    //! 就不是书名，这是这一刀的代价，写在模块头。

    use super::{fact_line, scan_size_bytes, split_display_name};

    /// 框里那行字按 MB 读：空 = 不限，逗号当小数点，负号与字母当「不限」。
    #[test]
    fn a_size_box_reads_megabytes_and_nothing_else() {
        assert_eq!(scan_size_bytes("5"), Some(5 * 1024 * 1024));
        assert_eq!(scan_size_bytes(" 0,5 "), Some(512 * 1024));
        assert_eq!(
            scan_size_bytes("12.5"),
            Some((12.5 * 1024.0 * 1024.0) as u64)
        );
        assert_eq!(scan_size_bytes(""), None, "空 = 这一头不限");
        assert_eq!(scan_size_bytes("   "), None);
        assert_eq!(scan_size_bytes("很大"), None, "读不懂就当不限");
        assert_eq!(scan_size_bytes("-3"), None, "负数不成立");
    }

    /// 三段各自一格：缺哪格就少一格，不打头那个点。
    #[test]
    fn a_fact_line_shows_only_the_cells_that_have_a_value() {
        assert_eq!(fact_line(Some("epub"), Some(4096), None), "EPUB · 4 KB");
        assert_eq!(
            fact_line(None, Some(4096), Some("已在书架")),
            "4 KB · 已在书架",
            "没有扩展名就不该以「 · 」开头"
        );
        assert_eq!(fact_line(Some("mobi"), None, None), "MOBI");
        assert_eq!(fact_line(None, None, None), "", "三格都没有就没话可说");
    }

    /// 名字与扩展名的那一刀：真机上常见的三种形状。
    #[test]
    fn the_name_split_keeps_the_stem_and_reads_the_extension() {
        assert_eq!(
            split_display_name("基督山伯爵.epub"),
            ("基督山伯爵", Some("epub"))
        );
        // 点在开头（`.envrc`）与没有点：都不削，免得把名字削成空串。
        assert_eq!(split_display_name(".envrc"), (".envrc", None));
        assert_eq!(
            split_display_name("无扩展名的那本"),
            ("无扩展名的那本", None)
        );
        // 长尾巴不算扩展名：那是文件名的一部分。
        assert_eq!(
            split_display_name("a.这是一长串不是类型"),
            ("a.这是一长串不是类型", None)
        );
    }
}
