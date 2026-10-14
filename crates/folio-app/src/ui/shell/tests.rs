//! 外壳要钉住的两层：导航栏在哪些档位收起（纯判据，两端都编译），
//! 以及「外壳起得来、标题栏真在视图树里、换语言能重画」这串启动回路。
//!
//! 后半截只在桌面跑：`try_find` 来自 `TestSupportExt`，那个 trait 在本文件是
//! `#[cfg(not(mobile))]` 才挂上的（移动端没有测试支持注册路径）。

/// 回归门：手机上正文压在栈顶时底栏收起，正文也不再为它预留那 56px。
///
/// 宿主到不了 Compact 档（`SizeClass::of` 在桌面把窄窗口夹到 `Medium`，底栏只在真机
/// 长出来），所以钉住渲染时用的那两条判据：收起与否、以及随之而来的预留量。
/// 预留不归零的话，正文底下会白留一条永远滚不进视野的空带；而预留报错了轴，
/// 另一端就会拿它去扣错的那一边。
#[test]
fn the_bottom_bar_yields_to_the_reading_page() {
    use super::{Reserve, immersive_nav, nav_reserve};
    use crate::ui::shell::navigation::{BOTTOM_BAR_HEIGHT, RAIL_WIDTH, SIDEBAR_WIDTH, SizeClass};

    assert!(
        immersive_nav(SizeClass::Compact, true),
        "阅读页在栈顶时底栏应收起"
    );
    assert_eq!(
        nav_reserve(SizeClass::Compact, true),
        Reserve::ZERO,
        "底栏收起后正文不该再预留底栏高"
    );

    // 没进阅读页就是原样：底栏在、预留也还在 —— 而手机上它横躺着，占的是**高**。
    assert!(!immersive_nav(SizeClass::Compact, false));
    assert_eq!(
        nav_reserve(SizeClass::Compact, false),
        Reserve {
            width: 0.0,
            height: BOTTOM_BAR_HEIGHT
        },
        "手机的导航栏占的是正文的高，不是宽"
    );

    // 桌面（Medium / Expanded）的阅读同样收起侧栏 —— 它竖着，占的是**宽**，
    // 而那一条宽正是双页对开要用的。收的是导航，不是目的地：回出有阅读页顶栏的返回。
    assert!(
        immersive_nav(SizeClass::Medium, true),
        "平板与桌面上进阅读页也该把侧栏让出来"
    );
    assert!(immersive_nav(SizeClass::Expanded, true));
    assert_eq!(
        nav_reserve(SizeClass::Medium, true),
        Reserve::ZERO,
        "侧栏收起后正文不该再预留那条宽"
    );
    assert_eq!(nav_reserve(SizeClass::Expanded, true), Reserve::ZERO);
    // 没进阅读页就是原样：侧栏在、预留也还在。
    assert!(!immersive_nav(SizeClass::Expanded, false));
    assert_eq!(
        nav_reserve(SizeClass::Medium, false),
        Reserve {
            width: RAIL_WIDTH,
            height: 0.0
        }
    );
    assert_eq!(
        nav_reserve(SizeClass::Expanded, false),
        Reserve {
            width: SIDEBAR_WIDTH,
            height: 0.0
        }
    );
}

/// 回归门：音量键只在阅读页翻页，别处必须把这一按还给系统。
///
/// 这条分流是整个功能唯一的判据，而两头的失败都安静：阅读页里不翻页＝键按下去没反应，
/// 书架里不转音量＝把系统本来就有的键做成了死键。翻页那一路要进阅读页、音量那一路要进
/// JNI，宿主测试两样都摸不到，所以钉在 [`VolumeKey::of`] 这一层。
/// 「音量上 = 往后一页」的符号也钉在这里 —— 改了口径测试会红，不会悄悄反过来。
#[test]
fn the_volume_keys_page_only_on_the_reading_view() {
    use super::VolumeKey;

    assert_eq!(VolumeKey::of(true, true), VolumeKey::Turn(1));
    assert_eq!(VolumeKey::of(true, false), VolumeKey::Turn(-1));
    assert_eq!(
        VolumeKey::of(false, true),
        VolumeKey::SystemVolume { up: true },
        "不在阅读页时音量键该调音量"
    );
    assert_eq!(
        VolumeKey::of(false, false),
        VolumeKey::SystemVolume { up: false },
        "两个方向都别吞"
    );
}

