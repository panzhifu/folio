//! 外壳要钉住的两层：读书与导航栏/架面栈互不牵连的那些判据（纯函数，两端都编译），
//! 以及「外壳起得来、书盖得上也收得回、标题栏真在视图树里、换语言能重画」这串启动回路。
//!
//! 后半截只在桌面跑：`try_find` 来自 `TestSupportExt`，那个 trait 在本文件是
//! `#[cfg(not(mobile))]` 才挂上的（移动端没有测试支持注册路径）。

/// 回归门：读书这件事**不再牵动**应用内那条导航栏。
///
/// 这一条钉的就是阅读层化要买的东西本身：那一层盖在导航栏之上，「收起导航栏给正文腾地方」
/// 已经没有意义；而它一旦留着，退场时就没有任何东西能挤动正在滑走的那一页 —— 上一版
/// 「返回时底部那条进度被往上顶」跳的正是这一处的容器。
///
/// 宿主到不了 Compact 档（`SizeClass::of` 在桌面把窄窗口夹到 `Medium`，底栏只在真机
/// 长出来），所以钉住渲染用的那两条判据：沉浸跟着书、预留不含导航栏。
#[test]
fn the_book_does_not_ask_the_nav_bar_to_move() {
    use super::{Reserve, SizeClass, immersive_nav, reading_reserve};
    use crate::ui::host::Insets;

    // 系统级那一层跟着书：挂着就沉浸，没书就还给系统。
    assert!(immersive_nav(SizeClass::Compact, true));
    assert!(
        immersive_nav(SizeClass::Expanded, true),
        "平板与桌面上开着书同样沉浸"
    );
    assert!(!immersive_nav(SizeClass::Compact, false));
    assert!(!immersive_nav(SizeClass::Expanded, false));

    // 而报给正文的那一条**永远不含导航栏**：宽恒为 0。底栏那 56 与侧栏那 260 都还在
    // 原地画着，只是被这一层盖住 —— 报错了轴的那一步正是上一版让两端各扣错一条边的地方。
    let safe = Insets {
        top: 41.0,
        bottom: 25.0,
        ..Default::default()
    };
    assert_eq!(
        reading_reserve(&safe),
        Reserve {
            width: 0.0,
            height: 41.0 + 25.0
        },
        "宽恒为 0（导航栏由这一层盖住，不再向正文报），高是顶上与底下两条安全区"
    );
}

/// 回归门：背光那一格只在**书挂着**的时候接管，书收了必须交还系统。
///
/// 失败的样子是安静的坏：读完一本书回到架面，屏幕还停在读者为那本书调的那一档暗，而应用
/// 以为自己从没接管过背光。它与 [`immersive_nav`] 共用「跟着书」那条判据，也同在宿主摸不到
/// 渲染的那一层（真机才长得出那一屏），所以钉在自由函数上。
#[test]
fn the_brightness_follows_the_book_and_hands_back_when_it_closes() {
    use super::reading_brightness;

    assert_eq!(reading_brightness(true, Some(0.4)), Some(0.4));
    assert_eq!(
        reading_brightness(true, None),
        None,
        "读者没接管过背光时，挂着书也不该往窗口写一个数"
    );
    assert_eq!(
        reading_brightness(false, Some(0.4)),
        None,
        "书不在上面就是交还系统，哪怕配置里还记着一档"
    );
}

