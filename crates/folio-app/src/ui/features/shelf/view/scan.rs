//! 整机扫描的候选清单那一屏。
//!
//! 扫描**只找不导**（`folio_core::ingest::scan`）：找到的候选摆成一张按目录分组的清单，
//! 读者勾选之后走与「选文件导入」同一条 [`ShelfView::dispatch_import`] —— 勾选是
//! 读者亲手挑的，所以是 Picked 语义（重名算更新）。已在上架的那几条**默认不勾**
//! 并标出来：再导一遍等于用同一份文件更新，多半不是读者点扫描想干的事。
//!
//! 每一本是一张**卡**：左边封面、中间三行（书名 → 作者 → 大小）、右边那颗勾选框。摆
//! 真封面与真书名要靠开容器 —— `find` 只列目录、不碰内容，候选身上只有文件名与大小。
//! 所以清单到手之后另起一路作业，按清单顺序**一本一本**开（见 [`start_peeks`]），
//! 每本开出来就画一张卡：读者看到的是清单**一本一本亮起来**，而不是等一整批。
//!
//! 清单是虚拟列表：一次整机扫描找到几千本是正常的，只画视口内那几十行，行高定死。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use folio_core::content::format::cover::CoverRef;
use folio_core::content::format::{ParseBudget, guess_media_type, thumb};
use folio_core::infra::paths::THUMB_LIST_PX;
use folio_core::ingest::identity::sha256_of_bytes;
use folio_core::{Cancel, Candidate, Priority, ScanReport, Store, TaskEvent, TaskManager, find};
use gpui_kit::base::v_virtual_list;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, FontWeight, Image, InteractiveElement as _, IntoElement,
    ObjectFit, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    StyledImage, Task, TestSupportExt as _, WeakEntity, div, img, px, size,
};

use crate::ui::components::muted_label;
use crate::ui::foundation::images::cover_image;
use crate::ui::services::app_store;
use crate::ui::services::config::ConfigHolder;
use crate::ui::services::l10n::{tf, tr};
use crate::ui::services::library::{scan_noise_exempt, scan_roots, target_dir};

use super::super::components::book_card::AUTHOR_SEP;
use super::ShelfView;

/// 一行的高度：封面 60 + 卡的上下内缩各 8 + 行与行之间各 8。虚拟列表按它画视口，
/// 三个数在这里合成一个 —— 改哪一处都要回来改这一个，否则字会被裁在半行上。
const ROW_HEIGHT: f32 = 92.0;
/// 一张目录分组头的高度。
const HEADER_HEIGHT: f32 = 28.0;
/// 卡上那一格封面：40×60，还是 2:3（与笔记卡片、目录书头那把尺同一族），只是这一屏是
/// 「挑一本」的密集清单，缩一档。写 `px` 不写 rem：那是位图的边界，不是排版间距。
const COVER_W: f32 = 40.0;
const COVER_H: f32 = 60.0;
/// 卡的上下内缩 = 行间距，同一档（`py_2`）。行高那一个数由它两个算出来。
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
        }
    }
}

/// 虚拟列表里的一项：一条目录分组头，或一行候选（下标指回 `rows`）。
pub(in crate::ui::features::shelf) enum ListEntry {
    Header(String),
    Row(usize),
}

/// 后台那一路交回的一份读数。**字节**而不是解好的图：`cover_image` 认格式、SVG 还要先
/// 栅格化，那一步留在能拿到实体的一侧做（与架上那一格同一口径）。
pub(crate) struct Peeked {
    path: PathBuf,
    title: String,
    authors: String,
    cover: Option<Vec<u8>>,
}