/// 回归门：报给阅读页的那份扣减里必须含外壳 pad 掉的安全区。
///
/// 阅读页自己量的是**窗口**高，正文盒 = 窗口高 − 报过去的这一条 − 页眉 − 页脚。
/// 少扣一条安全区，那个盒子就比真能画的地方高几十逻辑像素，多出来的一段被裁在
/// 页脚带下面 —— 屏幕上的表现是「已经滚到底了，最后几行还是看不见」。
#[test]
fn the_reading_page_is_told_about_the_safe_areas_too() {
    use super::{Reserve, SizeClass, nav_reserve, reading_reserve};
    use crate::ui::host::Insets;
    use crate::ui::shell::navigation::{BOTTOM_BAR_HEIGHT, SIDEBAR_WIDTH};

    let safe = Insets {
        top: 41.0,
        bottom: 25.0,
        ..Default::default()
    };

    // Compact：底栏 + 上下两条一起扣。底栏收起时那两条还在 —— 那是系统的地盘，
    // 不是应用 chrome，收起导航栏不动它。
    assert_eq!(
        reading_reserve(SizeClass::Compact, false, &safe),
        Reserve {
            width: 0.0,
            height: BOTTOM_BAR_HEIGHT + 41.0 + 25.0
        }
    );
    assert_eq!(
        reading_reserve(SizeClass::Compact, true, &safe),
        Reserve {
            width: 0.0,
            height: 41.0 + 25.0
        }
    );

    // 桌面那一支：导航栏占的是宽，纵向只 pad 上面那条安全区，所以高里只加它。
    assert_eq!(
        reading_reserve(SizeClass::Expanded, false, &safe),
        Reserve {
            width: SIDEBAR_WIDTH,
            height: 41.0
        }
    );

    // 安全区全 0（桌面的真实值）时这条退回 `nav_reserve` 原样：不多扣一个像素。
    assert_eq!(
        reading_reserve(SizeClass::Medium, false, &Insets::ZERO),
        nav_reserve(SizeClass::Medium, false)
    );
}

