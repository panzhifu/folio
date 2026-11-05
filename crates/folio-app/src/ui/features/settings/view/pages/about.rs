//! 关于：这份二进制是什么。
//!
//! 分组照的是参照应用同一页的形状 —— **版本与更新在这一页**，界面语言不自成一页：读者到这一页
//! 要问的是「关于这个应用」这两件事。 **界面语言 2026-11-05 搬去了「外观」**：它是要改的
//! 东西，不是一条关于这份二进制的事实。
//!
//! 这里没有那颗「检查更新」的开关，也没有许可证那一组：本应用还没有更新检查与授权子系统，
//! 为了对齐样式先摆一颗点了没用的控件是错的。依赖那几行版本（GPUI 本体、组件层、移动平台层
//! 各是哪个 commit）也不摆：读者要的是「手上这份是多少」，那三串是排障时的话。
//!
//! **配置文件路径那一格在这一版被撤掉了**，不是没想到。撤它的代价要说清：桌面上配置根
//! （`~/.config/folio`）与数据根（`~/.local/share/folio`）是**两个目录**，「存储」那一屏报的是
//! 数据根那一个，所以这一格撤掉之后，配置根的**路径字面**在界面上不再出现 —— 主题 JSON 那个
//! 目录（`<配置根>/themes`）仍能从「外观」那一格打开，但要看清那串路径得走「反馈日志」：
//! 日志快照的头部把**配置根与数据根两条路径都写进去**，那正是读者需要这串字的唯一场景
//! （「排障时把文件交给作者」）。同一格里那颗「打开所在文件夹」在手机上还是尽力而为，
//! 失败时读者对着自己抄不完整的字，那不是一个功能。
//!
//! 撤掉这一格**不产生孤儿代码**：`open_folder_button` 仍在「存储」与「外观」两屏用着
//! （数据目录、主题目录）。

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::text::TextView;
use gpui_kit::component::{Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, ClipboardItem, Entity, InteractiveElement as _, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement as _, Styled, div,
};

use crate::ui::components::muted_label;
use crate::ui::platform;
use crate::ui::services::feedback;
use crate::ui::services::update::{self, Status};

use super::super::*;

pub(in crate::ui::features::settings) fn about_page(cx: &App, notes: &Entity<NotesSheet>) -> Page {
    page(
        tr(cx, "page.about"),
        IconName::Info,
        vec![version_group(cx), update_group(cx, notes)],
    )
}

/// 「关于」这一组：跑着的这一份二进制是什么版本。
fn version_group(cx: &App) -> Group {
    group(
        tr(cx, "page.about"),
        vec![value_item(
            tr(cx, "about.version"),
            version(),
            Some(tr(cx, "about.version_desc")),
            &["version", "build", "版本"],
        )],
    )
}

/// 本应用版本（`Cargo.toml` 里那一份 workspace 版本）：编译期就定死在这份二进制里，
/// 不手写第二份，所以它不会和实际跑着的东西分叉。
fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// 一行「读得出但改不了」的事实：标签在左，值在右 —— 与每项字段那一行的动作位同一侧。
fn value_item(
    label: SharedString,
    value: &str,
    description: Option<SharedString>,
    keywords: &'static [&'static str],
) -> Row {
    let mut row = Row::info(label, value);
    if let Some(description) = description {
        row = row.description(description);
    }
    row.keywords(keywords)
}