/// 界面这一侧存好的一份读数：封面已经解成可画的图。
///
/// 按本地路径存进 [`ScanPhase::Results::peeked`]，随到随画。作者为空串 = 这本书没说，
/// 界面就不摆那一行（与「信息」那一屏「有值才摆那一行」同一条口径）。
pub(crate) struct Peek {
    title: SharedString,
    authors: SharedString,
    cover: Option<Arc<Image>>,
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
        /// 读不了（权限）而跳过的处数，界面上那句「跳过 N 处」。
        skipped: usize,
        /// 命中预算被截断，结果不完整。
        truncated: bool,
        /// 按目录分组摊平的列表项，与每项的高度 —— 结果到手时算一次。
        entries: Rc<Vec<ListEntry>>,
        sizes: Rc<Vec<gpui_kit::Size<gpui_kit::Pixels>>>,
        /// 已经开出真书目的那几本：本地路径 → 读数。**不在表里**就是还在路上，
        /// 那一行退回文件名 + 灰底 —— 这不是坏状态，而是清单正在一本一本亮起来的中途。
        peeked: HashMap<PathBuf, Rc<Peek>>,
    },
}

/// 那一路开容器作业的把手：取消旗标 + 收信那一条。
///
/// 整个被 `take()` 掉就同时停了两头 —— Task 一 drop，通道那头断掉，后台那一本写完就
/// 不再发信，自己看见发送失败也就收了。只留旗标不够：那一条收信循环还在空转。
pub(crate) struct PeekRun {
    cancel: Cancel,
    _reader: Task<()>,
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

/// 把「按清单顺序一本一本开容器」包成一路后台作业，同样走**预热档**：这一路的全部
/// 意义是画得好看一点，读者真点「导入所选」时那条线必须排在它前面。
///
/// 一次开**一本**、顺序开：外层已经是一本在飞一本，再让每本内部起一堆解析线程就是
/// 拿 N 本 × 核数去挤那两条 worker（`AtMost(1)` 那一档说给 provider 听）。
fn submit_peeks(
    manager: &TaskManager,
    store: Arc<Store>,
    book_dir: Option<PathBuf>,
    targets: Vec<(PathBuf, Option<i64>)>,
    sink: async_channel::Sender<Peeked>,
) -> folio_core::infra::task::TaskHandle<usize> {
    manager.submit_with(Priority::Prewarm, move |ctx| {
        let mut done = 0usize;
        for (path, shelf) in targets {
            if ctx.cancel().triggered() {
                break;
            }
            let Some(peeked) = peek_one(&store, book_dir.as_deref(), &path, shelf) else {
                continue;
            };
            // 收信那头不在了（那一屏已经收了）就收摊：为一张不在屏上的卡继续开文件是白烧。
            if sink.send_blocking(peeked).is_err() {
                break;
            }
            done += 1;
        }
        Ok(done)
    })
}

/// 开一本，取真书名、作者与封面。
///
/// 读不了（不是本书、容器坏了、权限）就回 `None` —— 那一行留在清单上用文件名画，
/// 而不是把整路作业带停：读者要看的是这批候选，不是某一本为什么开不了。
fn peek_one(
    store: &Store,
    book_dir: Option<&Path>,
    path: &Path,
    shelf: Option<i64>,
) -> Option<Peeked> {
    // 已在架上的那本：库里有现成的那一行与那张封面文件，不必再开一次容器。
    if let Some(id) = shelf {
        let row = store.book(id).ok().flatten()?;
        let cover = store.book_cover(id).ok().flatten();
        return Some(Peeked {
            path: path.to_path_buf(),
            title: row.title,
            authors: row.authors.join(AUTHOR_SEP),
            cover: cover.map(|bytes| shrink_cover(book_dir, bytes)),
        });
    }
    let doc = folio_core::content::document::open_with(path, ParseBudget::AtMost(1)).ok()?;
    let meta = doc.meta();
    let cover = doc.cover().map(<[u8]>::to_vec);
    Some(Peeked {
        path: path.to_path_buf(),
        title: meta.title.clone(),
        authors: meta.authors.join(AUTHOR_SEP),
        cover: cover.map(|bytes| shrink_cover(book_dir, bytes)),
    })
}

/// 把一张封面缩成列表档再交回界面。
///
/// 卡上那一格只有 40×60，而真书的封面常见 800×1200：揣着原图字节过一整晚是几百倍的
/// 浪费（`Image` 持的是**编码字节**，不是解出来的像素）。缩略图落进
/// `<书库>/.thumbs/96/`，文件名按封面字节自己的哈希 —— 与导入那一趟写的是**同一个名**，
/// 所以这本书真被导进来时那张图已经在哪儿了，这一步不白做。
///
/// 书库不在（还没确认位置）、写不进去、字节认不出格式，一律退回原图字节：缩略图是
/// 可丢弃的加速品，不是功能的入口。
fn shrink_cover(root: Option<&Path>, bytes: Vec<u8>) -> Vec<u8> {
    let Some(root) = root else { return bytes };
    let Some(mime) = guess_media_type(&bytes) else {
        return bytes;
    };
    let cover = CoverRef {
        hash: sha256_of_bytes(&bytes).to_hex(),
        mime: mime.to_string(),
    };
    let Some(path) = thumb::ensure(root, THUMB_LIST_PX, &cover, &bytes) else {
        return bytes;
    };
    std::fs::read(&path).unwrap_or(bytes)
}

impl ShelfView {
    /// 点「扫描」。书库位置没确认过就先摆确认面板 —— 勾选导入要往那里拷。
    ///
    /// Android 上多一道门：先在**后台线程**上要「所有文件访问」（请求会把调用
    /// 线程阻塞到读者从系统设置页回来，见 [`crate::ui::platform`]::
    /// request_storage_access），要不到就退回只扫应用自己目录的小范围扫描。
    pub(in crate::ui::features::shelf) fn start_scan(&mut self, cx: &mut Context<Self>) {
        if self.scan_open || self.importing.is_some() {
            // 正在扫或正在导：同一时刻一条线，读数行就是「等一下再点」的提示。
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
            let _ = this.update(cx, |this, cx| {
                if this.scan.take().is_some() {
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    /// 后端扫描的一条事件 → 面板的一次变化。
    fn scan_event(&mut self, event: TaskEvent<ScanReport>, cx: &mut Context<Self>) {
        match event {
            TaskEvent::Started => {}
            TaskEvent::Progress(progress) => {
                if let Some(ScanPhase::Scanning { found, .. }) = &mut self.scan {
                    *found = progress.done;
                    cx.notify();
                }
            }
            TaskEvent::Bytes(_) => {}
            TaskEvent::Finished(Ok(report)) => {
                // 上一批的读数还没开完就换了新清单：旧路径的读数落在新清单上是白做的。
                self.stop_peeks();
                // 「已在书架」按副本路径判：候选落进书库时的那个路径。顺带把库里那一行的
                // 主键拿住 —— 开容器那一路据此决定这一本要不要真的去开文件。
                let book_dir = target_dir(&ConfigHolder::library(cx));
                let store = app_store::handle(cx);
                let rows: Vec<ScanRow> = report
                    .candidates
                    .into_iter()
                    .map(|candidate| {
                        let shelf = match (&store, &book_dir) {
                            (Some(store), Some(dir)) => store
                                .book_by_path(&dir.join(&candidate.name))
                                .ok()
                                .flatten()
                                .map(|row| row.id),
                            _ => None,
                        };
                        ScanRow { candidate, shelf }
                    })
                    .collect();
                let (entries, sizes) = flatten(&rows);
                self.scan = Some(ScanPhase::Results {
                    checked: HashSet::new(),
                    rows,
                    skipped: report.skipped,
                    truncated: report.truncated,
                    entries,
                    sizes,
                    peeked: HashMap::new(),
                });
                // 清单到手 = 有东西可看了，这一屏就抬起来。读者若在扫的途中关掉过它，
                // 结果会把它再开一遍 —— 这是迁浮层**之前**就有的行为（那时判据就是
                // `scan.is_some()`），改的是容器不是这条。真要「扫完不打扰」，那是另一个
                // 决定：得让关掉那一下顺手把作业也停了。
                self.scan_open = true;
                self.start_peeks(book_dir, cx);
                cx.notify();
            }
            TaskEvent::Finished(Err(err)) => {
                tracing::error!("ShelfView: 扫描作业失败 {err:#}");
                self.stop_peeks();
                self.scan = None;
                self.scan_open = false;
                cx.notify();
            }
            TaskEvent::Cancelled => {
                // 扫描可取消、没有副作用：取消就是收摊，不留半个面板。
                tracing::info!("ShelfView: 扫描被取消");
                self.stop_peeks();
                self.scan = None;
                self.scan_open = false;
                cx.notify();
            }
        }
    }

    /// 起那一路「一本一本开容器」。清单空（或库里没有真候选）就什么都不起。
    fn start_peeks(&mut self, book_dir: Option<PathBuf>, cx: &mut Context<Self>) {
        let Some(ScanPhase::Results { rows, .. }) = self.scan.as_ref() else {
            return;
        };
        let Some(store) = app_store::handle(cx) else {
            return;
        };
        let targets: Vec<(PathBuf, Option<i64>)> = rows
            .iter()
            .filter_map(|row| Some((row.path()?.to_path_buf(), row.shelf)))
            .collect();
        if targets.is_empty() {
            return;
        }
        let (sink, rx) = async_channel::unbounded::<Peeked>();
        let handle = submit_peeks(
            &crate::ui::services::resident::pool(),
            store,
            book_dir,
            targets,
            sink,
        );
        let cancel = handle.cancel.clone();
        let reader = cx.spawn(async move |this: WeakEntity<Self>, cx| {
            while let Ok(peeked) = rx.recv().await {
                let _ = this.update(cx, |this, cx| this.record_peek(peeked, cx));
            }
        });
        self.peek = Some(PeekRun {
            cancel,
            _reader: reader,
        });
    }

    /// 收那一路作业。三路都要叫：读者关掉那一屏、勾完点「导入所选」、换了一批新结果
    /// （以及扫描自己失败/被取消）—— 不停就是为一屏已经不在了的卡继续开文件。
    fn stop_peeks(&mut self) {
        if let Some(run) = self.peek.take() {
            run.cancel.trigger();
        }
    }

    /// 一本的读数落进清单，顺手重画：读者看到的是一张一张卡**亮起来**，
    /// 而不是等整批开完再刷一次。
    fn record_peek(&mut self, peeked: Peeked, cx: &mut Context<Self>) {
        let Some(ScanPhase::Results { peeked: map, .. }) = &mut self.scan else {
            // 面板已经换了时刻（收掉了 / 换了一批）：这一条落在地上就行，不值得为它
            // 多留一份状态。
            return;
        };
        let cover = peeked.cover.as_deref().and_then(cover_image);
        map.insert(
            peeked.path,
            Rc::new(Peek {
                title: peeked.title.into(),
                authors: peeked.authors.into(),
                cover,
            }),
        );
        cx.notify();
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

    /// 全选 / 清除（已在架上的不参与 —— 见模块注释）。
    fn toggle_select_all(&mut self, cx: &mut Context<Self>) {
        let Some(ScanPhase::Results { rows, checked, .. }) = &mut self.scan else {
            return;
        };
        let eligible: Vec<PathBuf> = rows
            .iter()
            .filter(|row| row.shelf.is_none())
            .filter_map(|row| match &row.candidate.origin {
                folio_core::Origin::Local(path) => Some(path.clone()),
                _ => None,
            })
            .collect();
        let all = !eligible.is_empty() && eligible.iter().all(|p| checked.contains(p));
        if all {
            checked.retain(|p| !eligible.contains(p));
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
        self.stop_peeks();
        self.dispatch_import(Box::new(folio_core::PathsSource::picked(paths)), cx);
    }

    /// 收掉那一屏回架面。**只**改意图，理由同 [`ShelfView::close_url_panel`]。
    pub(in crate::ui::features::shelf) fn close_scan(&mut self, cx: &mut Context<Self>) {
        self.scan_open = false;
        // 屏收了，为它开的那一路也就没用了（清单留着，退场那半程还要画它已经亮起的那些）。
        self.stop_peeks();
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
                    // 回归门按 id 认这一屏画没画出来（与架面那行读数同一套）。
                    .test_support()
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
                skipped,
                truncated,
                sizes,
                ..
            }) => {
                let count = checked.len();
                let eligible: usize = rows.iter().filter(|row| row.shelf.is_none()).count();
                let all_checked = eligible > 0 && count >= eligible;
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

                let mut head = v_flex()
                    .w_full()
                    .gap_1()
                    .px(px(crate::ui::features::shelf::view::GUTTER))
                    .py_2()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(tr(cx, "shelf.scan_title")),
                    );
                if rows.is_empty() {
                    head = head.child(muted_label(tr(cx, "shelf.scan_none"), cx));
                } else {
                    head = head.child(muted_label(
                        tf(cx, "shelf.scan_summary", &[("n", &rows.len())]),
                        cx,
                    ));
                }
                if *skipped > 0 {
                    head = head.child(muted_label(
                        tf(cx, "shelf.scan_skipped", &[("n", skipped)]),
                        cx,
                    ));
                }
                if *truncated {
                    head = head.child(muted_label(tr(cx, "shelf.scan_truncated"), cx));
                }

                let list_entity = entity.clone();
                v_flex()
                    .id("shelf-scan-panel")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .child(head)
                    .child(div().flex_1().min_h_0().when(!rows.is_empty(), |list| {
                        list.child(
                            v_virtual_list(
                                list_entity,
                                "shelf-scan-rows",
                                sizes.clone(),
                                move |this, range, _window, cx| scan_rows(this, range, cx),
                            )
                            .flex_1(),
                        )
                    }))
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
                            .when(!rows.is_empty(), |foot| {
                                foot.child(
                                    Button::new("shelf-scan-select")
                                        .ghost()
                                        .label(select_label.to_string())
                                        .accessibility_label(select_label)
                                        .on_click(move |_, _, cx| {
                                            select_entity
                                                .update(cx, |this, cx| this.toggle_select_all(cx))
                                        }),
                                )
                                .child(
                                    Button::new("shelf-scan-import")
                                        .primary()
                                        .label(import_label.to_string())
                                        .accessibility_label(import_label)
                                        .on_click(move |_, _, cx| {
                                            import_entity
                                                .update(cx, |this, cx| this.import_checked(cx))
                                        }),
                                )
                            }),
                    )
                    .into_any_element()
            }
            // 面板只在 scan 有值时被调；这条分支是给类型闭合的。
            None => div().into_any_element(),
        }
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
        peeked,
        ..
    }) = &this.scan
    else {
        return Vec::new();
    };
    let foreground = cx.theme().foreground;
    let muted = cx.theme().muted_foreground;
    let border = cx.theme().border;
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
                        .px(px(super::GUTTER))
                        .text_color(muted)
                        .child(dir.clone())
                        .into_any_element(),
                );
            }
            Some(ListEntry::Row(ix)) => {
                let ix = *ix;
                let Some(row) = rows.get(ix) else { continue };
                // 开容器那一路还没回到这一本时，卡上摆的是**文件名**而不是书名 —— 这不是
                // 坏状态，是清单正在一本一本亮起来（封面那一格先留一块灰底）。
                let seen = row.path().and_then(|path| peeked.get(path));
                let name: SharedString = match seen {
                    Some(peek) if !peek.title.is_empty() => peek.title.clone(),
                    _ => row.candidate.name.clone().into(),
                };
                let authors: Option<SharedString> = seen
                    .filter(|peek| !peek.authors.is_empty())
                    .map(|peek| peek.authors.clone());
                let cover = seen.and_then(|peek| peek.cover.clone());
                let size_label: Option<SharedString> =
                    row.candidate.size.map(|bytes| fmt_size(bytes).into());
                let on_shelf = row.shelf.is_some();
                let picked = row.path().is_some_and(|path| checked.contains(path));
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
                                .py(px(ROW_PAD))
                                .rounded(card_radius)
                                // GPUI 不保证圆角的父裁掉孩子：不裁的话封面那一格的方角
                                // 会从卡角外头露出来。
                                .overflow_hidden()
                                .border_1()
                                .border_color(border)
                                .child(
                                    // 点得着的那半张卡：封面 + 那三行。勾选框是它的**兄弟**
                                    // 而不是孩子 —— gpui 把一次点击发给每一个 hitbox 含按下点
                                    // 的 handler，套在里面的话点一下会先勾上再被外层取消掉。
                                    // 封面那一格自己不给说明：旁边那三行写着这本叫什么，
                                    // 灰底不会读成「这本书坏了」。
                                    h_flex()
                                        .id(format!("shelf-scan-pick-{ix}"))
                                        .test_support()
                                        .flex_1()
                                        .min_w_0()
                                        .items_center()
                                        .gap_3()
                                        .cursor_pointer()
                                        .active(|style| style.opacity(0.75))
                                        .on_click(move |_, _, cx| {
                                            row_entity.update(cx, |this, cx| {
                                                this.set_row_checked(ix, !picked, cx)
                                            })
                                        })
                                        .child(
                                            div()
                                                .flex_none()
                                                .w(px(COVER_W))
                                                .h(px(COVER_H))
                                                .overflow_hidden()
                                                .rounded(cx.theme().radius_tokens().sm)
                                                .bg(placeholder)
                                                .when_some(cover, |this, image| {
                                                    this.child(
                                                        img(image)
                                                            .size_full()
                                                            .object_fit(ObjectFit::Cover),
                                                    )
                                                }),
                                        )
                                        .child(
                                            v_flex()
                                                .flex_1()
                                                .min_w_0()
                                                .gap_0p5()
                                                .overflow_hidden()
                                                .child(
                                                    div()
                                                        .w_full()
                                                        .truncate()
                                                        .text_color(foreground)
                                                        .text_sm()
                                                        .font_weight(FontWeight::SEMIBOLD)
                                                        .child(name),
                                                )
                                                .when_some(authors, |this, authors| {
                                                    this.child(
                                                        div()
                                                            .w_full()
                                                            .truncate()
                                                            .text_xs()
                                                            .text_color(muted)
                                                            .child(authors),
                                                    )
                                                })
                                                .child(
                                                    // 第三行只有读数，没有句子：大小与「已在架上」
                                                    // 各占一格，中间隔 8，谁都不跟谁拼成一句话
                                                    // （拼接的译文在九门语言里对不上）。
                                                    h_flex()
                                                        .w_full()
                                                        .items_center()
                                                        .gap_2()
                                                        .overflow_hidden()
                                                        .text_xs()
                                                        .text_color(muted)
                                                        .when_some(size_label, |this, size| {
                                                            this.child(size)
                                                        })
                                                        .when(on_shelf, |this| {
                                                            this.child(tr(
                                                                cx,
                                                                "shelf.scan_on_shelf",
                                                            ))
                                                        }),
                                                ),
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

/// 把候选摊平成「分组头 + 行」的列表项与对应高度表。
fn flatten(
    rows: &[ScanRow],
) -> (
    Rc<Vec<ListEntry>>,
    Rc<Vec<gpui_kit::Size<gpui_kit::Pixels>>>,
) {
    let mut entries = Vec::with_capacity(rows.len() * 2);
    let mut sizes = Vec::with_capacity(rows.len() * 2);
    let mut last_dir: Option<String> = None;
    for (ix, row) in rows.iter().enumerate() {
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
    //! 开容器那一路的三条判据：已在架上的**不开文件**（读库）、开不出来的**不带停**、
    //! 真书开得出读数。这一路是整个界面里唯一会去碰陌生文件的部分，而它坏了屏幕上
    //! 看不出区别 —— 卡上永远有文件名可画，所以只能这样钉。

    use std::path::{Path, PathBuf};

    use folio_core::{DocMeta, Format, Store};

    use super::{peek_one, shrink_cover};

    fn meta(title: &str) -> DocMeta {
        DocMeta {
            title: title.into(),
            authors: vec!["作者甲".into()],
            format: Format::Epub,
            size: 4096,
            ..DocMeta::default()
        }
    }

    /// 已在架上的那本：书目从库里读，**文件在不在都一样**。这里故意给一个磁盘上不存在
    /// 的副本路径 —— 门若建成「先落一个临时文件」那版，就抓不住「忘了走库里那条分支、
    /// 每本都去开一次容器」这件事：文件真在时两版都过，而读者看到的差别是整屏慢半拍。
    #[test]
    fn a_book_already_on_the_shelf_is_read_from_the_store() {
        let store = Store::in_memory().unwrap();
        let id = store
            .add_book(Path::new("/books/ghost.epub"), &meta("书甲"))
            .unwrap();
        let peeked = peek_one(&store, None, Path::new("/books/ghost.epub"), Some(id))
            .expect("库里有这一行就该读出书目");
        assert_eq!(peeked.path, PathBuf::from("/books/ghost.epub"));
        assert_eq!(peeked.title, "书甲");
        assert_eq!(peeked.authors, "作者甲");
        // 没有封面文件就是 `None`：那一格画灰底，不是错误。
        assert!(peeked.cover.is_none(), "这本没落过封面文件");
    }

    /// 开不出来的那本（文件不在了、不是本书）回 `None`，整路作业接着走下一条 ——
    /// 一行的失败不该把清单钉死在半途。
    #[test]
    fn a_file_that_will_not_open_is_skipped_not_fatal() {
        let store = Store::in_memory().unwrap();
        assert!(peek_one(&store, None, Path::new("/books/没有这本.epub"), None).is_none());
        // 空文件也一样：格式认不出，不是「一本没有封面的书」。
        let tmp = tempfile::tempdir().unwrap();
        let empty = tmp.path().join("空.epub");
        std::fs::write(&empty, []).unwrap();
        assert!(peek_one(&store, None, &empty, None).is_none());
    }

    /// 真开一本：一份纯文本落在临时目录里，开出来要有读数、路径要原样带回来。
    ///
    /// 钉的是这一路的**接线**（provider 选得到、`open_with` 那一层递得进去），不钉书名
    /// 具体是什么 —— 那是 TXT 那一族的分章规则（`plans/纯文本小说.md`）的事。
    #[test]
    fn a_plain_text_file_opens_and_reports_a_row() {
        let store = Store::in_memory().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("某书.txt");
        std::fs::write(&path, "第一章\n正文一二三。\n").unwrap();
        let peeked = peek_one(&store, None, &path, None).expect("纯文本该开得出书目");
        assert_eq!(peeked.path, path);
        // 纯文本没有封面：那一格画灰底（与「这本没声明封面」同一表现）。
        assert!(peeked.cover.is_none(), "TXT 不产封面");
    }

    /// 缩略图那一格的两条退路：书库不在、字节认不出格式 —— 都原样交回字节。
    ///
    /// 这一步是**加速品**：做不出 96 那档不该让一张卡没封面，更不该让这一本读不出来。
    #[test]
    fn a_cover_without_a_throwable_thumbnail_stays_as_it_is() {
        let bytes = b"not-a-real-jpeg".to_vec();
        assert_eq!(
            shrink_cover(None, bytes.clone()),
            bytes,
            "书库没确认位置时原样交回"
        );
        assert_eq!(
            shrink_cover(Some(Path::new("/tmp")), bytes.clone()),
            bytes,
            "字节认不出格式时原样交回"
        );
    }
}