/// 回归门：音量键只在阅读页翻页，别处必须把这一按还给系统。
///
/// 这条分流是整个功能唯一的判据，而两头的失败都安静：阅读页里不翻页＝键按下去没反应，
/// 书架里不转音量＝把系统本来就有的键做成了死键。翻页那一路要进阅读页、音量那一路要进
/// JNI，宿主测试两样都摸不到，所以钉在 [`VolumeKey::of`] 这一层。
/// 「音量上 = 往前一页、音量下 = 往后一页」的符号也钉在这里 —— 改了口径测试会红，
/// 不会悄悄反过来。
#[test]
fn the_volume_keys_page_only_on_the_reading_view() {
    use super::VolumeKey;

    assert_eq!(VolumeKey::of(true, true), VolumeKey::Turn(-1));
    assert_eq!(VolumeKey::of(true, false), VolumeKey::Turn(1));
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

/// 回归门：报给正文的那份扣减里必须含安全区上下两条，而**摆层只让顶上那一条**。
///
/// 正文自己量的是**窗口**高，正文盒 = 窗口高 − 报过去的这一条 − 页眉 − 页脚。少扣一条
/// 安全区，那个盒子就比真能画的地方高几十逻辑像素，多出来的一段被裁在页脚带下面 ——
/// 屏幕上的表现是「已经滚到底了，最后几行还是看不见」。多扣一条导航栏则是反过来的错：
/// 章尾凭空少一行，而那一行本来放得下。
///
/// 底下那一条是**层不让、底栏与页脚自己让**（见 `reader::page` 的 `bottom_inset`）：
/// 手势条那一段要由卡片的底色铺满，不能是层垫出来的一条空缝。
#[test]
fn the_reading_page_is_told_about_the_safe_areas_too() {
    use super::{Reserve, reading_reserve, reading_top_inset};
    use crate::ui::host::Insets;

    let safe = Insets {
        top: 41.0,
        bottom: 25.0,
        ..Default::default()
    };

    // 正文盒上下两条都扣。
    assert_eq!(
        reading_reserve(&safe),
        Reserve {
            width: 0.0,
            height: 41.0 + 25.0
        }
    );

    // 而摆层只把顶上那一条推下去：底下那一条留给底栏与页脚自己抬内容、铺底色。
    assert_eq!(reading_top_inset(&safe), 41.0);
    assert_eq!(
        reading_reserve(&safe).height,
        reading_top_inset(&safe) + safe.bottom
    );

    // 安全区全 0（桌面的真实值）就是零：不多让一个像素。
    assert_eq!(reading_reserve(&Insets::ZERO), Reserve::ZERO);
    assert_eq!(reading_top_inset(&Insets::ZERO), 0.0);
}

/// 回归门：转场里那一页往哪边走、什么时候淡。
///
/// 位移是从 `NavStack` 那个**已经过完缓动**的进度折出来的，所以这一层钉得住的是方向与端点：
/// 返回时出场那页往右走并淡出、架面从左外滑回原位；进书正好反过来。接反了不报错、不 panic，
/// 只在真机上读成「书是从右边来的」—— 而这是外壳里唯一一个方向不该靠猜的数。
#[test]
fn the_exit_pages_right_and_the_entry_comes_from_the_left() {
    use super::{FOLLOW, LEAVE, NavOperation, PresencePhase, nav_offset};

    let width = 400.0;
    // 返回的起步：出场那页还在原位、还满不透明。
    assert_eq!(
        nav_offset(NavOperation::Pop, PresencePhase::Exiting, 0.0, width),
        (0.0, 1.0)
    );
    // 动完：往右走满 `LEAVE`、完全淡出。
    assert_eq!(
        nav_offset(NavOperation::Pop, PresencePhase::Exiting, 1.0, width),
        (width * LEAVE, 0.0)
    );
    // 进场的架面：起步在左外 `FOLLOW`，动完回到 0，全程不淡（它身后只有外壳背景，淡起来是灰）。
    assert_eq!(
        nav_offset(NavOperation::Pop, PresencePhase::Entering, 0.0, width),
        (-width * FOLLOW, 1.0)
    );
    assert_eq!(
        nav_offset(NavOperation::Pop, PresencePhase::Entering, 1.0, width),
        (0.0, 1.0)
    );

    // 进书 = 返回反过来：新那页从右外走满 `LEAVE` 盖上来，底下的架面往左退 `FOLLOW`，两张都不淡。
    assert_eq!(
        nav_offset(NavOperation::Push, PresencePhase::Entering, 0.0, width),
        (width * LEAVE, 1.0)
    );
    assert_eq!(
        nav_offset(NavOperation::Push, PresencePhase::Entering, 1.0, width),
        (0.0, 1.0)
    );
    assert_eq!(
        nav_offset(NavOperation::Push, PresencePhase::Exiting, 1.0, width),
        (-width * FOLLOW, 1.0)
    );
    // 替换按进书摆：换上来那张同样是盖住别人的一张。
    assert_eq!(
        nav_offset(NavOperation::Replace, PresencePhase::Entering, 0.0, width),
        (width * LEAVE, 1.0)
    );

    // 中间每一帧都单调往前挪，不会冲过头再退回来；不透明度始终在 0..=1 里。
    let mut last = 0.0;
    for step in 0..=10 {
        let progress = step as f32 / 10.0;
        let (shift, alpha) = nav_offset(NavOperation::Pop, PresencePhase::Exiting, progress, width);
        assert!(shift >= last, "进度 {progress} 时出场那页往回退了");
        last = shift;
        assert!(
            (0.0..=1.0).contains(&alpha),
            "进度 {progress} 时不透明度 {alpha} 跑出了 0..=1"
        );
    }
}

/// 回归门：阅读层那两个数接对头了没有。
///
/// `Presence` 报的是「还有多在场」（进场 0→1、退场 1→0），`nav_offset` 要的是「这一趟走了
/// 多远」。进场同向、退场相反 —— 两边都取反或都不取反都不会报错，画面分别是
/// 「书一边出现一边往右 drift 走」与「书退到一半又停住不动」。这一层专门钉两头各一次。
#[test]
fn the_layer_reads_presence_and_writes_distance_apart() {
    use super::{LEAVE, PresencePhase, layer_offset};

    let width = 400.0;
    // 进场：还没在场 = 还在右外 `LEAVE`；完全在场 = 回到原位、满不透明。
    assert_eq!(
        layer_offset(PresencePhase::Entering, 0.0, width),
        (width * LEAVE, 1.0)
    );
    assert_eq!(
        layer_offset(PresencePhase::Entering, 1.0, width),
        (0.0, 1.0)
    );

    // 退场：还满在场 = 还在原位；一丝不剩 = 走满 `LEAVE` 并淡光。
    assert_eq!(layer_offset(PresencePhase::Exiting, 1.0, width), (0.0, 1.0));
    assert_eq!(
        layer_offset(PresencePhase::Exiting, 0.0, width),
        (width * LEAVE, 0.0)
    );

    // 定住与不在：一个样式都不加。
    assert_eq!(layer_offset(PresencePhase::Present, 1.0, width), (0.0, 1.0));
    assert_eq!(layer_offset(PresencePhase::Absent, 0.0, width), (0.0, 1.0));

    // 进场那一趟是**从右外回到原位**，所以位移必须单调变小 —— 反过来就是那处不报错的错。
    let mut last = width * LEAVE;
    for step in 0..=10 {
        let present = step as f32 / 10.0;
        let (shift, _) = layer_offset(PresencePhase::Entering, present, width);
        assert!(shift <= last, "进场时读数 {present} 这一层反而更往右了");
        last = shift;
    }
}

#[cfg(not(mobile))]
mod booted {
    use crate::ui::features::shelf::ShelfView;
    use crate::ui::services::config::ConfigHolder;
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
    use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext, point, px, size};

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
                crate::ui::services::app_store::install(Store::in_memory().expect("内存库"), cx);
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
                crate::ui::services::app_store::install(Store::in_memory().expect("内存库"), cx);
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
            .update(|cx| crate::ui::services::app_store::handle(cx))
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
            // 连画两帧：`Popover` 的那一层要先量一次触发器的 bounds 才把卡挂进弹窗层，
            // 所以「开」与「看得见」之间隔了一帧 —— 一帧就断言会把这条正常的延迟读成没画出来。
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

    /// 方向键翻页（PC）——从键绑定一路扎到页号上。
    ///
    /// `reader::view` 那条钉的是 `arrow_turn` 本身；这一条钉的是它**接得上键**：
    /// `KeyBinding` → 动作 → [`AppShell::arrow_key`] → 阅读页。接错了在屏幕上只是
    /// 「按了没反应」，而这条链只有真窗口跑得完（正文本身不拿焦点，这些键在
    /// `None` 上下文上绑着）。
    #[gpui_kit::test]
    fn the_arrow_keys_turn_the_pages(cx: &mut TestAppContext) {
        use folio_core::content::format::{ChapterLocator, ChapterText, DocMeta};
        use folio_core::{Chapter, FlowDoc, Format};

        let (handle, shell) = boot(cx, false);
        let store = cx.update(|cx| crate::ui::services::app_store::handle(cx).unwrap());
        // 正文换成一段够长的合成语料 —— 夹具本身只有两百多字，一页就装完了，「翻得动」
        // 那个断言就无从成立。位置写成 `Inline`（正文在手上）：`Markdown` 那一条位置
        // 本来算的是 `Section`，而打开时回容器按序号取，取得的是夹具自己那两百多字。
        let fixture = std::path::Path::new("../../tests/fixtures/oss/minimal-zh.epub");
        let source = (0..80)
            .map(|i| {
                format!(
                    "<p>于是我们奋力向前，逆水行舟，被不断地向后推，推入过去。（第 {i} 段）</p>"
                )
            })
            .collect::<String>();
        let flow = FlowDoc {
            chapters: vec![Chapter {
                title: "第一章".into(),
                spine_href: String::new(),
                text: ChapterText::At(ChapterLocator::Inline(source)),
                char_len: None,
                images: Vec::new(),
            }],
            ..Default::default()
        };
        let book = store
            .add_book(
                fixture,
                &DocMeta {
                    title: "测试书".into(),
                    format: Format::Markdown,
                    ..DocMeta::default()
                },
            )
            .expect("合成书该入得了库");
        store
            .save_flow(book, &flow, Format::Markdown)
            .expect("章节该存得进去");

        shell.update(cx, |this, cx| this.open_reading(book, cx));
        // 排版在渲染帧里做：一帧只把恢复请求挂上，切页要再走一帧。
        let frames = |cx: &mut TestAppContext| {
            for _ in 0..3 {
                cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                    .expect("窗口应能渲染");
                cx.run_until_parked();
            }
        };
        let read_page = |cx: &mut TestAppContext| {
            shell.read_with(cx, |this, app| {
                this.reading
                    .as_ref()
                    .expect("书该挂着")
                    .page
                    .read(app)
                    .page_number(app)
            })
        };
        // 等这一章切出多页。语料够长，但排版是异步的，一帧未必切完。
        let mut count = 0;
        for _ in 0..20 {
            frames(cx);
            count = shell.read_with(cx, |this, app| {
                this.reading
                    .as_ref()
                    .expect("书该挂着")
                    .page
                    .read(app)
                    .sheet_count(app)
            });
            if count > 1 {
                break;
            }
        }
        assert!(count > 1, "前置：这段语料该切成多页，实得 {count} 页");

        let first = read_page(cx);
        // 往右 / 往下 = 往后一屏。
        cx.simulate_keystrokes(handle, "right");
        frames(cx);
        let forward = read_page(cx);
        assert!(forward > first, "右箭头该往前走：{first} → {forward}");
        // 往左 / 往上 = 往回一屏。
        cx.simulate_keystrokes(handle, "left");
        frames(cx);
        assert_eq!(read_page(cx), first, "左箭头该退回原来那一屏");
        // 上下与左右同向（纵向没有 RTL 这回事）。
        cx.simulate_keystrokes(handle, "down");
        frames(cx);
        assert_eq!(read_page(cx), forward, "下箭头与右箭头同向");
        cx.simulate_keystrokes(handle, "up");
        frames(cx);
        assert_eq!(read_page(cx), first, "上箭头与左箭头同向");
    }

    /// 阅读层必须是一条 hit-test 边界：它盖着书架时，点击不能落到身后的卡片上。
    ///
    /// 这条钉的是「阅读层只在进场/退场时 occlude」的那个洞：定住时背景不透明，
    /// 但背景不参与命中，卡片仍会收到 `ShelfEvent::Open`，`open_reading` 于是
    /// 调 `page.open` 换书重载。测试按书架的**真实封面坐标**点下去，而不是
    /// 直接调 `open_reading`，所以漏掉整层 occlude 时它会真的把当前书换掉。
    #[gpui_kit::test]
    fn reading_layer_blocks_clicks_that_land_on_a_shelf_card(cx: &mut TestAppContext) {
        use folio_core::{Chapter, ChapterText, DocMeta, FlowDoc, Format};

        let (handle, shell) = boot(cx, false);
        // 这条钉定住后的命中；把减动效打开，`Presence` 直接给 Present，不把转场时钟混进来。
        cx.update(|cx| cx.set_reduce_motion(true));
        let store = cx.update(|cx| crate::ui::services::app_store::handle(cx).unwrap());
        let fixture = std::path::Path::new("../../tests/fixtures/oss/minimal-zh.epub");
        // 第二本用同一份夹具的临时副本：`add_book` 按路径认书，同一路径加两次
        // 只会拿到同一个 id。`local/` 下的书不进 CI，不能用。
        let fixtures = tempfile::tempdir().expect("临时目录");
        let other_fixture = fixtures.path().join("other.epub");
        std::fs::copy(fixture, &other_fixture).expect("复制夹具");
        let add = |title: &str, path: &std::path::Path| -> i64 {
            store
                .add_book(
                    path,
                    &DocMeta {
                        title: title.into(),
                        format: Format::Markdown,
                        ..DocMeta::default()
                    },
                )
                .expect("测试书该入得了库")
        };
        let opened = add("正在读", fixture);
        let other = add("身后那本", &other_fixture);
        let flow = FlowDoc {
            chapters: vec![Chapter {
                title: "第一章".into(),
                spine_href: String::new(),
                text: ChapterText::Inline(
                    "<p>于是我们奋力向前，逆水行舟，被不断地向后推，推入过去。</p>".repeat(20),
                ),
                char_len: None,
                images: Vec::new(),
            }],
            ..Default::default()
        };
        store
            .save_flow(opened, &flow, Format::Markdown)
            .expect("正文该存得进去");
        // 身后那本也要有可读正文：只登记索引的话，`open_book` 会在
        // `load_content` 那一步失败、保持当前书不变 —— 那正好会把「点击穿透」
        // 测成通过。两本都能打开，换书才藏不住。
        store
            .save_flow(other, &flow, Format::Markdown)
            .expect("身后那本的正文也该存得进去");
        shell.update(cx, |this, cx| {
            this.shelf.update(cx, |shelf, cx| shelf.force_reload(cx));
        });
        let frames = |cx: &mut TestAppContext| {
            for _ in 0..3 {
                cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                    .expect("窗口应能渲染");
                cx.run_until_parked();
            }
        };
        frames(cx);
        let target = cx
            .update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window
                    .try_find(format!("shelf-cover-{other}"))
                    .map(|snapshot| snapshot.bounds())
            })
            .expect("窗口应能渲染")
            .expect("身后那本书的封面该在架面上画出来");

        shell.update(cx, |this, cx| this.open_reading(opened, cx));
        frames(cx);
        let page = shell.read_with(cx, |this, _| {
            this.reading.as_ref().expect("书该挂着").page.clone()
        });
        assert_eq!(
            page.read_with(cx, |page, app| page.book_id(app)),
            Some(opened),
            "前置：正在读第一本"
        );

        // 点的是身后那张卡片的**真实中心**；阅读层若没有 occlude，这一击会同时
        // 命中卡片，`open_reading` 会把 page 换成另一本。
        cx.update_window(handle, |_, window, cx| {
            window.click_at(
                format!("shelf-cover-{other}"),
                point(
                    px(f32::from(target.size.width) / 2.0),
                    px(f32::from(target.size.height) / 2.0),
                ),
                cx,
            );
        })
        .expect("窗口应能渲染");
        frames(cx);
        assert_eq!(
            page.read_with(cx, |page, app| page.book_id(app)),
            Some(opened),
            "点击落在身后书架卡片上时，阅读层必须替读者挡住这一击"
        );
    }

    /// 整层 occlude 不能把阅读页自己也挡住：点正文中心仍然要开顶栏/底栏。
    ///
    /// `occlude` 挡的是这一层**后面**的 hitbox；正文、菜单、目录都是它的子元素，必须
    /// 照常收事件。这条用「点正文 → 底栏出现」钉住，免得修点击穿透时把阅读页点死。
    #[gpui_kit::test]
    fn reading_layer_still_receives_clicks_when_it_occludes(cx: &mut TestAppContext) {
        let (handle, shell) = boot(cx, false);
        cx.update(|cx| cx.set_reduce_motion(true));
        shell.update(cx, |this, cx| this.open_reading(NOT_A_BOOK, cx));
        for _ in 0..3 {
            cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                .expect("窗口应能渲染");
            cx.run_until_parked();
        }
        assert!(
            cx.update_window(handle, |_, window, _| {
                window.try_find("reader-bottom-bar").is_none()
            })
            .expect("窗口应能渲染"),
            "前置：刚开书时底栏还没出来"
        );

        cx.update_window(handle, |_, window, cx| window.click("folio-reader", cx))
            .expect("窗口应能渲染");
        for _ in 0..2 {
            cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                .expect("窗口应能渲染");
            cx.run_until_parked();
        }
        assert!(
            cx.update_window(handle, |_, window, _| {
                window.try_find("reader-bottom-bar").is_some()
            })
            .expect("窗口应能渲染"),
            "阅读层 occlude 之后，正文中心的一击仍该开底栏"
        );
    }

    /// 回归门：没有焦点时，返回键照常走到外壳的导航 —— 上面那一档只在真有异议时才吃掉这一键。
    ///
    /// 书开着时「导航」就是收起那一层（它是最上面那一屏），而不是去弹架面那条栈。
    #[gpui_kit::test]
    fn back_still_reaches_the_shell_when_nothing_has_focus(cx: &mut TestAppContext) {
        let (handle, shell) = boot(cx, false);
        let stack = shell.read_with(cx, |this, _| {
            this.stacks[Destination::Shelf.index()].clone()
        });
        shell.update(cx, |this, cx| this.open_reading(1, cx));
        assert!(
            shell.read_with(cx, |this, _| this.reading_open()),
            "书该已经盖上来了"
        );

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.focused(cx).is_none(), "点开书之后没有输入框拿着焦点");
        })
        .expect("窗口应能渲染");
        cx.simulate_keystrokes(handle, "escape");

        assert!(
            shell.read_with(cx, |this, _| !this.reading_open()),
            "没有焦点时返回要收那一层，而不是空转一次"
        );
        assert_eq!(
            stack.read_with(cx, |state, _| state.depth()),
            1,
            "收书不该去动架面那条栈"
        );
    }

    /// 回归门：站在某一组那一页里按返回，落点是**书架**，不是把进程退掉。
    ///
    /// 手机那一端 `host::back_exits_app` 是真：那条栈走到栈底就 `cx.quit()`。某一组若是
    /// 架面上的一格筛选状态，栈深自始至终是 1 —— 在组里按返回就是退出应用，读者报的
    /// 「在分组内返回直接返回系统主界面」正是这一条。现在某一组是压在同一条栈上的一页：
    /// 第一下弹回整架，第二下才轮到平台边界（这一端是桌面，不退出，所以原地不动）。
    ///
    /// 起点是「点开那一组的命令已经派到架面」：真实入口是顶部那几张组卡与那颗「去处」菜单，
    /// 而测试窗口没有「点到这一块元素」的定位法（`open_group_for_test` 同 `feed_import_event`）。
    #[gpui_kit::test]
    fn back_inside_a_group_lands_on_the_shelf_not_the_home_screen(cx: &mut TestAppContext) {
        use std::path::Path;

        let (handle, shell) = boot(cx, false);
        let stack = shell.read_with(cx, |this, _| {
            this.stacks[Destination::Shelf.index()].clone()
        });
        let store = cx.update(|cx| crate::ui::services::app_store::handle(cx).unwrap());
        let meta = |title: &str| folio_core::DocMeta {
            title: title.into(),
            authors: vec!["作者丙".into()],
            format: folio_core::Format::Epub,
            size: 1,
            ..Default::default()
        };
        let book = store
            .add_book(Path::new("/books/甲.epub"), &meta("甲"))
            .expect("入库");
        store
            .add_book(Path::new("/books/乙.epub"), &meta("乙"))
            .expect("入库");
        let group = store.create_collection("想读").expect("建组");
        store.add_to_collection(group, book).expect("挂进去");

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let shelf = stack
                .read(cx)
                .current()
                .cloned()
                .and_then(|top| top.downcast::<ShelfView>().ok())
                .expect("书架栈底就是架面本身");
            shelf.update(cx, |shelf, cx| shelf.open_group_for_test(group, window, cx));
        })
        .expect("窗口应能渲染");
        assert_eq!(
            stack.read_with(cx, |state, _| state.depth()),
            2,
            "前置：某一组是架面栈上压着的一页"
        );

        cx.simulate_keystrokes(handle, "escape");
        assert_eq!(
            stack.read_with(cx, |state, _| state.depth()),
            1,
            "在组里按返回要弹回书架，而不是走到那条退出路"
        );
        let top = stack
            .read_with(cx, |state, _| state.current().cloned())
            .expect("栈底总有一页")
            .downcast::<ShelfView>()
            .expect("弹回来的该是整架那一块架面");
        assert_eq!(
            top.read_with(cx, |this, _| this.card_count()),
            2,
            "落回的是整架那一片（两本），不是组里那一本"
        );

        // 栈底那一按照旧归平台边界：这一端不退出，所以栈深与目的地都不动。
        cx.simulate_keystrokes(handle, "escape");
        assert_eq!(stack.read_with(cx, |state, _| state.depth()), 1);
        assert_eq!(
            shell.read_with(cx, |this, _| this.destination),
            Destination::Shelf,
            "返回不许把读者甩到别的目的地"
        );
    }

    /// 回归门：书盖着与退场的整段时间，架面那条栈与那条导航栏**一动不动**。
    ///
    /// 这一条钉的是阅读层化买的东西本身。上一版书是架面栈上的第 2 页，「栈上压了页」与
    /// 「导航栏收起」被绑成同一件事，于是 pop 那一帧导航栏回来、正文容器矮一截，而贴着
    /// 容器底边的那条进度带当场往上跳一段 —— 正文按**窗口**量、带子按容器贴，两套参照系
    /// 在读者眼前分开。现在书是盖在整窗上的一层：架面栈深全程是 1，导航栏压根没被收起，
    /// 也就没有可跳的地方。跟着书的只剩系统级那一层（`immersive`），那是另一件事。
    #[gpui_kit::test]
    fn the_book_leaves_the_shelf_stack_alone(cx: &mut TestAppContext) {
        use crate::ui::shell::{ChromeState, NAV_TRANSITION};
        use std::time::Duration;

        let (handle, shell) = boot(cx, false);
        let stack = shell.read_with(cx, |this, _| {
            this.stacks[Destination::Shelf.index()].clone()
        });
        shell.update(cx, |this, cx| this.open_reading(NOT_A_BOOK, cx));
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("窗口应能渲染");

        assert_eq!(
            stack.read_with(cx, |state, _| state.depth()),
            1,
            "书不在架面那条栈上 —— 它盖在上面"
        );
        assert_eq!(
            shell.read_with(cx, |this, _| this.chrome_painted),
            ChromeState {
                nav_gone: false,
                immersive: true
            },
            "读书时导航栏该照旧画着（被那一层盖住），只有系统级那一层跟着书"
        );

        // 退场走完：系统级那一层放开，栈深还是那个 1 —— 全程没人挤过容器。
        shell.update(cx, |this, cx| this.close_reading(cx));
        cx.executor()
            .advance_clock(NAV_TRANSITION + Duration::from_millis(20));
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("窗口应能渲染");
        assert_eq!(
            shell.read_with(cx, |this, _| this.chrome_painted),
            ChromeState {
                nav_gone: false,
                immersive: false
            },
            "书没了就该把系统栏还给系统，而导航栏本来就没走"
        );
        assert_eq!(stack.read_with(cx, |state, _| state.depth()), 1);
    }

    /// 回归门：退场没走完之前那一层不许放手，走完了就该放手。
    ///
    /// `close_reading` 一调，落库与统计当场就记了账（不等动画，动画被打断、进程被杀都不该
    /// 把这两笔带走），而画面还要往外滑 280 毫秒。两头都得钉：提前收是一刀切断的动画，
    /// 拖到最后不收是把已经离开的那本书一直挂在树上。
    ///
    /// 进场要先走完再收：`Presence` 的退场是**从当前值**起跑的，进场只画了一帧就收，
    /// 那一层的当前值还停在起点 —— 那不是 bug，是不把动画走完就测退场的测法本身。
    #[gpui_kit::test]
    fn the_book_stays_mounted_until_its_exit_has_run(cx: &mut TestAppContext) {
        use crate::ui::shell::NAV_TRANSITION;
        use std::time::Duration;

        let (handle, shell) = boot(cx, false);
        shell.update(cx, |this, cx| this.open_reading(NOT_A_BOOK, cx));
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("窗口应能渲染");
        assert!(
            shell.read_with(cx, |this, _| this.reading_open()),
            "书该已经盖上来了"
        );
        // 把进场走完。
        cx.executor()
            .advance_clock(NAV_TRANSITION + Duration::from_millis(20));
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("窗口应能渲染");
        assert!(
            shell.read_with(cx, |this, _| this.reading_open()),
            "进场走完，这一层该稳稳挂着"
        );

        shell.update(cx, |this, cx| this.close_reading(cx));
        // 音量键那一条已经不再认这本书了：它不在退场动画里替人翻页。
        assert!(
            !shell.read_with(cx, |this, _| this.reading_open()),
            "退场期间不该还能用音量键翻这本书的页"
        );
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("窗口应能渲染");

        // 退场走了一半：还挂着。
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("窗口应能渲染");
        assert!(
            shell.read_with(cx, |this, _| this.reading_mounted()),
            "退场还在走，这一层不该已经放手"
        );

        // 走完：放手。
        cx.executor()
            .advance_clock(NAV_TRANSITION + Duration::from_millis(20));
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("窗口应能渲染");
        assert!(
            shell.read_with(cx, |this, _| this.reading.is_none()),
            "动完了就该把这一层放手"
        );
    }

    /// 回归门：顶栏那颗返回不许把进程带走。
    ///
    /// 那颗按钮的回调是 `page.update(|this| this.pop())`，而 `pop` 早先请外壳再 `update`
    /// 一次**同一个** `ReadingPage`（`close_reading` 里的 `page.close`）—— 同一个实体的
    /// 二次可变借用直接 panic；release 下 `panic = abort`，真机上的表现是读者报的那句
    /// 「点返回直奔系统桌面」。这条门照按钮那条路走一遍：`dismiss_reading` 只翻退出那一格，
    /// 正文自己先收尾。
    #[gpui_kit::test]
    fn the_top_bars_back_closes_the_book_without_taking_the_process_with_it(
        cx: &mut TestAppContext,
    ) {
        use crate::ui::shell::NAV_TRANSITION;
        use std::time::Duration;

        let (handle, shell) = boot(cx, false);
        let page = shell.update(cx, |this, cx| {
            this.open_reading(NOT_A_BOOK, cx);
            this.reading
                .as_ref()
                .map(|layer| layer.page.clone())
                .expect("书该已经盖上来")
        });
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("窗口应能渲染");
        // 把进场走完：退场从半路的当前值起跑不是这条门要钉的事。
        cx.executor()
            .advance_clock(NAV_TRANSITION + Duration::from_millis(20));
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("窗口应能渲染");

        // 照顶栏那颗按钮的回调来：正更新着正文的时候调 `pop`（旧写法在这里 panic）。
        page.update(cx, |page, cx| page.pop(cx));

        assert!(
            !shell.read_with(cx, |this, _| this.reading_open()),
            "返回该把这一层收走"
        );
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("窗口应能渲染");
        assert!(
            shell.read_with(cx, |this, _| this.reading_mounted()),
            "退场动画还在走，这一层还不能放手"
        );
    }

    /// 回归门：删掉**正开着**的那一本，那一层要退回架面；删掉别的那本，它一动不动。
    ///
    /// 架面删完书会广播 `ShelfEvent::Deleted`，而这条广播只有外壳接得住（那一层在它手上）。
    /// 没人接的表现不是崩，是安静：那一层继续显示一行已经不在的书，离席那两笔账各走一条
    /// `tracing::warn!`，那几十分钟就此不记账。后半句（删别的那本不该动）一样要紧 ——
    /// 判据是「开着的是不是正是被删的那一本」，不看 id 就成了一删就退。
    ///
    /// 起点是架面**发那条事件**而不是它自己那颗删除按钮：删那一步要动硬盘上的副本文件，
    /// 而这条门钉的是外壳那一侧的分流。
    ///
    /// 开着的那一本必须**真入过库**（索引也存进去），不是只登记一行：`open_book` 从库里取
    /// 索引出来的章节，只有行的那本会走到「书库里读不出来」那条 error、停在原屏，那样的
    /// `book_id()` 是 `None` —— 分流的两半就都在拿空值比空值。这三句的形状抄自
    /// `ReaderSession` 那条内置样本路径（`open(path)` → `add_book` → `save_document`）。
    /// 前置条件本身也钉了一条（那一层记下的 id 必须就是这一本）。
    #[gpui_kit::test]
    fn deleting_the_opened_book_sends_the_layer_back(cx: &mut TestAppContext) {
        use crate::ui::features::shelf::ShelfEvent;
        use crate::ui::shell::NAV_TRANSITION;
        use std::time::Duration;

        let (handle, shell) = boot(cx, false);
        let store = cx.update(|cx| crate::ui::services::app_store::handle(cx).unwrap());
        // 宿主测试的工作目录是 crate 目录，所以仓库根的夹具在 `../../`。
        let fixture = std::path::Path::new("../../tests/fixtures/oss/minimal-zh.epub");
        let doc = folio_core::content::document::open(fixture).expect("仓库里那份合成书该打得开");
        let opened = store
            .add_book(fixture, doc.meta())
            .expect("合成书该入得了库");
        store
            .save_document(opened, doc.as_ref())
            .expect("那份索引该存得进去");
        // 另一本不需要真文件：这一条只要 id 不同。
        let other = store
            .add_book(
                std::path::Path::new("/books/not-the-open-one.epub"),
                doc.meta(),
            )
            .unwrap();

        shell.update(cx, |this, cx| this.open_reading(opened, cx));
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("窗口应能渲染");
        assert_eq!(
            shell.read_with(cx, |this, app| {
                this.reading
                    .as_ref()
                    .and_then(|layer| layer.page.read(app).book_id(app))
            }),
            Some(opened),
            "那一层得真的开着这一本并记下它的 id"
        );

        let shelf = cx
            .update(|cx| {
                let stack = shell.read(cx).stacks[Destination::Shelf.index()].clone();
                stack
                    .read(cx)
                    .current()
                    .cloned()
                    .and_then(|top| top.downcast::<ShelfView>().ok())
            })
            .expect("书架栈底就是架面本身");

        cx.update(|cx| shelf.update(cx, |_, cx| cx.emit(ShelfEvent::Deleted(other))));
        assert!(
            shell.read_with(cx, |this, _| this.reading_open()),
            "删的不是开着的那一本，那一层不该退场"
        );

        cx.update(|cx| shelf.update(cx, |_, cx| cx.emit(ShelfEvent::Deleted(opened))));
        assert!(
            !shell.read_with(cx, |this, _| this.reading_open()),
            "开着的那本被删了：那一层该开始退场，而不是继续盖着一行没了的书"
        );
        cx.executor()
            .advance_clock(NAV_TRANSITION + Duration::from_millis(20));
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("窗口应能渲染");
        assert!(
            shell.read_with(cx, |this, _| this.reading.is_none()),
            "退场走完就该把这一层放手"
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
                crate::ui::services::app_store::install(store, cx);
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
    /// 走的是外壳已有的 `open_reading`（把那一层盖上来），所以这条同时钉住两件事：那一层
    /// 挂上了，而**目的地没被切走** —— 书盖的是读者当前看着的那一屏，退完场他就回到榜单上。
    /// 上一版把书压进架面那条栈，从统计点进去时栈与目的地各说一套，是一个没人报过的错位。
    #[gpui_kit::test]
    fn a_stats_row_opens_the_book_over_the_report(cx: &mut TestAppContext) {
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

        assert!(
            shell.read_with(cx, |this, _| this.reading_open()),
            "那一层要盖上来了：返回时靠它把位置与时长一起结掉"
        );
        assert_eq!(
            stack.read_with(cx, |state, _| state.depth()),
            1,
            "从统计点开一本书不该去动架面那条栈"
        );
    }

    /// 回归门：关掉一本书之后统计要跟上，不必等读者再切一次目的地。
    ///
    /// 读者最自然的一次追问就是「我刚读的那几分钟记上了吗」。结算是收层那一刻做的
    /// （`ReadingPage::close`，不等退场动画），所以同一处必须顺手叫一次重取，否则那一屏
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
        // 而**盖上来的那一层**故意用一个读不出来的 id：`open_book` 认不出库里的行
        // 就只写日志、不动别的，收层时那次重取照样发生 —— 这条门钉的是「收层会叫重取」，
        // 不是「书真能打开」。时间戳取现在附近：`reading_summary(14)` 只看最近两周。
        let store = cx.update(|cx| crate::ui::services::app_store::handle(cx).unwrap());
        let book = store
            .add_book(
                std::path::Path::new("/books/a.epub"),
                &folio_core::DocMeta {
                    title: "A".into(),
                    authors: vec![],
                    format: folio_core::Format::Epub,
                    size: 1,
                    ..folio_core::content::format::DocMeta::default()
                },
            )
            .unwrap();
        let now = folio_core::infra::time::now();
        shell.update(cx, |this, cx| this.open_reading(NOT_A_BOOK, cx));
        store.record_session(book, now - 600, now, 0).unwrap();
        shell.update(cx, |this, cx| this.close_reading(cx));

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
            .update(|cx| crate::ui::services::app_store::handle(cx))
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
            .update(|cx| crate::ui::services::app_store::handle(cx))
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
