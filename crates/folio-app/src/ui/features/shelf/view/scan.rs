//! 整机扫描的候选清单那一屏。
//!
//! 扫描**只找不导**（`folio_core::ingest::scan`）：找到的候选摆成一张按目录分组的清单，
//! 读者勾选之后走与「选文件导入」同一条 [`ShelfView::dispatch_import`] —— 勾选是
//! 读者亲手挑的，所以是 Picked 语义（重名算更新）。已在上架的那几条**默认不勾**
//! 并标出来：再导一遍等于用同一份文件更新，多半不是读者点扫描想干的事。
//!
//! 清单是虚拟列表：一次整机扫描找到几千本是正常的，只画视口内那几十行。

use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;

use folio_core::{Cancel, Candidate, Priority, ScanReport, TaskEvent, TaskManager, find};
use gpui_kit::base::v_virtual_list;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, Styled as _, TestSupportExt as _, WeakEntity, div, px, size,
};

use crate::ui::services::app_store;
use crate::ui::services::config::ConfigHolder;
use crate::ui::services::l10n::{tf, tr};
use crate::ui::services::library::{scan_noise_exempt, scan_roots, target_dir};
use crate::ui::components::muted_label;

use super::ShelfView;

/// 一行候选的高度（逻辑像素）。虚拟列表按它画视口。
const ROW_HEIGHT: f32 = 36.0;
/// 一条目录分组头的高度。
const HEADER_HEIGHT: f32 = 28.0;

/// 清单里的一行：一个候选 + 它在不在架上。
pub(crate) struct ScanRow {
    pub candidate: Candidate,
    pub on_shelf: bool,
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
        /// 读不了（权限）而跳过的处数，界面上那句「跳过 N 处」。
        skipped: usize,
        /// 命中预算被截断，结果不完整。
        truncated: bool,
        /// 按目录分组摊平的列表项，与每项的高度 —— 结果到手时算一次。
        entries: Rc<Vec<ListEntry>>,
        sizes: Rc<Vec<gpui_kit::Size<gpui_kit::Pixels>>>,
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
            &crate::ui::services::services::pool(),
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
                // 「已在书架」按副本路径判：候选落进书库时的那个路径。
                let dest_dir = target_dir(&ConfigHolder::library(cx));
                let store = app_store::handle(cx);
                let rows: Vec<ScanRow> = report
                    .candidates
                    .into_iter()
                    .map(|candidate| {
                        let on_shelf = match (&store, &dest_dir) {
                            (Some(store), Some(dir)) => store
                                .book_by_path(&dir.join(&candidate.name))
                                .map(|row| row.is_some())
                                .unwrap_or(false),
                            _ => false,
                        };
                        ScanRow {
                            candidate,
                            on_shelf,
                        }
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
                });
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
            .filter(|row| !row.on_shelf)
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
                    folio_core::Origin::Local(path) if checked.contains(path) => {
                        Some(path.clone())
                    }
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
        self.dispatch_import(Box::new(folio_core::PathsSource::picked(paths)), cx);
    }

    /// 收掉那一屏回架面。**只**改意图，理由同 [`ShelfView::close_url_panel`]。
    pub(in crate::ui::features::shelf) fn close_scan(&mut self, cx: &mut Context<Self>) {
        self.scan_open = false;
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
                let eligible: usize = rows.iter().filter(|row| !row.on_shelf).count();
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

/// `[range.start, range.end)` 这几行清单。分组头与候选行都在这里画。
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
                let (name, size, path, on_shelf) = match rows.get(ix) {
                    Some(ScanRow {
                        candidate:
                            Candidate {
                                origin, name, size, ..
                            },
                        on_shelf,
                    }) => match origin {
                        folio_core::Origin::Local(path) => (name, *size, path.clone(), *on_shelf),
                        // 扫描只交本地候选；真来了远端的就空一行，别让它毁掉整张清单。
                        folio_core::Origin::Remote { .. } => continue,
                    },
                    None => continue,
                };
                let checked = checked.contains(&path);
                let row_entity = entity.clone();
                let label: SharedString = match size {
                    Some(bytes) => tf(
                        cx,
                        "shelf.scan_row",
                        &[("name", name), ("size", &fmt_size(bytes))],
                    )
                    .into(),
                    None => name.clone().into(),
                };
                out.push(
                    h_flex()
                        .id(("shelf-scan-row", ix))
                        .h(px(ROW_HEIGHT))
                        .w_full()
                        .px(px(super::GUTTER))
                        .gap_2()
                        .items_center()
                        .child(
                            Checkbox::new(("shelf-scan-check", ix))
                                .checked(checked)
                                .on_click(move |checked: &bool, _, cx| {
                                    row_entity.update(cx, |this, cx| {
                                        this.set_row_checked(ix, *checked, cx)
                                    })
                                }),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(foreground)
                                .child(label),
                        )
                        .when(on_shelf, |row| {
                            row.child(
                                div()
                                    .text_color(muted)
                                    .child(tr(cx, "shelf.scan_on_shelf").to_string()),
                            )
                        })
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
        let dir = match &row.candidate.origin {
            folio_core::Origin::Local(path) => path
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            _ => String::new(),
        };
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