/// 「更新」这一组：一颗检查按钮，与它念出来的那句状态。
///
/// 发现新版本时这一格**就地**摊开一块面板（版本号、这一端有没有包、正文、两颗按钮）。面板
/// 不挪进弹层：读者按下的就是这一格，「有没有新版、这一端能不能下」那一眼的答案该出现在
/// 他视线停留的地方，而不是藏在另一层里。正文那一段现在按 Markdown 排 —— 与「更新日志」
/// 那张卡片同一件元素，不再手写「认三样」的降级版。
///
/// 「更新日志」那一格开的是 [`NotesSheet`]（一块从底边升起来的卡片，整篇正文摊在里面）。
/// 卡片要宿主，桌面与紧凑两屏因此**各挂了一份**：`BottomSheet` 的用法是「功能自己持有实体、
/// 在自己的 `render` 里挂」（`shelf/view/mod.rs:221-327` 与 `:1375-1381`），句柄由建它的那一屏
/// 经 [`spec_pages`](super::super::spec_pages) 交给这一页的规格，而不是全进程共用一份 ——
/// 两屏可以同时活着（「我的」那一个目的地的根 + 从阅读页压进来的那一屏），共用一份时读者
/// 关掉的是头顶那一屏，沉下去的却是底下那一屏的内容。
///
/// 状态读的是 `services::update` 那个实体，所以这一格会自己重绘 —— 观察者挂在
/// `settings/view/desktop.rs` 与 `compact.rs` 的构造上（那条链的所以然写在服务层顶部）。
fn update_group(cx: &App, notes: &Entity<NotesSheet>) -> Group {
    // 「更新日志」那一格要按下的正是**挂这一屏的**那一张卡片，所以句柄在这里落成一份拥有权
    // 交给规格的闭包（闭包要 `'static`，借用带不进去）。
    let card = notes.clone();
    group(
        tr(cx, "update.group"),
        vec![
            Row::custom_titled(tr(cx, "update.group"), move |cx| {
                let handle = update::handle(cx);
                let status = handle
                    .as_ref()
                    .map(|entity| entity.read(cx).status().clone())
                    .unwrap_or_default();
                let busy = status == Status::Checking;
                let newer = matches!(status, Status::Newer);

                let row = h_flex()
                    .w_full()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(muted_label(status_label_text(&status, cx), cx)),
                    )
                    .child(
                        Button::new("about-check-update")
                            .label(tr(cx, "update.check").to_string())
                            .ghost()
                            .small()
                            .disabled(busy)
                            .on_click(|_, _, cx| update::check(cx)),
                    );

                if newer {
                    // 面板挂在同一格里而不是另起一屏：读者按下的就是这一格，
                    // 结果出现在他视线停留的地方。
                    return v_flex()
                        .w_full()
                        .gap_2()
                        .child(row.into_any_element())
                        .child(newer_panel(cx).into_any_element())
                        .into_any_element();
                }
                row.into_any_element()
            })
            .description(tr(cx, "update.desc"))
            .keywords(&[
                "update", "check", "version", "release", "更新", "检查", "版本", "新版",
            ]),
            // 「更新日志」独立一格，**不挂在检查后面**：读者到这一页要问的是「我手上这版改了
            // 什么」，那件事不需要先跑一次版本比较。正文按 `v{当前版本}` 那条 tag 取，
            // 所以装了 0.1.1 而最新是 0.2.0 时，这里讲的仍是 0.1.1。
            Row::custom_titled(tr(cx, "update.notes"), move |cx| {
                // 规格里那两层的闭包都要 `'static`，所以借用在这一格就换成一份拥有权；
                // 每一帧一份、每一次点击一份，`Entity` 的 clone 只是一次引用计数。
                let card = card.clone();
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(muted_label(tr(cx, "update.notes_hint").to_string(), cx)),
                    )
                    .child(
                        Button::new("about-show-notes")
                            .label(tr(cx, "update.view").to_string())
                            .ghost()
                            .small()
                            .on_click(move |_, window, cx| {
                                // 一颗按钮可以按很多次，而 `with_notes` 收的是 `FnOnce`。
                                let card = card.clone();
                                update::with_notes(window, cx, move |payload, _window, cx| {
                                    // 卡片是**拿到正文之后**才抬起来的（`with_notes` 里那三条
                                    // 分支），不先开一张空卡片赌它会不会自己重画。抬的是
                                    // **挂这一屏的那一份**，不是全进程那一份 —— 理由写在
                                    // `notes_sheet` 顶部。
                                    card.update(cx, |card, cx| card.open(payload, cx));
                                })
                            }),
                    )
                    .into_any_element()
            })
            .description(tr(cx, "update.notes.desc"))
            .keywords(&[
                "changelog",
                "notes",
                "release",
                "log",
                "更新日志",
                "日志",
                "改动",
                "说明",
            ]),
            // 「反馈日志」也落在这一组：读者到这一屏要办的事是「这应用不对劲，我要说一声」，
            // 那一格是上面两格的下一步。
            //
            // **隐私那句话摆在按下之前**：日志里有《书名》与落盘路径（`import:` 那些行带的
            // 就是它们，见 `folio-core/src/ingest/import/pipeline.rs:974`），事后告知等于没告知。
            Row::custom_titled(tr(cx, "feedback.title"), move |cx| {
                let status = feedback::handle(cx)
                    .map(|entity| entity.read(cx).status().clone())
                    .unwrap_or_default();
                let busy = status == feedback::FeedbackStatus::Working;
                let shown = feedback::status_label(&status, cx);
                let mut body = v_flex().w_full().gap_1().child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(muted_label(shown.to_string(), cx)),
                        )
                        .child(
                            Button::new("about-send-logs")
                                .label(tr(cx, "feedback.send").to_string())
                                .ghost()
                                .small()
                                .disabled(busy)
                                .on_click(|_, _, cx| feedback::send(cx)),
                        ),
                );
                // 桌面那两档要把**路径本身**摆出来：那句「请把这个文件作为附件」没有路径
                // 就是一句空话，而 `mailto:` 带不了附件是协议的事，不是这里能替读者省掉的。
                if let Some(path) = feedback::status_path(&status) {
                    body = body.child(
                        div()
                            .w_full()
                            .child(muted_label(path.display().to_string(), cx)),
                    );
                }
                body.child(div().child(muted_label(tr(cx, "feedback.privacy").to_string(), cx)))
                    .into_any_element()
            })
            .description(tr(cx, "feedback.desc"))
            .keywords(&[
                "feedback", "log", "logs", "report", "bug", "反馈", "日志", "问题", "报错",
            ]),
        ],
    )
}