#[cfg(not(mobile))]
mod booted {
    use crate::ui::features::shelf::ShelfView;
    use crate::ui::shared::config_model::ConfigHolder;
    use crate::ui::shell::AppShell;
    use crate::ui::shell::navigation::Destination;
    use folio_core::infra::i18n::{Locale, t_in};
    use folio_core::infra::task::ProgressEvent;
    use folio_core::{
        Candidate, Config, Language, Library, PathsSource, ScanReport, Store, TaskEvent,
        TaskManager, submit_import,
    };
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext, px, size};

    /// 起一版外壳，顺带把它那个实体也交出来 —— 「切个目的地」这个动作要从它上面调，
    /// 而设置整页正站在「我的」那一个目的地里。
    ///
    /// `desktop_host` 决定装不装平台服务：装了就是「这一端点得住浮层」（桌面那一档），
    /// 没装按点不住算（宿主测试的默认，与手机同一档）。
    ///
    /// 书库位置**显式确认过**：没确认时外壳画的是欢迎页（见
    /// [`the_welcome_screen_gates_the_app_until_a_library_is_chosen`]），
    /// 既有用例要验的是正常的壳路径，不能被欢迎页挡在门外。
    fn boot(cx: &mut TestAppContext, desktop_host: bool) -> (AnyWindowHandle, Entity<AppShell>) {
        cx.update(gpui_kit::init);
        let mut shell = None;
        let handle = cx
            .open_window(size(px(900.), px(600.)), |window, cx| {
                crate::ui::shared::app_store::install(Store::in_memory().expect("内存库"), cx);
                ConfigHolder::install(confirmed_config(), cx);
                if desktop_host {
                    crate::ui::host::install(cx);
                }
                let view = cx.new(|cx| AppShell::boot(window, cx));
                shell = Some(view.clone());
                Root::new(view, window, cx)
            })
            .into();
        (handle, shell.expect("外壳应已建起"))
    }

    /// 已确认书库位置的一份配置。壳里的正常用例都从这里起。
    fn confirmed_config() -> Config {
        Config {
            library: Library {
                dir: "/tmp/folio-books".into(),
                watch: None,
            },
            ..Config::default()
        }
    }

    /// 起一版**首次启动**的外壳：书库位置为空串（＝没确认过），应当停在欢迎页。
    fn boot_first_run(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<AppShell>) {
        cx.update(gpui_kit::init);
        let mut shell = None;
        let handle = cx
            .open_window(size(px(900.), px(600.)), |window, cx| {
                crate::ui::shared::app_store::install(Store::in_memory().expect("内存库"), cx);
                ConfigHolder::install(Config::default(), cx);
                let view = cx.new(|cx| AppShell::boot(window, cx));
                shell = Some(view.clone());
                Root::new(view, window, cx)
            })
            .into();
        (handle, shell.expect("外壳应已建起"))
    }

    fn open_app(cx: &mut TestAppContext) -> AnyWindowHandle {
        boot(cx, false).0
    }

    /// 切到「我的」再画一帧，问那一格里长出的是设置整页还是行列表。
    fn account_shows_the_settings_page(
        cx: &mut TestAppContext,
        handle: AnyWindowHandle,
        shell: &Entity<AppShell>,
    ) -> bool {
        cx.update_window(handle, |_, window, cx| {
            shell.update(cx, |this, cx| this.go(Destination::Account, window, cx));
            window.render_frame(cx);
            window.try_find("folio-settings").is_some()
        })
        .expect("窗口应能渲染")
    }

    /// 回归门：自定义标题栏必须挂在视图树里（曾经的 bug 是窗口选项声明了透明标题栏、
    /// 却没有任何人绘制它，于是顶上什么都没有）。
    #[gpui_kit::test]
    fn the_app_renders_a_custom_title_bar(cx: &mut TestAppContext) {
        let handle = open_app(cx);
        let present = cx
            .update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.try_find("app-title").is_some()
            })
            .expect("窗口应能渲染");
        assert!(present, "标题栏里的应用标题应出现在视图树中");
    }

    /// 回归门：首次启动（书库位置没确认过）时停在欢迎页，不进主界面；选完位置就放行。
    ///
    /// 判据就是 `Library::is_confirmed()` —— 与「导入要不要拦住」同一条。写反的代价是
    /// 读者第一次打开应用直接撞见书架（导入时又被拦下问位置），或者永远卡在欢迎页出不来。
    #[gpui_kit::test]
    fn the_welcome_screen_gates_the_app_until_a_library_is_chosen(cx: &mut TestAppContext) {
        let (handle, shell) = boot_first_run(cx);

        let welcome_shown = cx
            .update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.try_find("folio-welcome").is_some()
            })
            .expect("窗口应能渲染");
        assert!(welcome_shown, "书库位置没确认时应画欢迎页");
        assert!(
            shell.read_with(cx, |this, _| this.onboarding),
            "没确认位置时外壳该停在欢迎页"
        );

        // 选完位置（等价于选择器选中或点「使用默认位置」）：配置一变，门禁就该放行。
        cx.update(|cx| {
            ConfigHolder::edit_library(cx, |library| library.dir = "/tmp/folio-books".into());
        });
        let welcome_shown = cx
            .update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.try_find("folio-welcome").is_some()
            })
            .expect("窗口应能渲染");
        assert!(!welcome_shown, "位置确认后欢迎页应消失");
        assert!(
            !shell.read_with(cx, |this, _| this.onboarding),
            "位置确认后外壳该离开欢迎页、进主界面"
        );
    }

    /// 回归门：返回键先退掉输入框的编辑态，而不是把这一屏弹走。
    ///
    /// Android 上系统返回与软键盘的「完成」都被 fork 翻成 `escape`，而软键盘**只在焦点
    /// 离开输入框时**才收（`gpui-mobile` 的 `text_input_state_changed(FocusLost)`），
    /// gpui-kit 的输入框对 escape 又只做 `cx.propagate()`。两头都不接，读者就看见键盘停在
    /// 屏上收不掉，而同一键还会顺手把当前那一屏弹走（栈底那一档在手机上就是退出应用）。
    #[gpui_kit::test]
    fn back_gives_up_the_field_focus_instead_of_navigating(cx: &mut TestAppContext) {
        let (handle, shell) = boot(cx, false);
        let stack = shell.read_with(cx, |this, _| {
            this.stacks[Destination::Shelf.index()].clone()
        });

        // 焦点走真实那一条路：架面渲染过一帧，搜索框的元素真在树上。
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let shelf = stack
                .read(cx)
                .current()
                .cloned()
                .and_then(|top| top.downcast::<ShelfView>().ok())
                .expect("书架栈底就是架面本身");
            shelf.update(cx, |shelf, cx| shelf.focus_search(window, cx));
            assert!(window.focused(cx).is_some(), "搜索框应真的拿到焦点");
        })
        .expect("窗口应能渲染");

        cx.simulate_keystrokes(handle, "escape");

        cx.update_window(handle, |_, window, cx| {
            assert!(
                window.focused(cx).is_none(),
                "返回该先把编辑态退掉：焦点还在框上时，键盘没有任何一条路能收起来"
            );
        })
        .expect("窗口应能渲染");
        // 退编辑态与弹栈是两件事，一次按键只做前者。
        assert_eq!(
            stack.read_with(cx, |state, _| state.depth()),
            1,
            "退掉焦点的那一次返回不该把这一屏弹走"
        );
        assert!(
            shell.read_with(cx, |this, _| this.reading.is_none()),
            "同上：不该顺手 push 出别的一屏"
        );
    }

    /// 回归门：点在架面的空白上，搜索框的编辑态要退掉。
    ///
    /// 这条走的是**真实嵌入**（书架顶上那颗 [`SearchBox`]）：组件自测钉的是
    /// 判点机制本身，这里钉的是书架没有把它包丢 —— 哪天有人把搜索框换回裸
    /// `Input` 或在外面多套一层容器，判点失聪，真机上的表现又是键盘钉在屏上。
    #[gpui_kit::test]
    fn clicking_blank_on_the_shelf_gives_up_the_search_focus(cx: &mut TestAppContext) {
        let (handle, shell) = boot(cx, false);
        let stack = shell.read_with(cx, |this, _| {
            this.stacks[Destination::Shelf.index()].clone()
        });

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let shelf = stack
                .read(cx)
                .current()
                .cloned()
                .and_then(|top| top.downcast::<ShelfView>().ok())
                .expect("书架栈底就是架面本身");
            shelf.update(cx, |shelf, cx| shelf.focus_search(window, cx));
            assert!(window.focused(cx).is_some(), "搜索框应真的拿到焦点");
        })
        .expect("窗口应能渲染");

        // 窗口正中落一次点击：900×600 的桌面上那是空架说明文所在的空白 ——
        // 框外的空白。（测试观测只注册带交互的元素，架面根上没有可点目标，
        // 所以这里像组件自测一样按坐标落点。）
        cx.update_window(handle, |_, window, cx| {
            let at = gpui_kit::point(px(450.), px(300.));
            window.drag(at, at, cx);
            assert!(
                window.focused(cx).is_none(),
                "点在架面空白上应退掉搜索框的编辑态"
            );
        })
        .expect("窗口应能渲染");
    }

    /// 回归门：后端导入管线交回的东西要真的变成架上一格，那一行导入读数要跟着收掉。
    ///
    /// 两头都是真的：结果来自跑过一遍的 `folio_core::ingest::import`（真的把文件拷进书库、真的
    /// 解析），架面是外壳里挂着的那一个（内存书库）。钉住的是接线之后的三个方向 ——
    /// 事件进模型、架上多一格、读数不再挂着。少任何一环，真机上都是「点了导入，过了几秒
    /// 什么也没发生」，而这正是这条链从界面线程挪去后台之后新引入的风险：以前那一份是
    /// 同步跑完的，屏幕下一帧自然就是新的。
    ///
    /// 起点选在「事件已经上了界面线程」：系统文件选择器在测试窗口里不会返回，那一段只能
    /// 由 [`ShelfView::feed_import_event`] 代劳，剩下的都是真实路径。
    #[gpui_kit::test]
    fn the_backends_import_events_redraw_the_shelf(cx: &mut TestAppContext) {
        let (handle, shell) = boot(cx, false);
        let stack = shell.read_with(cx, |this, _| {
            this.stacks[Destination::Shelf.index()].clone()
        });

        // 一次交两本给后端：一本认得、一本认不出 —— 坏的那本不该把好的挡在架外。
        let tmp = tempfile::tempdir().unwrap();
        let picked = tmp.path().join("原处");
        std::fs::create_dir_all(&picked).unwrap();
        std::fs::write(picked.join("a.txt"), "第一本\n").unwrap();
        std::fs::write(picked.join("b.mobi"), "后端不认这个扩展名\n").unwrap();
        let manager = TaskManager::new(1);
        // 落库在管线里做完：作业跑完，库里已经有那本好的；喂给架面的
        // 只是「结果到了」的信号，架面自己重查库。
        let store = cx
            .update(|cx| crate::ui::shared::app_store::handle(cx))
            .expect("书库应已装上");
        let job = submit_import(
            &manager,
            Box::new(PathsSource::picked(vec![
                picked.join("a.txt"),
                picked.join("b.mobi"),
            ])),
            tmp.path().join("books"),
            store,
        );
        let outcomes = job
            .events
            .into_iter()
            .find_map(|event| match event {
                TaskEvent::Finished(Ok(outcomes)) => Some(outcomes),
                _ => None,
            })
            .expect("导入作业要以 Finished(Ok(..)) 收尾");

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let shelf = stack
                .read(cx)
                .current()
                .cloned()
                .and_then(|top| top.downcast::<ShelfView>().ok())
                .expect("书架栈底就是架面本身");
            assert_eq!(shelf.read(cx).card_count(), 0, "还没导书时架上是空的");
            assert!(
                window.try_find("shelf-importing").is_none(),
                "没在导入时不该有那一行读数"
            );

            shelf.update(cx, |shelf, cx| {
                shelf.feed_import_event(
                    TaskEvent::Progress(ProgressEvent {
                        done: 1,
                        total: 2,
                        message: "导入《第一本》".into(),
                    }),
                    cx,
                );
            });
            window.render_frame(cx);
            assert!(
                window.try_find("shelf-importing").is_some(),
                "进度读数要画出来：这几秒里架面不动，读者只有这一行可看"
            );

            shelf.update(cx, |shelf, cx| {
                shelf.feed_import_event(TaskEvent::Finished(Ok(outcomes)), cx);
            });
            window.render_frame(cx);
            assert!(
                window.try_find("shelf-importing").is_none(),
                "导完那一行读数要收掉"
            );
            assert_eq!(
                shelf.read(cx).card_count(),
                1,
                "好的那本要立刻成为架上一格，坏的那本不该把它挡在架外"
            );
        })
        .expect("窗口应能渲染");
    }

    /// 回归门：扫描清单是「找到 → 勾选 → 导入所选」的那条路，勾中的要走
    /// 与选文件导入同一条管线。
    ///
    /// 扫描结果由 [`ShelfView::feed_scan_event`] 喂进来（真实的整机扫描要扫
    /// 测试机的家目录，那条不是门该扫的），**勾选之后的导入是真跑**：
    /// `import_checked` 把勾中的路径交给后端管线，架上要出现「导入中」的读数。
    /// 面板要在扫描结果到手时出现、在读数出现时收掉 —— 清单在时盖着架面，
    /// 导入开始就该换回「导入中」那一行。
    #[gpui_kit::test]
    fn the_scan_panel_hands_the_checked_rows_to_the_pipeline(cx: &mut TestAppContext) {
        let (handle, shell) = boot(cx, false);
        let stack = shell.read_with(cx, |this, _| {
            this.stacks[Destination::Shelf.index()].clone()
        });

        // 两本候选：一本好的、一本后端不认的 —— 与导入门同一组文件。
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("下载");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "第一本\n").unwrap();
        std::fs::write(dir.join("b.mobi"), "后端不认这个扩展名\n").unwrap();
        let report = ScanReport {
            candidates: vec![
                Candidate {
                    origin: folio_core::Origin::Local(dir.join("a.txt")),
                    name: "a.txt".into(),
                    size: None,
                },
                Candidate {
                    origin: folio_core::Origin::Local(dir.join("b.mobi")),
                    name: "b.mobi".into(),
                    size: None,
                },
            ],
            skipped: 2,
            truncated: false,
        };

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let shelf = stack
                .read(cx)
                .current()
                .cloned()
                .and_then(|top| top.downcast::<ShelfView>().ok())
                .expect("书架栈底就是架面本身");
            assert!(
                window.try_find("shelf-scan-panel").is_none(),
                "没在扫描时不该有面板"
            );

            shelf.update(cx, |shelf, cx| {
                shelf.feed_scan_event(TaskEvent::Finished(Ok(report)), cx)
            });
            window.render_frame(cx);
            assert!(
                window.try_find("shelf-scan-panel").is_some(),
                "结果到手面板要出现"
            );

            // 勾第一行（a.txt）。清单两项同目录：一条头 + 两行，第 1 项就是 a.txt。
            shelf.update(cx, |shelf, cx| shelf.check_scan_row(1, true, cx));
            shelf.update(cx, |shelf, cx| shelf.import_checked_for_test(cx));
            window.render_frame(cx);
            assert!(
                window.try_find("shelf-importing").is_some(),
                "导入所选要接上管线：读数行要出现"
            );
            assert!(
                window.try_find("shelf-scan-panel").is_none(),
                "清单在导入开始时要收掉"
            );
        })
        .expect("窗口应能渲染");
    }

    /// 回归门：切目的地时输入框的编辑态要跟着退。
    ///
    /// 每条导航栈都常驻，切去别的页面只是不画书架了，输入框实体还活着 ——
    /// 焦点不清，手机上软键盘就压在新的一屏上不下来。这一刀在 [`AppShell::go`]
    /// 里，导航栏那一次点击与宿主测试走的是同一条路。
    #[gpui_kit::test]
    fn switching_destination_gives_up_the_field_focus(cx: &mut TestAppContext) {
        let (handle, shell) = boot(cx, false);
        let stack = shell.read_with(cx, |this, _| {
            this.stacks[Destination::Shelf.index()].clone()
        });

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let shelf = stack
                .read(cx)
                .current()
                .cloned()
                .and_then(|top| top.downcast::<ShelfView>().ok())
                .expect("书架栈底就是架面本身");
            shelf.update(cx, |shelf, cx| shelf.focus_search(window, cx));
            assert!(window.focused(cx).is_some(), "搜索框应真的拿到焦点");
        })
        .expect("窗口应能渲染");

        cx.update_window(handle, |_, window, cx| {
            shell.update(cx, |this, cx| this.go(Destination::Stats, window, cx));
            assert!(
                window.focused(cx).is_none(),
                "切走目的地后编辑态不应留着：焦点不清键盘就收不起来"
            );
        })
        .expect("窗口应能渲染");
    }

    /// 回归门：没有焦点时，返回照常弹栈 —— 上面那一档只在真有异议时才吃掉这一键。
    #[gpui_kit::test]
    fn back_still_pops_when_nothing_has_focus(cx: &mut TestAppContext) {
        let (handle, shell) = boot(cx, false);
        let stack = shell.read_with(cx, |this, _| {
            this.stacks[Destination::Shelf.index()].clone()
        });
        shell.update(cx, |this, cx| this.open_reading(1, cx));
        assert_eq!(
            stack.read_with(cx, |state, _| state.depth()),
            2,
            "阅读页应已压上书架栈"
        );

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.focused(cx).is_none(), "点开书之后没有输入框拿着焦点");
        })
        .expect("窗口应能渲染");
        cx.simulate_keystrokes(handle, "escape");

        assert_eq!(
            stack.read_with(cx, |state, _| state.depth()),
            1,
            "没有焦点时返回要弹栈，而不是空转一次"
        );
    }

    /// 回归门：桌面上「我的」那一格是**一整页字段**（`SettingsView`）。
    ///
    /// 独立设置窗口删掉之后，那一整页字段只有这一个落脚处；这条钉住它真的被挂上去了，
    /// 而不是留在原地等一个再也没人调的 `open`。
    #[gpui_kit::test]
    fn the_account_destination_is_the_settings_page_where_overlays_work(cx: &mut TestAppContext) {
        let (handle, shell) = boot(cx, true);
        assert!(
            account_shows_the_settings_page(cx, handle, &shell),
            "桌面那一格应画设置整页"
        );
    }

    /// 回归门：塞不下两栏的那一端，「我的」画的是钻入式列表（[`CompactSettings`]）。
    ///
    /// 桌面的 `Settings` 组件是侧栏加正文两栏，360dp 的小屏装不下；这一端吃同一份
    /// 页面规格、渲染成页列表。数据同源，渲染各归各档 —— 判据写反时这条会红。
    #[gpui_kit::test]
    fn the_account_destination_drills_in_where_the_two_pane_will_not_fit(cx: &mut TestAppContext) {
        let (handle, shell) = boot(cx, false);
        let compact = cx
            .update_window(handle, |_, window, cx| {
                shell.update(cx, |this, cx| this.go(Destination::Account, window, cx));
                window.render_frame(cx);
                window.try_find("settings-compact").is_some()
            })
            .expect("窗口应能渲染");
        assert!(compact, "塞不下两栏的那一端应画出钻入式设置列表");
        assert!(
            !account_shows_the_settings_page(cx, handle, &shell),
            "这一端不该画出两栏整页"
        );
    }

    /// 多语言的验收：导航栏的文案由配置里的语言驱动，改偏好后视图能重画到新语言。
    ///
    /// 不断言侧栏那一格的元素 id —— `SidebarMenuItem` 的 id 是内部拼的
    /// （`"{menu-id}-{ix}"`），不是标签。这里渲染一帧确认能重画，再按当前语言解出
    /// 导航标签，等于同时验了「配置 → locale」与「视图可重画」两件事。
    #[gpui_kit::test]
    fn switching_the_interface_language_changes_the_rendered_labels(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let store = Store::in_memory().expect("内存库");
        let config = Config {
            language: Language {
                locale: Locale::ZhHans,
            },
            ..Default::default()
        };
        let handle: AnyWindowHandle = cx
            .open_window(size(px(900.), px(600.)), |window, cx| {
                crate::ui::shared::app_store::install(store, cx);
                ConfigHolder::install(config.clone(), cx);
                let view = cx.new(|cx| AppShell::boot(window, cx));
                Root::new(view, window, cx)
            })
            .into();

        let nav_shelf = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let locale = ConfigHolder::language(cx).locale;
                t_in(locale, "nav.shelf", &[])
            })
            .expect("窗口应能渲染")
        };
        assert_eq!(nav_shelf(cx), "书架", "偏好是中文时导航标签应为中文");

        cx.update(|cx| {
            ConfigHolder::edit_language(cx, |l| l.locale = Locale::En);
        });
        assert_eq!(nav_shelf(cx), "Shelf", "切英文后导航标签应跟着变");

        cx.update(|cx| {
            ConfigHolder::edit_language(cx, |l| l.locale = Locale::Ja);
        });
        assert_eq!(nav_shelf(cx), "本棚", "日文文案也要生效");
    }

    /// 回归门：切进「统计」应当拿到一份快照并把它画出来。
    ///
    /// 钉的是这三处接线的存在：`go(Stats)` 叫一次 `refresh`、快照拿到了才有一排档位
    /// 按钮、内容区与滚动层各自挂在元素 id 上。空库也算拿到了 —— 那一屏要说的是
    /// 「还没读」，不是「这里空着」。
    #[gpui_kit::test]
    fn the_stats_destination_reads_the_library_it_was_given(cx: &mut TestAppContext) {
        let (handle, shell) = boot(cx, false);
        let drawn = cx
            .update_window(handle, |_, window, cx| {
                shell.update(cx, |this, cx| this.go(Destination::Stats, window, cx));
                window.render_frame(cx);
                (
                    window.try_find("folio-stats").is_some(),
                    window.try_find("stats-ranges").is_some(),
                    shell.read(cx).stats.read(cx).has_report(),
                )
            })
            .expect("窗口应能渲染");
        assert!(drawn.0, "统计那一格应画出自己的根");
        assert!(drawn.1, "拿到快照就该有那排档位按钮（默认档是近 14 天）");
        assert!(drawn.2, "切进来就要拿到一份快照，等下一帧再去查库就晚了");
    }

    /// 一个库里必然不存在的 `book_id`。自增键从 1 起，所以登记一本拿到的就是 1 ——
    /// 测试里「点开一本读不出来的书」不能撞上那本真的在册的，否则 `open_book` 会把
    /// 源文件不在了的那本**连数据一起删**（那是它对「书从架上消失」的正确处置），
    /// 后面那条会话就插不进外键了。
    const NOT_A_BOOK: i64 = 9_999;

    /// 回归门：统计的榜单那一行点开一本书 = 与书架同一个动作。
    ///
    /// 走的是外壳已有的 `open_reading`（往书架栈 push 阅读页），所以这条要同时钉住
    /// 两件事：栈上多了一页，而 `reading` 那格被填上了 —— 少了后者，返回时没人结那
    /// 一笔位置与时长的账。
    #[gpui_kit::test]
    fn a_stats_row_opens_the_book_on_the_shelf_stack(cx: &mut TestAppContext) {
        let (_handle, shell) = boot(cx, false);
        let stack = shell.read_with(cx, |this, _| {
            this.stacks[Destination::Shelf.index()].clone()
        });
        assert_eq!(stack.read_with(cx, |state, _| state.depth()), 1);

        let stats = shell.read_with(cx, |this, _| this.stats.clone());
        cx.update(|cx| {
            stats.update(cx, |_, cx| {
                cx.emit(crate::ui::features::stats::StatsEvent::Open(7))
            })
        });

        assert_eq!(
            stack.read_with(cx, |state, _| state.depth()),
            2,
            "从统计点开一本书应压上书架那条栈，而不是留在报告里"
        );
        assert!(
            shell.read_with(cx, |this, _| this.reading.is_some()),
            "阅读页那格要填上：弹栈时靠它把位置与时长一起结掉"
        );
    }

    /// 回归门：关掉一本书之后统计要跟上，不必等读者再切一次目的地。
    ///
    /// 读者最自然的一次追问就是「我刚读的那几分钟记上了吗」。结算是 pop 那一刻做的
    /// （`ReadingPage::close`），所以同一个回调里必须顺手叫一次重取，否则那一屏
    /// 画的还是进来时的快照，读到的答案永远是上一次。
    #[gpui_kit::test]
    fn closing_the_reading_page_refreshes_the_stats(cx: &mut TestAppContext) {
        let (_handle, shell) = boot(cx, false);
        // 先停在统计上，拿到一份「库里还没有读数」的快照。
        cx.update(|cx| {
            shell.update(cx, |this, cx| {
                this.stats.update(cx, |stats, cx| stats.refresh(cx));
            })
        });
        let before = shell.read_with(cx, |this, cx| this.stats.read(cx).window_sessions());
        assert_eq!(before, Some(0), "内存库里此刻还没有一段阅读");

        // 那一行必须挂在库里真的存在的 `book_id` 上（外键），所以先登记一本。
        // 而**压上栈的那一页**故意用一个读不出来的 id：`open_book` 认不出库里的行
        // 就只写日志、不动别的，弹栈时那次重取照样发生 —— 这条门钉的是「pop 会叫重取」，
        // 不是「书真能打开」。时间戳取现在附近：`reading_summary(14)` 只看最近两周。
        let store = cx.update(|cx| crate::ui::shared::app_store::handle(cx).unwrap());
        let book = store
            .add_book(
                std::path::Path::new("/books/a.epub"),
                &folio_core::DocMeta {
                    title: "A".into(),
                    authors: vec![],
                    format: folio_core::Format::Epub,
                    size: 1,
                    ..folio_core::content::format::DocMeta::default()
                    }
            )
            .unwrap();
        let now = folio_core::infra::time::now();
        shell.update(cx, |this, cx| this.open_reading(NOT_A_BOOK, cx));
        store.record_session(book, now - 600, now, 0).unwrap();
        shell.update(cx, |this, cx| {
            let stack = this.stacks[Destination::Shelf.index()].clone();
            stack.update(cx, |state, cx| {
                state.pop(gpui_kit::base::NavMotion::Immediate, cx)
            });
        });

        let after = shell.read_with(cx, |this, cx| this.stats.read(cx).window_sessions());
        assert_eq!(after, Some(1), "弹掉阅读页那次重取要把新的一段带上来");
    }

    /// 回归门：读者在「信息」那一页改掉书名，架上那张卡要跟着变。
    ///
    /// 钉的是三处接线：那一页写的确实是外壳装上的**同一份库**、写完架面真的重查了一遍、
    /// 重查出来的那一行字是新的。少任何一处的表现都是「改完名字，架上还是旧的那行」，
    /// 而编译与单元测试都看不见它。
    ///
    /// 起点是架面收到「开那一页」的命令（真实入口是卡片长按菜单，测试窗口没有那种
    /// 定位法），中间两格走真的 [`InputState`](gpui_kit::component::input::InputState)，
    /// 写库走真的 [`Store::set_book_meta`]。
    #[gpui_kit::test]
    fn renaming_in_the_info_page_redraws_the_card(cx: &mut TestAppContext) {
        use crate::ui::features::shelf::{BookInfoView, ShelfView};
        use folio_core::content::format::{DocMeta, Format};

        let (handle, shell) = boot(cx, false);
        let stack = shell.read_with(cx, |this, _| {
            this.stacks[Destination::Shelf.index()].clone()
        });
        let store = cx
            .update(|cx| crate::ui::shared::app_store::handle(cx))
            .expect("书库应已装上");
        // 一本名字是导入时抠出来的那种烂名字：正是读者要自己改掉的场景。
        let id = store
            .add_book(
                std::path::Path::new("/books/untitled.epub"),
                &DocMeta {
                    title: "Untitled".into(),
                    authors: vec!["出版社".into()],
                    format: Format::Epub,
                    size: 4096,
                    ..folio_core::content::format::DocMeta::default()
                    }
            )
            .unwrap();

        let shelf = cx
            .update(|cx| {
                stack
                    .read(cx)
                    .current()
                    .cloned()
                    .and_then(|top| top.downcast::<ShelfView>().ok())
            })
            .expect("书架栈底就是架面本身");

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            shelf.update(cx, |shelf, cx| shelf.force_reload(cx));
            assert_eq!(shelf.read(cx).card_count(), 1, "架上该有一格");

            shelf.update(cx, |shelf, cx| shelf.show_info_for_test(id, window, cx));
            window.render_frame(cx);
            assert!(
                window.try_find("folio-book-info").is_some(),
                "点开的那一屏要画出来"
            );

            // 栈顶换成了那一页 —— 导航栏收不收看的正是「栈上压了页没有」，
            // 不是压上来的是谁（外壳那条 `page_pushed`）。
            let page = stack
                .read(cx)
                .current()
                .cloned()
                .and_then(|top| top.downcast::<BookInfoView>().ok())
                .expect("压上来的就是信息那一页");
            page.update(cx, |page, cx| {
                page.rename_for_test("  窄门  ", "安德烈·纪德 · 译者甲", window, cx)
            });
            assert_eq!(
                store.book(id).unwrap().map(|row| row.title).as_deref(),
                Some("窄门"),
                "那一页写的就是外壳装上的那一份库（首尾空白不写进去）"
            );
        })
        .expect("窗口应能渲染");

        // 架面重查那一手在 `Saved` 事件到达时做，而 gpui 把事件排到最外层 update
        // 结束才 flush —— 所以这条断言必须在上面那个闭包**之外**读。
        let title = shelf.read_with(cx, |shelf, _| shelf.card_title(0));
        assert_eq!(title, Some("窄门".into()), "改完书名，架上那一行字要跟着变");

        // 库里也是同一份：作者按人名存，而不是一个名叫「甲 · 乙」的人。
        let row = store.book(id).unwrap().expect("那一本还在");
        assert_eq!(row.title, "窄门");
        assert_eq!(row.authors, ["安德烈·纪德", "译者甲"]);
    }

    /// 回归门：信息那一屏摆的是这本书**说了的那些**书目项，没说的整行不占位。
    ///
    /// 钉的是从 provider 到界面这一整条接线：`DocMeta` 的那几项 → `books` 的那些列
    /// （`Store::add_book`）→ `Store::book_meta` 的投影 → 那一屏的筛选。中间接错的两种
    /// 表现编译与纯函数测试都看不见：书里明明有简介而屏上没有，或反过来把一本什么都没说的
    /// 书摆成十来行空格。
    ///
    /// 两端共用这一屏（`cfg(mobile)` 只剩宿主与平台），桌面这一趟画的就是手机上那一趟。
    #[gpui_kit::test]
    fn the_info_page_shows_only_the_bibliographic_fields_that_have_a_value(
        cx: &mut TestAppContext,
    ) {
        use folio_core::content::format::{DocMeta, Format, Series};

        let (handle, shell) = boot(cx, false);
        let stack = shell.read_with(cx, |this, _| {
            this.stacks[Destination::Shelf.index()].clone()
        });
        let store = cx
            .update(|cx| crate::ui::shared::app_store::handle(cx))
            .expect("书库应已装上");
        let full = store
            .add_book(
                std::path::Path::new("/books/full.epub"),
                &DocMeta {
                    title: "窄门".into(),
                    authors: vec!["安德烈·纪德".into()],
                    contributors: vec!["译者甲".into()],
                    subjects: vec!["法国小说".into(), "诺贝尔文学奖".into()],
                    description: "一段简介。".into(),
                    publisher: "上海译文出版社".into(),
                    published: "1909".into(),
                    language: "fr".into(),
                    isbn: "978-7-5327-0000-0".into(),
                    // 库里存着、界面上按设计不摆的那一项。
                    dctype: "text".into(),
                    rights: "© 出版社".into(),
                    series: Some(Series {
                        name: "译文经典".into(),
                        position: "3".into(),
                    }),
                    format: Format::Epub,
                    size: 4096,
                },
            )
            .unwrap();
        // 同一屏的第二本：书目区里什么都没写，只有文件名与格式大小。
        let bare = store
            .add_book(
                std::path::Path::new("/books/bare.txt"),
                &DocMeta {
                    title: "一段笔记".into(),
                    format: Format::Text,
                    size: 128,
                    ..DocMeta::default()
                },
            )
            .unwrap();
        let shelf = cx
            .update(|cx| {
                stack
                    .read(cx)
                    .current()
                    .cloned()
                    .and_then(|top| top.downcast::<ShelfView>().ok())
            })
            .expect("书架栈底就是架面本身");

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            shelf.update(cx, |shelf, cx| shelf.show_info_for_test(full, window, cx));
            window.render_frame(cx);
            for field in [
                "series",
                "contributors",
                "subjects",
                "description",
                "publisher",
                "published",
                "language",
                "isbn",
                "rights",
                "format",
                "size",
            ] {
                let id = format!("book-info-{field}");
                assert!(
                    window.try_find(id.clone()).is_some(),
                    "说了的那一项该摆出来：{id}"
                );
            }
            assert!(
                window.try_find("book-info-dctype").is_none(),
                "dc:type 存进库，但不摆进界面"
            );

            // 换一本书目区全空的重画一遍。格式与大小是文件自己的两行，永远在 ——
            // 它们同时是这条门的对照组：`try_find` 只认标了 `test_support` 的元素，
            // 没有正例时「全都不在」也能一路绿到底。
            stack.update(cx, |state, cx| {
                state.pop(gpui_kit::base::NavMotion::Immediate, cx)
            });
            shelf.update(cx, |shelf, cx| shelf.show_info_for_test(bare, window, cx));
            window.render_frame(cx);
            for field in [
                "series",
                "contributors",
                "subjects",
                "description",
                "publisher",
                "published",
                "language",
                "isbn",
                "rights",
            ] {
                let id = format!("book-info-{field}");
                assert!(
                    window.try_find(id.clone()).is_none(),
                    "没说的那一项不该占一行：{id}"
                );
            }
            for field in ["format", "size"] {
                let id = format!("book-info-{field}");
                assert!(
                    window.try_find(id.clone()).is_some(),
                    "文件自己的那两行该在：{id}"
                );
            }
        })
        .expect("窗口应能渲染");
    }
}