/// 那一格右侧的一句状态。文案表在服务层（两屏共用一份），这里只是把它变成字。
fn status_label_text(status: &Status, cx: &App) -> String {
    update::status_label(status, cx).to_string()
}

/// 发现新版本时摊开的那块：版本号、这一端有没有包、正文、两颗按钮。
fn newer_panel(cx: &App) -> AnyElement {
    // 自己再取一次句柄而不是从调用处递 `Option`：这一格只在 Newer 时被画，句柄却可能在
    // 那一刻刚好没有（测试里直接构造页面而不装服务）。**没有就画空**，不 expect ——
    // 一次设置页的 panic 会把整个应用带走，而那件事的起因只是少装了一个 Global。
    let Some(entity) = update::handle(cx) else {
        return div().into_any_element();
    };
    let state = entity.read(cx);
    let Some(release) = state.found() else {
        return div().into_any_element();
    };
    let version = release.version.to_string();
    let notes = release.notes.trim().to_owned();
    // 这一端有没有可下的包，决定那句「请进行更新」怎么说得出口。
    let has_asset = state.asset().is_some();
    let hint = if has_asset {
        tr(cx, "update.available")
    } else {
        tr(cx, "update.no_asset")
    };
    let page_url = folio_core::net::update::RELEASES_PAGE.to_owned();

    v_flex()
        .w_full()
        .gap_2()
        .child(muted_label(format!("v{version} · {}", hint), cx))
        .child(
            div()
                // 滚动要先有 `id`：`overflow_y_scroll` 挂在 `Stateful<Div>` 上，
                // 裸 `div()` 没那个方法（编译期就拦着，不是运行时才看得见）。
                .id("about-update-notes")
                .w_full()
                .max_h_48()
                .overflow_y_scroll()
                // 正文交给框架的富文本元素排：标题、粗体、嵌套的项目符号都认得。手写那一版
                // 只认三样，屏上就把 `**听书（合成朗读）。**` 那两串星号原样摆出来，而
                // `CHANGELOG.md` 里每条重点都是这个形状。与「更新日志」那张卡片同一件元素。
                .child(if notes.is_empty() {
                    muted_label(tr(cx, "update.notes_missing").to_string(), cx).into_any_element()
                } else {
                    TextView::markdown("about-update-notes-body", notes)
                        .w_full()
                        .into_any_element()
                }),
        )
        // 只有 Android 摆这颗：桌面这一版刻意不自动替换正在运行的二进制（README「发版」），
        // 而 iOS / HarmonyOS 没有这个口子。判据在 `platform::HAS_SELF_INSTALL`，只写那一处。
        .child(when_self_install(state.download(), cx))
        .child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("about-open-releases")
                        .label(tr(cx, "update.open_page").to_string())
                        .ghost()
                        .small()
                        .on_click({
                            let url = page_url.clone();
                            move |_, _, _| {
                                if !platform::open_url(&url) {
                                    tracing::warn!("打不开发布页，读者只能自己复制：{url}");
                                }
                            }
                        }),
                )
                .child(
                    Button::new("about-copy-releases-url")
                        .label(tr(cx, "update.copy_url").to_string())
                        .ghost()
                        .small()
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(page_url.clone()));
                        }),
                ),
        )
        .into_any_element()
}

/// 「下载并安装」那一行：Android 才有，其余端画空元素。
///
/// 进度那句直接念 `update.downloading` 而不拼百分比：`total` 来自资产表里的 `size`，
/// 而 GitHub 那个字段在少数包上是 0 —— 拿 0 去除会念出「下了 12 MB 的 0%」这种话。
/// 宁可少报一个数，也不要报一个编出来的数。
fn when_self_install(download: &update::Download, cx: &App) -> AnyElement {
    if !platform::HAS_SELF_INSTALL {
        return div().into_any_element();
    }
    let busy = matches!(download, update::Download::Running { .. });
    // 闭包要 'static，所以这里把它算成一个 **bool** 再捕获，而不是捕获那个借用。
    let install_step = matches!(
        download,
        update::Download::Ready | update::Download::NeedsPermission
    );
    let (button, shown) = if install_step {
        // 校验过了，动词就得换：再写「下载」是在说一件已经完成的事。
        (
            "update.install",
            update::download_label(download, cx).to_string(),
        )
    } else {
        (
            "update.download",
            update::download_label(download, cx).to_string(),
        )
    };
    v_flex()
        .w_full()
        .gap_1()
        .child(muted_label(shown, cx))
        .child(
            h_flex().gap_2().child(
                Button::new("about-download-update")
                    .label(tr(cx, button).to_string())
                    .ghost()
                    .small()
                    .disabled(busy)
                    .on_click(move |_, _, cx| {
                        // 校验过的那一颗走的是「交给安装器」，不是再下一遍。
                        // `NeedsPermission` 也走这一条：读者从系统设置回来再点，就是重试授权。
                        if install_step {
                            update::install_verified(cx);
                        } else {
                            update::download_and_install(cx);
                        }
                    }),
            ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    //! 「关于」那一屏只剩版本与更新：语言那一格 2026-11-05 搬去「外观」，
    //! 它的选项表与搜索词由那边的测试看着。

    use super::*;

    use std::time::Duration;

    // 下面那条「把这一屏画一帧」的测试要的东西。**在模块顶层导入而不是函数体里**：
    // 函数签名的 `&mut TestAppContext` 是在模块作用域里解析的。
    use crate::ui::features::settings::{SettingsPage, SettingsView};
    use crate::ui::services::config::ConfigHolder;
    use folio_core::Config;
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{AppContext as _, TestAppContext, px, size};

    /// 把「关于」那一屏真画一帧，问三颗按钮在不在。
    ///
    /// 为什么值得单开一条：编译过不等于**渲染得出来**。这三格里有 `with_notes` 那条异步链、
    /// 一颗要把**本屏那份** `NotesSheet` 句柄 clone 进回调的按钮、还有一个只在 `Newer` 时
    /// 才出现的面板（面板里那一栏 Markdown 也是渲染时才建的）—— 而设置页的取值闭包是
    /// **渲染时**才跑的，不画一帧就一行都没执行过。
    #[gpui_kit::test]
    fn the_about_page_grows_its_three_action_rows(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| ConfigHolder::install(Config::default(), cx));
        let handle = cx
            .open_window(size(px(900.), px(600.)), |window, cx| {
                let view = cx.new(|cx| SettingsView::new(SettingsPage::About, cx));
                Root::new(view, window, cx)
            })
            .into();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find("about-check-update").is_some(),
                "「检查更新」那颗按钮没长出来"
            );
            assert!(
                window.try_find("about-show-notes").is_some(),
                "「更新日志」那颗按钮没长出来"
            );
            assert!(
                window.try_find("about-send-logs").is_some(),
                "「反馈日志」那颗按钮没长出来"
            );
            assert!(
                window.try_find("about-download-update").is_none(),
                "「下载并安装」只属于 Android；桌面摆一颗按下去什么都不会发生的按钮，\
                 读者读到的是「这个应用的更新坏了」（判据在 platform::HAS_SELF_INSTALL）"
            );
            assert!(
                window.try_find("about-open-releases").is_none(),
                "什么都没查过就不该有「打开下载页」—— 那一颗只属于发现新版本之后的那块面板"
            );
        })
        .expect("窗口应能渲染");
    }

    /// 按下「查看」该抬起**这一屏自己挂着的那张卡片**，里面交的是正文。
    ///
    /// 为什么值得钉这一条：句柄是从规格里 clone 进回调的，接错（抬了另一屏那一份、或者
    /// 根本没抬）的症状是「按了没反应」，而编译与上面那条「三颗按钮在不在」都照旧绿。
    /// 取数走 `with_notes` 的**已缓存**那一条分支（[`update::seed_notes`] 预装正文），
    /// 这一条因此不发任何请求。
    #[gpui_kit::test]
    fn the_notes_button_raises_the_card_this_screen_mounts(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
            update::seed_notes("### 新增\n- **听书（合成朗读）。**", cx);
        });
        let handle = cx
            .open_window(size(px(900.), px(600.)), |window, cx| {
                let view = cx.new(|cx| SettingsView::new(SettingsPage::About, cx));
                Root::new(view, window, cx)
            })
            .into();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("about-show-notes", cx);
        })
        .expect("窗口应能渲染");
        // 弹簧按测试钟计步：不推开钟它永远停在 0，卡片看着就像「没升起来」。
        for _ in 0..40 {
            cx.executor().advance_clock(Duration::from_millis(16));
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                .expect("渲染应成功");
        }
        cx.update_window(handle, |_, window, _cx| {
            let surface = window
                .try_find("bottom-sheet-surface")
                .expect("按下「查看」该把更新日志那张卡片抬起来");
            // 卡片要占到整屏的大部分：正文区从前写死 384 逻辑像素（Dialog 里的 `max_h_96`），
            // `CHANGELOG.md` 0.1.1 那一节要滚七八趟才算读完。判成比例而不是绝对数，
            // 因为窗口多高是宿主的事。
            let height = f32::from(surface.bounds().size.height);
            let viewport = f32::from(window.viewport_size().height);
            assert!(
                height > viewport * 0.7,
                "展开那一档该是视口高的 85%，实测 {height} / {viewport}"
            );
            assert!(
                window.try_find("update-notes-body").is_some(),
                "抬起来时要把正文交给 Markdown 那一栏"
            );
        })
        .expect("窗口应能渲染");
    }
}
