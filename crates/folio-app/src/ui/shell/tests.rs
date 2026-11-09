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

    // 而报给正文的那一条**永远不含导航栏**：宽恒为 0。底栏那 56 与侧栏那 96 都还在
    // 原地画着，只是被这一层盖住 —— 报错了轴的那一步正是上一版让两端各扣错一条边的地方。
    let safe = Insets {
        top: 41.0,
        bottom: 25.0,
        ..Default::default()
    };
    assert_eq!(
        reading_reserve(&safe),
        Reserve { width: 0.0 },
        "宽恒为 0（导航栏由这一层盖住，不再向正文报），高走 PageInsets"
    );
}

/// 三档的占位宽与底栏那两条数：**只有嵌入的两档占宽**，底栏浮在内容上。
///
/// 这是「三档统一」里最容易被改歪的一格：底栏一旦顺手也占一条布局行，内容就再也
/// 到不了它底下（悬浮感当场没有）；侧栏两档一旦不占宽，书架第一列就永远压在卡片下面
/// （看着像丢了一列书）。两个方向都只在这一处判。
#[test]
fn the_nav_bar_reserves_width_only_where_it_is_docked() {
    use super::SizeClass;
    use super::navigation::{
        BAR_HEIGHT, NAV_MARGIN, NAV_WIDTH, bar_offset, clearance, reserved_width,
    };

    assert_eq!(
        reserved_width(SizeClass::Compact),
        0.0,
        "底栏是浮的：内容铺满整窗，只靠尾部留白让开"
    );
    assert_eq!(
        reserved_width(SizeClass::Medium),
        NAV_WIDTH + NAV_MARGIN * 2.0,
        "悬浮侧栏：卡片 96，左右各 12 的列归它"
    );
    assert_eq!(
        reserved_width(SizeClass::Expanded),
        NAV_WIDTH,
        "嵌入侧栏：贴边，只吃 96"
    );

    // 手势条那一条算两次：底栏自己让一次（`bar_offset`），内容尾部留白再让一次
    // （`clearance`），否则滚到底那一排会压在胶囊下面。
    assert_eq!(bar_offset(25.0), NAV_MARGIN + 25.0);
    assert_eq!(clearance(25.0), BAR_HEIGHT + NAV_MARGIN * 2.0 + 25.0);
    assert_eq!(
        clearance(0.0) - bar_offset(0.0),
        BAR_HEIGHT + NAV_MARGIN,
        "留白比栏位偏移多出一条栏高再多一圈：多出来的那圈是栏顶到内容的呼吸"
    );
}

/// 回归门：Compact 档的底栏是**浮**的 —— 它不吃布局行，且离手势条正好一圈。
///
/// 宿主进不到这一档（[`SizeClass::of`] 在桌面把窄窗口夹到 `Medium`），所以这里直接问
/// `navigation`：560×640 的窗口、25 的手势条，胶囊应当宽 536、高 56、下沿落在 603。
/// 内容那一格仍然铺满整窗 —— 它一旦被挤短一行，内容就再也到不了栏底下（悬浮感当场没有），
/// 这正是把「底栏」写成布局行的代价。
#[gpui_kit::test]
fn the_floating_bar_sits_above_the_gesture_strip_without_taking_a_row(
    cx: &mut gpui_kit::TestAppContext,
) {
    use crate::ui::host::Insets;
    use crate::ui::shell::navigation::{Destination, SizeClass, navigation};
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{
        AppContext as _, Context, InteractiveElement as _, IntoElement, ParentElement, Render,
        Styled, TestSupportExt as _, Window, div, px, size,
    };

    struct Page;
    impl Render for Page {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let safe = Insets {
                bottom: 25.0,
                ..Default::default()
            };
            div()
                .relative()
                .size_full()
                .child(div().id("page-body").size_full().test_support())
                .child(navigation(
                    cx,
                    Destination::Shelf,
                    SizeClass::Compact,
                    &safe,
                    false,
                    _window,
                    |_, _, _| {},
                ))
        }
    }

    // 导航栏要读文案（`tr` 走配置里的语言）与主题色，两样都得先装上。
    cx.update(gpui_kit::init);
    cx.update(|cx| {
        crate::ui::services::config::ConfigHolder::install(folio_core::Config::default(), cx)
    });

    let handle = cx
        .open_window(size(px(560.), px(640.)), |window, cx| {
            let page = cx.new(|_| Page);
            Root::new(page, window, cx)
        })
        .into();
    let (page, bar) = cx
        .update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            (
                window.try_find("page-body").map(|snap| snap.bounds()),
                window.try_find("folio-bar").map(|snap| snap.bounds()),
            )
        })
        .expect("窗口应能渲染");

    let page = page.expect("内容那一格应在视图树里");
    let bar = bar.expect("Compact 档该画出浮栏");
    assert_eq!(
        f32::from(page.size.height),
        640.0,
        "内容铺满整窗：底栏不吃布局行"
    );
    assert_eq!(
        f32::from(bar.size.width),
        560.0 - crate::ui::shell::navigation::NAV_MARGIN * 2.0,
        "左右各留一圈"
    );
    assert_eq!(
        f32::from(bar.size.height),
        crate::ui::shell::navigation::BAR_HEIGHT
    );
    assert_eq!(
        f32::from(bar.origin.y + bar.size.height),
        640.0 - 25.0 - 12.0,
        "下沿离手势条那一条还有一圈"
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

/// 阅读页的浅深只在**书挂着**时覆盖软件外观；关书之后一个字节都不许带走。
///
/// 失败的样子与背光那条同族：读着夜读的书退回架面，整屏还是黑的，而软件外观明明选的
/// 是浅色 —— 读者只会以为设置没生效。这条也钉在自由函数上：外壳那一帧在测试里摸不到
/// 墨水屏（或减动效）把三个时钟收成零：判据是纯函数芯，全局旗在 `repaint` 那头推。
/// 电子纸上每一帧重绘都看得见，转场与翻页在那端不是动效是闪烁。
#[test]
fn the_motion_clocks_collapse_when_calm() {
    use super::{Duration, NAV_TRANSITION, nav_duration_for, turn_duration_for, turn_over_for};

    // 整屏进出 / 阅读层进出：calm 与 reduce 两者居其一即零，平常档是 280。
    assert_eq!(nav_duration_for(false, true), Duration::ZERO, "calm");
    assert_eq!(nav_duration_for(true, false), Duration::ZERO, "reduce");
    assert_eq!(
        nav_duration_for(false, false),
        NAV_TRANSITION,
        "平常档还是整屏进出那一个数"
    );
    // 翻页时长：calm 归零，平常档照配置走。
    assert_eq!(turn_duration_for(280.0, true), Duration::ZERO);
    assert_eq!(turn_duration_for(280.0, false), Duration::from_millis(280));
    // 翻页曲线：calm 之下纸当场落定，给多长都不滑。
    assert_eq!(
        turn_over_for(Duration::from_millis(400), false, true),
        Duration::ZERO
    );
    assert_eq!(
        turn_over_for(Duration::from_millis(400), false, false),
        Duration::from_millis(400)
    );
}

/// `Window::appearance()` 的系统档位。
#[test]
fn the_reader_mode_only_wins_while_the_layer_covers() {
    use super::themed_appearance;
    use folio_core::infra::config::{Appearance, Mode};

    let mut appearance = Appearance {
        mode: Mode::Light,
        ..Appearance::default()
    };
    assert_eq!(
        themed_appearance(appearance.clone(), false).mode,
        Mode::Light,
        "层没盖满时软件外观说了算，哪怕配置里记着一档阅读浅深"
    );

    appearance.reading_mode = Some(Mode::Dark);
    assert_eq!(
        themed_appearance(appearance.clone(), true).mode,
        Mode::Dark,
        "层盖满时阅读页自己那一档覆盖软件外观"
    );
    assert_eq!(
        themed_appearance(appearance, false).mode,
        Mode::Light,
        "层没盖满（进场中 / 退场中 / 没开书）回到软件外观"
    );

    let inherited = Appearance {
        mode: Mode::Light,
        reading_mode: None,
        ..Appearance::default()
    };
    assert_eq!(
        themed_appearance(inherited, true).mode,
        Mode::Light,
        "阅读页没选过时跟软件外观走"
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

    // 两条系统边都由阅读页报给会话的 `PageInsets` 管，这里不再报高度。
    assert_eq!(reading_reserve(&safe), Reserve::ZERO);

    // 摆层只把顶上那一条推下去：底下那一条留给底栏与页脚自己抬内容、铺底色。
    assert_eq!(reading_top_inset(&safe), 41.0);

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

/// 回归门：读夜里的书，架面在**转场全程**都是白天的；退场起步那一帧就翻回来。
///
/// 失败的样子读者报过（Android 真机）：书是黑夜模式，退出阅读时架面先跟着深一整段
/// 退场动画、层放完才翻回白天 —— 那一翻就是一记闪。成因是折档判据跟着「层挂着」走：
/// 层还在往外滑，壳就还涂着夜里，架面以深色露出来，等 `Presence` 走完才换肤。
///
/// 现在的不变量是**全局主题只在层盖满的那段时间里是阅读档**（[`themed_appearance`]）：
/// 进场没盖满不折（架面白天、深页从右盖上来），盖满那一帧才翻夜里（被挡住，看不见）；
/// 退场起步那一帧就翻回白天（层还全盖着，看不见），架面全程是白天。正文自己的颜色
/// 不跟壳走（`foundation::reading_theme` 取侧自持），差量也仍等退场走完才撤 —— 页面
/// 在动画里保持夜里。这一条钉的是这几个时刻。
#[cfg(not(mobile))]
#[gpui_kit::test]
fn leaving_a_night_book_repaints_the_shelf_in_the_app_mode(cx: &mut gpui_kit::TestAppContext) {
    use crate::ui::services::config::ConfigHolder;
    use crate::ui::shell::AppShell;
    use folio_core::content::document::Document;
    use folio_core::ChapterExt;
    use folio_core::{
        Appearance, BookSettings, Chapter, Config, Content, DocMeta, FlowDoc, Format, Locator,
        Mode, Reader,
    };
    use gpui_kit::AnyWindowHandle;
    use gpui_kit::AppContext as _;
    use gpui_kit::base::Theme as BaseTheme;
    use gpui_kit::base::ThemeAppearance;
    use gpui_kit::component::{Root, Theme, ThemeMode};
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{px, size};
    use std::sync::Arc;

    cx.update(gpui_kit::init);
    let store = folio_core::Store::in_memory().expect("内存库");
    store
        .add_book(
            std::path::Path::new("/books/night.epub"),
            &DocMeta {
                title: "夜读".into(),
                format: Format::Epub,
                size: 1,
                ..DocMeta::default()
            },
        )
        .expect("入库");
    let source = (0..20)
        .map(|i| format!("<p>夜读的第 {i} 段。于是我们奋力向前，逆水行舟，被不断地向后推。</p>"))
        .collect::<String>();
    let content = Content::Reflow(FlowDoc {
        chapters: vec![Chapter::inline("第一章", source)],
        ..Default::default()
    });
    let doc: Arc<dyn Document> = Arc::from(folio_core::content::document::from_content(
        DocMeta::default(),
        content,
    ));

    let mut shell = None;
    let handle: AnyWindowHandle = cx
        .open_window(size(px(900.), px(600.)), |window, cx| {
            crate::ui::services::app_store::install(store, cx);
            ConfigHolder::install(
                Config {
                    library: folio_core::Library {
                        dir: "/tmp/folio-books".into(),
                        watch: None,
                    },
                    appearance: Appearance {
                        mode: Mode::Light,
                        ..Default::default()
                    },
                    ..Config::default()
                },
                cx,
            );
            let view = cx.new(|cx| AppShell::boot(window, cx));
            shell = Some(view.clone());
            Root::new(view, window, cx)
        })
        .into();
    let shell = shell.expect("外壳应已建起");
    cx.run_until_parked();

    // 按生产那条路开书：会话建起来、把这一本挂进配置实体，再让外壳把那一层盖上。
    cx.update_window(handle, |_, _, cx| {
        shell.update(cx, |shell, cx| {
            shell.open_reading_for_test(doc, Reader::default(), Locator::start(), cx);
        });
    })
    .expect("开书这一步该跑得起来");
    // 生产那条路是会话建起来时就挂上差量；这里递进来的文档不带书库主键，补上那一挂。
    cx.update(|cx| ConfigHolder::set_book(cx, Some(BookSettings::default())));
    // 阅读浅深钉成夜里：与读者在弹层或设置页里点「夜里」写进去的是同一格
    // （2026-11-07 起这一格只归全局，写默认层）。
    cx.update(|cx| ConfigHolder::edit_appearance(cx, |a| a.reading_mode = Some(Mode::Dark)));
    cx.run_until_parked();
    // 进场还没走完：层没盖满，壳**不折** —— 架面（此刻在层身后）仍是白天的软件外观。
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("开书那一屏该画得出来");
    assert_eq!(
        cx.update(|cx| Theme::global(cx).mode),
        ThemeMode::Light,
        "进场没盖满，壳还是白天 —— 翻面要等盖满那一帧"
    );
    // 走完进场：盖满那一帧才折到夜里，此刻层正挡着整窗，读者看不见这一翻。
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(400));
    for _ in 0..4 {
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("进场走完那一屏该画得出来");
        cx.run_until_parked();
    }
    let (mounted, reading_mode, theme_mode) = cx.update(|cx| {
        (
            shell.read(cx).reading_mounted_for_test(),
            ConfigHolder::appearance(cx).reading_mode,
            Theme::global(cx).mode,
        )
    });
    assert!(mounted, "书该挂着");
    assert_eq!(reading_mode, Some(Mode::Dark), "这一本该是夜里");
    assert_eq!(theme_mode, ThemeMode::Dark, "盖满之后该是夜里");

    // 读者按返回：退场开始。**起步那一帧**壳就翻回白天 —— 层还全盖着，看不见这一翻；
    // 架面从这一帧起就是白天，那一记「层放完才翻」的闪没有了。
    cx.update(|cx| shell.update(cx, |shell, cx| shell.close_reading(cx)));
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("退场起步那一屏该画得出来");
    assert_eq!(
        cx.update(|cx| Theme::global(cx).mode),
        ThemeMode::Light,
        "退场起步壳就回到白天 —— 层还盖着，读者看不见这一翻"
    );
    cx.run_until_parked();
    let park = std::time::Duration::from_millis(500);
    for _ in 0..400 {
        cx.executor().advance_clock(park);
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("退场那几帧该画得出来");
        cx.run_until_parked();
        if !cx.update(|cx| shell.read(cx).reading_mounted_for_test()) {
            break;
        }
    }
    assert!(
        !cx.update(|cx| shell.read(cx).reading_mounted_for_test()),
        "退场该在几秒之内走完"
    );

    // 放开的那一帧里，差量已经撤掉、解析出来的已经是白天 —— 架面就是这一帧画的。
    assert!(
        cx.update(|cx| ConfigHolder::book(cx)).is_none(),
        "放开的那一帧就要撤掉这一本的差量"
    );
    assert_eq!(
        cx.update(|cx| Theme::global(cx).mode),
        ThemeMode::Light,
        "退出之后回到软件外观（白天）"
    );
    assert_eq!(
        cx.update(|cx| BaseTheme::global(cx).appearance),
        ThemeAppearance::Light,
        "底层滚动条那一份主题也要跟着换"
    );
}

#[cfg(not(mobile))]
mod booted {
    use folio_core::ChapterExt;
    use crate::ui::components::BottomSheetState;
    use crate::ui::features::shelf::ShelfView;
    use crate::ui::services::config::ConfigHolder;
    use crate::ui::shell::AppShell;
    use crate::ui::shell::navigation::Destination;
    use folio_core::infra::i18n::{Locale, t_in};
    use folio_core::infra::task::{ByteProgress, ProgressEvent};
    use folio_core::{
        Candidate, Config, ImportLanding, ImportLogEntry, Language, Library, PathsSource,
        ScanReport, Store, TaskEvent, TaskManager, submit_import,
    };
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{
        AnyWindowHandle, AppContext as _, Entity, ScrollDelta, TestAppContext, point, px, size,
    };

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
        boot_sized(cx, size(px(900.), px(600.)), desktop_host)
    }

    /// 同一套壳，但视口由调用方给：导航栏让位只在 Compact / Medium 两档发生（`SizeClass::of`
    /// 把 840 宽以上判成 Expanded，那一档 `note_scroll` 直接返回），要验让位就得把窗口开窄。
    fn boot_sized(
        cx: &mut TestAppContext,
        window_size: gpui_kit::Size<gpui_kit::Pixels>,
        desktop_host: bool,
    ) -> (AnyWindowHandle, Entity<AppShell>) {
        cx.update(gpui_kit::init);
        let mut shell = None;
        let handle = cx
            .open_window(window_size, |window, cx| {
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

    /// 起一版「扫描结果已经到手、架上有 40 本」的架面：扫描那几条回归门共用的前置。
    ///
    /// 视口由调用方给 —— 导航栏让位只在 840 宽以下发生（`SizeClass::of`）。
    fn boot_scanned_shelf(
        cx: &mut TestAppContext,
        window_size: gpui_kit::Size<gpui_kit::Pixels>,
    ) -> (
        AnyWindowHandle,
        Entity<AppShell>,
        Entity<ShelfView>,
        tempfile::TempDir,
    ) {
        let (handle, shell) = boot_sized(cx, window_size, false);
        let stack = shell.read_with(cx, |this, _| {
            this.stacks[Destination::Shelf.index()].clone()
        });
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("下载");
        std::fs::create_dir_all(&dir).unwrap();
        let report = ScanReport {
            candidates: (0..30)
                .map(|ix| {
                    let name = format!("{ix}.txt");
                    std::fs::write(dir.join(&name), "正文\n").unwrap();
                    Candidate {
                        origin: folio_core::Origin::Local(dir.join(&name)),
                        name: format!("{ix:02}.txt"),
                        size: Some(8),
                    }
                })
                .collect(),
            skipped: 0,
            truncated: false,
        };
        let shelf = stack
            .read_with(cx, |stack, _| {
                stack
                    .current()
                    .cloned()
                    .and_then(|top| top.downcast::<ShelfView>().ok())
            })
            .expect("书架栈底就是架面本身");
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            shelf.update(cx, |shelf, cx| {
                shelf.feed_scan_event(TaskEvent::Finished(Ok(report)), cx)
            });
            // 架面自己也要滚得动，否则「架面不动」那一半量的是空气。
            let store = crate::ui::services::app_store::handle(cx).expect("书库应已装上");
            for ix in 0..40 {
                let path = dir.join(format!("架上{ix:02}.txt"));
                std::fs::write(&path, "正文\n").unwrap();
                store
                    .add_book(
                        &path,
                        &folio_core::content::format::DocMeta {
                            title: format!("架上{ix:02}"),
                            format: folio_core::Format::Text,
                            size: 6,
                            ..Default::default()
                        },
                    )
                    .unwrap();
            }
            shelf.update(cx, |shelf, cx| shelf.force_reload(cx));
            for _ in 0..4 {
                window.render_frame(cx);
            }
        })
        .expect("窗口应能渲染");
        (handle, shell, shelf, tmp)
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

    /// 给只验层/点击/退场的用例挂一层真实可读的阅读页。
    ///
    /// 外壳现在不允许拿假 book id 构造阅读页（`open_reading` 先检查 `has_book`）；
    /// 这些用例不验内容，给一份最小可重排文档就够了。
    fn open_test_reading(shell: &Entity<AppShell>, cx: &mut TestAppContext) {
        let doc: std::sync::Arc<dyn folio_core::content::document::Document> =
            std::sync::Arc::from(folio_core::content::document::from_content(
                folio_core::content::format::DocMeta::default(),
                folio_core::Content::Reflow(folio_core::FlowDoc {
                    chapters: vec![folio_core::Chapter::inline("第一章", "<p>测试正文。</p>")],
                    ..Default::default()
                }),
            ));
        shell.update(cx, |shell, cx| {
            shell.open_reading_for_test(
                doc,
                folio_core::Reader::default(),
                folio_core::Locator::start(),
                cx,
            );
        });
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

    /// 回归门：书库里没有这一本时，阅读层不许被构造。
    ///
    /// `open_reading` 先 `open_book` 再问 `has_book()`；假 id 在空书库里读不出来，
    /// `Source::Empty` 只能停在会话里，不能变成一屏。这里同时画一帧，
    /// 确认空态外壳仍能出帧（不是靠 panic 挡住）。
    #[gpui_kit::test]
    fn a_missing_book_does_not_mount_the_reading_layer(cx: &mut TestAppContext) {
        let (handle, shell) = boot(cx, false);
        shell.update(cx, |this, cx| this.open_reading(i64::MAX, cx));
        assert!(
            !cx.update(|cx| shell.read(cx).reading_mounted_for_test()),
            "读不出来的书不该构造阅读页"
        );
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("窗口应能渲染");
        assert!(
            !cx.update(|cx| shell.read(cx).reading_mounted_for_test()),
            "空态渲染之后仍不该有阅读页"
        );
    }

    /// 回归门：自定义标题栏必须挂在视图树里（曾经的 bug 是窗口选项声明了透明标题栏、
    /// 却没有任何人绘制它，于是顶上什么都没有）。判据是 `TitleBar` 自己的 `title-bar`
    /// 那格，不是里面摆的孩子 —— 左侧的品牌字样已经撤掉，栏本身仍要在。
    #[gpui_kit::test]
    fn the_app_renders_a_custom_title_bar(cx: &mut TestAppContext) {
        let handle = open_app(cx);
        let present = cx
            .update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.try_find("title-bar").is_some()
            })
            .expect("窗口应能渲染");
        assert!(present, "自绘标题栏应出现在视图树中");
    }

    /// 回归门：≥840 时导航栏是**嵌入**的那一列，宽就是 96（三档统一之后不再是 260）。
    ///
    /// 测试窗口开在 900×600，落在 Expanded 档。那 260 宽的老侧栏一旦回来，这条当场红；
    /// 而 96 是华为「侧边页签」控件那一格，也是书架 `reserved_width` 里同一个数 ——
    /// 两处各写一份的话，改一侧忘另一侧只会表现为「格子宽得莫名其妙」。
    #[gpui_kit::test]
    fn the_nav_bar_docks_its_ninety_six_column_when_wide(cx: &mut TestAppContext) {
        let handle = open_app(cx);
        let nav = cx
            .update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window
                    .try_find("folio-nav")
                    .map(|snap| f32::from(snap.bounds().size.width))
            })
            .expect("窗口应能渲染");

        assert_eq!(
            nav,
            Some(crate::ui::shell::navigation::NAV_WIDTH),
            "嵌入侧栏宽就是 96，不该再是 260"
        );
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
                window.try_find("shelf-import-records").is_none(),
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
            // 连画两帧：`Popover` 的那一层要先量一次触发器的 bounds 才把卡挂进弹窗层，
            // 「开」与「看得见」之间隔了一帧 —— 与上面那条扫描门同一口径。
            window.render_frame(cx);
            window.render_frame(cx);
            assert!(
                window.try_find("shelf-import-records").is_some(),
                "进度那张卡要画出来：这几秒里架面不动，读者只有它可看"
            );

            shelf.update(cx, |shelf, cx| {
                shelf.feed_import_event(TaskEvent::Finished(Ok(outcomes)), cx);
            });
            // 连画两帧：清单是一条虚拟列表，要先量一帧才排得出行。
            window.render_frame(cx);
            window.render_frame(cx);
            assert_eq!(
                shelf.read(cx).card_count(),
                1,
                "好的那本要立刻成为架上一格，坏的那本不该把它挡在架外"
            );
            // 一批跑完这张卡**不收**：它换成交代那一种内容 —— 一行一本书，结清的先后倒过来排，
            // 所以第 0 行是那本后端不认的 `.mobi`，第 1 行才是落地的《第一本》。
            assert!(
                window.try_find("shelf-import-records").is_some(),
                "结清之后这张卡还在：它现在说的是哪几本怎么样了"
            );
            // 卡头只有一行标题：成功几本 / 没进来几本不再在这儿重数一遍（逐行自己带状态）。
            // 门量的是**高度** —— 两行文字（标题 + 汇总）会把这一条撑到 60 以上，
            // 一行标题连上下留白不到 52。把汇总加回去这条当场红。
            let head_h = window
                .try_find("shelf-record-head")
                .map(|snap| f32::from(snap.bounds().size.height));
            assert!(
                head_h.is_some_and(|h| h > 0.0 && h < 52.0),
                "卡头只该有一行标题，汇总那一行不该再摆：{head_h:?}"
            );
            assert!(
                window.try_find("shelf-record-title").is_some(),
                "标题该独占了卡头那一行"
            );
            // 结清那一档只有**要读者处理**的那几类出标签（这里第 0 行是后端不认的
            // `.mobi`）；成功 / 已在架上不占标签位，它们落进名字下面那行小字的头一个词。
            assert!(
                window.try_find("shelf-record-status-0").is_some(),
                "没进来的那一行要有一枚语义色标签：读者得一眼看见它没进来"
            );
            assert!(
                window.try_find("shelf-record-status-1").is_none(),
                "落地那一行不该再挂中性标签：书名才是这一屏的唯一焦点"
            );
            assert!(
                window.try_find("shelf-record-open-1").is_some(),
                "落定的那一本要有一颗「打开」"
            );
            assert!(
                window.try_find("shelf-record-open-0").is_none(),
                "没进来的那一本没有对象可作用，那颗钮就不该摆上去"
            );

            // 屏幕上这一列是内存里的，进程一没就没了 —— 这一批同时进了库才叫「记录」。
            // 「重开一次库还在」由 core 那族门钉（`store::import_log`），这条钉的是**接线**：
            // 结清的那一批确实写进去了，逐本的结局也带过去了。
            let log = crate::ui::services::app_store::handle(cx)
                .expect("书库应已装上")
                .import_log(10)
                .unwrap();
            assert_eq!(log.len(), 2, "这一批两本都该进库");
            assert!(
                log.iter()
                    .any(|entry| entry.landing == folio_core::ImportLanding::Added),
                "落地的那本要带着「成功」回来"
            );
            assert!(
                log.iter().any(|entry| entry.landing
                    == folio_core::ImportLanding::Rejected(
                        folio_core::ImportReject::UnsupportedFormat
                    )),
                "没进来的那本要带着它那一档原因回来"
            );
        })
        .expect("窗口应能渲染");
    }

    /// 回归门：字节读数怎么长成「书名下面那条进度条」。
    ///
    /// 记录行不是界面上预摆的（`shelf::view::status` 模块头写了为什么不摆），是
    /// [`ByteProgress`] 喂出来的：名字对不上就新开一行，那一行下面那条子读的就是这两个数。
    /// 这条钉住三件事 —— 行确实从字节里长出来、报过的每一本各占一行、第二本不许落在
    /// 第一本那一条上（那才是「哪一本在传」这个问题的答案）。
    #[gpui_kit::test]
    fn the_byte_readout_grows_a_row_with_its_own_bar(cx: &mut TestAppContext) {
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
            assert!(
                window.try_find("shelf-import-records").is_none(),
                "没在导入时不该有这张卡"
            );

            shelf.update(cx, |this, cx| {
                this.feed_import_event(
                    TaskEvent::Progress(ProgressEvent {
                        done: 0,
                        total: 3,
                        message: String::new(),
                    }),
                    cx,
                );
                this.feed_import_event(
                    TaskEvent::Bytes(ByteProgress {
                        done: 1 << 20,
                        total: Some(4 << 20),
                        name: "基督山伯爵.epub".into(),
                    }),
                    cx,
                );
            });
            window.render_frame(cx);
            window.render_frame(cx);
            assert!(
                window.try_find("shelf-record-bar-0").is_some(),
                "报过字节的那一本要有一行，名字下面是按字节走的那条子"
            );

            // 第二本（总数不知道那一档：服务器没给 `Content-Length`）。
            shelf.update(cx, |this, cx| {
                this.feed_import_event(
                    TaskEvent::Bytes(ByteProgress {
                        done: 7,
                        total: None,
                        name: "没有长度的一本书.bin".into(),
                    }),
                    cx,
                );
            });
            window.render_frame(cx);
            assert!(
                window.try_find("shelf-record-bar-1").is_some(),
                "第二本另起一行，不许把读数记到第一本那一条上"
            );
        })
        .expect("窗口应能渲染");
    }

    /// 回归门：这张卡**由读者关掉**之后剩下的那两条语义。
    ///
    /// 从前那两行读数摆在架面上，读者躲不开；换成浮层就多了「关掉」这一手，于是关掉的
    /// 范围必须说清楚：关掉的是「别一直报进度」，不是「这笔账不用说」。所以同一批的
    /// 下一条读数不再把卡叫回来，而那一批跑完真有几本没进来时，卡要再出来交代一次。
    #[gpui_kit::test]
    fn the_dismissed_import_card_goes_quiet_until_the_batch_reports(cx: &mut TestAppContext) {
        let (handle, shell) = boot(cx, false);
        let stack = shell.read_with(cx, |this, _| {
            this.stacks[Destination::Shelf.index()].clone()
        });

        let tmp = tempfile::tempdir().unwrap();
        let picked = tmp.path().join("原处");
        std::fs::create_dir_all(&picked).unwrap();
        std::fs::write(picked.join("a.txt"), "第一本\n").unwrap();
        std::fs::write(picked.join("b.mobi"), "后端不认这个扩展名\n").unwrap();
        let manager = TaskManager::new(1);
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

        let progress = |done: u64, total: u64| {
            TaskEvent::Progress(ProgressEvent {
                done,
                total,
                message: "".into(),
            })
        };

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let shelf = stack
                .read(cx)
                .current()
                .cloned()
                .and_then(|top| top.downcast::<ShelfView>().ok())
                .expect("书架栈底就是架面本身");

            shelf.update(cx, |this, cx| this.feed_import_event(progress(1, 2), cx));
            window.render_frame(cx);
            window.render_frame(cx);
            assert!(
                window.try_find("shelf-import-records").is_some(),
                "先看一眼：这会儿那张卡在屏幕上"
            );

            shelf.update(cx, |this, cx| this.dismiss_import_status_for_test(cx));
            window.render_frame(cx);
            window.render_frame(cx);
            assert!(
                window.try_find("shelf-import-records").is_none(),
                "读者关掉这张卡，它就该从屏幕上没了"
            );

            shelf.update(cx, |this, cx| this.feed_import_event(progress(2, 2), cx));
            window.render_frame(cx);
            window.render_frame(cx);
            assert!(
                window.try_find("shelf-import-records").is_none(),
                "同一批的下一条读数不该把刚关掉的那张卡叫回来"
            );

            shelf.update(cx, |this, cx| {
                this.feed_import_event(TaskEvent::Finished(Ok(outcomes)), cx)
            });
            window.render_frame(cx);
            window.render_frame(cx);
            assert!(
                window.try_find("shelf-import-records").is_some(),
                "一批跑完有了逐本的着落：那一幕是新信息，卡要再出来交代一次"
            );

            // 读者把这张卡关掉之后，三点菜单里那颗「导入记录」要还能把它翻回来：
            // `dismiss` 挡的是**自动**冒出来那几次，不挡主动要看的那一次。
            shelf.update(cx, |this, cx| this.dismiss_import_status_for_test(cx));
            window.render_frame(cx);
            window.render_frame(cx);
            assert!(
                window.try_find("shelf-import-records").is_none(),
                "关掉之后先确认它真的不在了 —— 下一条断言才有意义"
            );
            shelf.update(cx, |this, cx| this.open_import_records_for_test(cx));
            window.render_frame(cx);
            window.render_frame(cx);
            assert!(
                window.try_find("shelf-import-records").is_some(),
                "从菜单翻开导入记录：主动要看的那一次不该被上一次的关掉挡住"
            );
        })
        .expect("窗口应能渲染");
    }

    /// 回归门：**重启之后不许自己弹导入记录。**
    ///
    /// 库里那份历史是启动时读回来的（`load_import_log`），它只说明「以前导过」，不是
    /// 「现在有话要说」。从前判据把「历史非空」也算进自动抬卡那一档，于是每次开软件
    /// 都先弹一张昨天的记录。这一条钉：历史在库里 → 卡不自己出来；从菜单点名 →
    /// 还是要能翻出来（历史没丢，只是不吵）。
    #[gpui_kit::test]
    fn the_import_log_from_the_last_run_does_not_pop_the_card(cx: &mut TestAppContext) {
        let (handle, shell) = boot(cx, false);
        let stack = shell.read_with(cx, |this, _| {
            this.stacks[Destination::Shelf.index()].clone()
        });
        // 上一次运行留下的那一笔：现在就在库里，架面起来时会被读回来。
        let store = cx
            .update(|cx| crate::ui::services::app_store::handle(cx))
            .expect("书库应已装上");
        store
            .record_imports(&[ImportLogEntry {
                made_at: 0,
                name: "昨天的.epub".into(),
                format: Some("EPUB".into()),
                size: Some(1024),
                landing: ImportLanding::Added,
                book_id: None,
                secs: 1.0,
            }])
            .expect("写一条记录");

        cx.update_window(handle, |_, window, cx| {
            // 多画几帧：读库发生在第一帧渲染里，卡若会自己冒出来也得给它一帧。
            for _ in 0..4 {
                window.render_frame(cx);
            }
            assert!(
                window.try_find("shelf-import-records").is_none(),
                "历史非空不是「现在有话要说」：重启之后不该自动弹导入记录"
            );

            let shelf = stack
                .read(cx)
                .current()
                .cloned()
                .and_then(|top| top.downcast::<ShelfView>().ok())
                .expect("书架栈底就是架面本身");
            shelf.update(cx, |this, cx| this.open_import_records_for_test(cx));
            for _ in 0..2 {
                window.render_frame(cx);
            }
            assert!(
                window.try_find("shelf-import-records").is_some(),
                "从菜单点名要看的那一次，历史还得翻得出来"
            );
        })
        .expect("窗口应能渲染");
    }

    /// 回归门：扫描清单是「找到 → 勾选 → 导入所选」的那条路，勾中的要走
    /// 与选文件导入同一条管线。
    ///
    /// 扫描结果由 [`ShelfView::feed_scan_event`] 喂进来（真实的整机扫描要扫
    /// 测试机的家目录，那条不是门该扫的），**勾选之后的导入是真跑**：
    /// `import_checked` 把勾中的路径交给后端管线，屏幕正中要出现「正在导入」那张状态卡。
    /// 面板要在扫描结果到手时出现、在导入开始时收掉 —— 清单在时盖着架面，
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
            // 同上一条门：浮层要先量一帧触发器才挂得上。
            window.render_frame(cx);
            window.render_frame(cx);
            assert!(
                window.try_find("shelf-import-records").is_some(),
                "导入所选要接上管线：那张状态卡要出现"
            );
            assert!(
                window.try_find("shelf-scan-panel").is_none(),
                "清单在导入开始时要收掉"
            );
        })
        .expect("窗口应能渲染");
    }

    /// 回归门：扫描事件流收尾不能把刚到手的结果清单一起收走。
    ///
    /// 真实的收尾在 `begin_scan` 那条 spawn 的末尾（通道关了就跑一次「还在扫就收摊」）。
    /// 从前的写法是 `scan.take()`，而 `Finished(Ok)` 刚把清单换进同一个槽 —— 于是清单
    /// 前脚到手、后脚被这一下抽走，弹层留在屏幕上一片空白。这里按真实顺序喂一遍：
    /// 结果事件 → 流收尾 → 清单还要在。
    #[gpui_kit::test]
    fn closing_the_scan_stream_keeps_the_results(cx: &mut TestAppContext) {
        let (handle, shell) = boot(cx, false);
        let stack = shell.read_with(cx, |this, _| {
            this.stacks[Destination::Shelf.index()].clone()
        });
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("下载");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "第一本\n").unwrap();
        let report = ScanReport {
            candidates: vec![Candidate {
                origin: folio_core::Origin::Local(dir.join("a.txt")),
                name: "a.txt".into(),
                size: Some(8),
            }],
            skipped: 0,
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
            shelf.update(cx, |shelf, cx| {
                shelf.feed_scan_event(TaskEvent::Finished(Ok(report)), cx)
            });
            // `Popover` 那一层要先量一帧触发器的 bounds 才把卡挂进弹窗层。
            window.render_frame(cx);
            window.render_frame(cx);
            assert!(
                window.try_find("shelf-scan-panel").is_some(),
                "结果到手面板要出现"
            );
            assert!(
                window.try_find("shelf-scan-pick-0").is_some(),
                "清单第一行该画出来"
            );

            shelf.update(cx, |shelf, cx| shelf.close_scan_stream_for_test(cx));
            window.render_frame(cx);
            assert!(
                window.try_find("shelf-scan-panel").is_some(),
                "收尾那一下不许把扫完的面板抽成空壳"
            );
            assert!(
                window.try_find("shelf-scan-pick-0").is_some(),
                "结果行要还在"
            );
        })
        .expect("窗口应能渲染");
    }

    /// 回归门：点卡面那一下**净**勾上一本，不是勾上又取消。
    ///
    /// 勾选框必须是卡面的**兄弟**而不是孩子：gpui 把一次点击发给每一个 hitbox 含按下点的
    /// handler（不沿祖先链冒泡），嵌在里面的话同一下会触发两次 `set_row_checked` —— 屏幕上
    /// 就是「这张卡按了没反应」，而源码只差一层嵌套，是最难查的一种坏法。
    #[gpui_kit::test]
    fn tapping_the_card_face_checks_it_once(cx: &mut TestAppContext) {
        let (handle, shell) = boot(cx, false);
        let stack = shell.read_with(cx, |this, _| {
            this.stacks[Destination::Shelf.index()].clone()
        });
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("下载");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "第一本\n").unwrap();
        let report = ScanReport {
            candidates: vec![Candidate {
                origin: folio_core::Origin::Local(dir.join("a.txt")),
                name: "a.txt".into(),
                size: Some(8),
            }],
            skipped: 0,
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
            shelf.update(cx, |shelf, cx| {
                shelf.feed_scan_event(TaskEvent::Finished(Ok(report)), cx)
            });
            // 多画几帧：浮层那一张卡要先量一次触发器，虚拟列表要先有一次实测的视口才排得出行。
            for _ in 0..4 {
                window.render_frame(cx);
            }
            assert!(
                window.try_find("shelf-scan-panel").is_some(),
                "结果到手面板要出现"
            );
            assert!(
                window.try_find("shelf-scan-pick-0").is_some(),
                "候选要画成一张卡（封面 + 三行 + 勾选框）"
            );

            window.click("shelf-scan-pick-0", cx);
            assert_eq!(
                shelf.read(cx).checked_rows_for_test(),
                1,
                "点卡面该净勾上一本；这里是 0 就是同一下被勾上又取消掉了"
            );
            window.click("shelf-scan-pick-0", cx);
            assert_eq!(shelf.read(cx).checked_rows_for_test(), 0, "再点一下该取消");

            // 真正吃得住的那一下是点在**勾选框上**：它若是卡面的孩子，这同一次点击会同时
            // 命中内外两层 handler（gpui 的点击发给每个含按下点的 hitbox），净结果回到 0 ——
            // 屏幕上就是「按了框没反应」。兄弟关系只有这一头量得出来。
            window.click("shelf-scan-check-0", cx);
            assert_eq!(
                shelf.read(cx).checked_rows_for_test(),
                1,
                "点勾选框该净勾上一本；这里是 0 就说明框嵌在卡面里、被双重触发了"
            );
            window.click("shelf-scan-check-0", cx);
            assert_eq!(shelf.read(cx).checked_rows_for_test(), 0, "再点一下该取消");
        })
        .expect("窗口应能渲染");
    }

    /// 回归门：清单的搜索与筛选。
    ///
    /// 判据（格式怎么认、大小怎么切）在 `folio_core::ScanFilter` 里，core 那边有单元测试；
    /// 这一条钉的是**接线**：搜索只筛文件名、格式格只筛真扩展名、筛空了说的是「没有符合
    /// 条件的书」而不是「设备里没找到书」—— 两句话说的不是一回事。也钉了「筛掉的只是
    /// 摆不摆出来」：结果还在，清掉条件就回来。
    #[gpui_kit::test]
    fn the_scan_list_is_filtered_by_name_and_format(cx: &mut TestAppContext) {
        let (handle, shell) = boot(cx, false);
        let stack = shell.read_with(cx, |this, _| {
            this.stacks[Destination::Shelf.index()].clone()
        });
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("下载");
        std::fs::create_dir_all(&dir).unwrap();
        let books = [
            ("三体.epub", 5u64 * 1024 * 1024),
            ("球状闪电.pdf", 50 * 1024 * 1024),
            ("笔记.txt", 1024),
        ];
        let report = ScanReport {
            candidates: books
                .iter()
                .map(|(name, size)| {
                    std::fs::write(dir.join(name), "正文\n").unwrap();
                    Candidate {
                        origin: folio_core::Origin::Local(dir.join(name)),
                        name: name.to_string(),
                        size: Some(*size),
                    }
                })
                .collect(),
            skipped: 0,
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
            shelf.update(cx, |shelf, cx| {
                shelf.feed_scan_event(TaskEvent::Finished(Ok(report)), cx)
            });
            for _ in 0..4 {
                window.render_frame(cx);
            }
            assert_eq!(
                shelf.read(cx).visible_scan_names_for_test().len(),
                3,
                "前置：三本都摆出来"
            );
            assert!(
                window.try_find("shelf-scan-filter-panel").is_none(),
                "前置：筛选浮层默认收着"
            );

            // 漏斗打开那张浮层；它开着的时候才在树上。
            shelf.update(cx, |shelf, cx| {
                shelf.set_scan_filter_open_for_test(true, cx)
            });
            for _ in 0..3 {
                window.render_frame(cx);
            }
            assert!(
                window.try_find("shelf-scan-filter-panel").is_some(),
                "点开漏斗该挂出筛选浮层"
            );
            shelf.update(cx, |shelf, cx| {
                shelf.set_scan_filter_open_for_test(false, cx)
            });
            for _ in 0..3 {
                window.render_frame(cx);
            }
            assert!(
                window.try_find("shelf-scan-filter-panel").is_none(),
                "收起来之后浮层不该还留在树上"
            );

            // 搜索只筛文件名（不区分大小写之外不做别的）：一个词留下一本。
            shelf.update(cx, |shelf, cx| shelf.set_scan_query_for_test("闪电", cx));
            for _ in 0..2 {
                window.render_frame(cx);
            }
            assert_eq!(
                shelf.read(cx).visible_scan_names_for_test(),
                vec!["球状闪电.pdf".to_string()]
            );

            // 清词，改按格式筛。
            shelf.update(cx, |shelf, cx| shelf.set_scan_query_for_test("", cx));
            shelf.update(cx, |shelf, cx| {
                shelf.toggle_scan_format_for_test("epub", cx)
            });
            for _ in 0..2 {
                window.render_frame(cx);
            }
            assert_eq!(
                shelf.read(cx).visible_scan_names_for_test(),
                vec!["三体.epub".to_string()]
            );
            assert!(
                window.try_find("shelf-scan-pick-0").is_some(),
                "筛剩的那一本还要画成一张卡"
            );

            // 筛到空：一句灰字，且清单里一张卡都不剩。
            shelf.update(cx, |shelf, cx| {
                shelf.toggle_scan_format_for_test("epub", cx)
            });
            shelf.update(cx, |shelf, cx| {
                shelf.toggle_scan_format_for_test("mobi", cx)
            });
            for _ in 0..2 {
                window.render_frame(cx);
            }
            assert!(
                shelf.read(cx).visible_scan_names_for_test().is_empty(),
                "这台机器上没有 mobi，筛出来该是空的"
            );
            assert!(
                window.try_find("shelf-scan-empty").is_some(),
                "筛空了要说「没有符合条件的书」，不能留一屏空白"
            );
            assert!(
                window.try_find("shelf-scan-pick-0").is_none(),
                "筛掉的行不该还画在清单里"
            );

            // 大小上限：只留小于 1 MB 的那一本。
            shelf.update(cx, |shelf, cx| shelf.clear_scan_filter_for_test(window, cx));
            shelf.update(cx, |shelf, cx| {
                shelf.set_scan_size_for_test(Some(1024 * 1024), cx)
            });
            for _ in 0..2 {
                window.render_frame(cx);
            }
            assert_eq!(
                shelf.read(cx).visible_scan_names_for_test(),
                vec!["笔记.txt".to_string()],
                "小于 1 MB 的只剩那本 1 KB 的"
            );

            // 结果没有丢：条件清掉就回来。
            shelf.update(cx, |shelf, cx| shelf.clear_scan_filter_for_test(window, cx));
            for _ in 0..2 {
                window.render_frame(cx);
            }
            assert_eq!(
                shelf.read(cx).visible_scan_names_for_test().len(),
                3,
                "清掉条件该回到全部三本"
            );
            assert!(
                window.try_find("shelf-scan-empty").is_none(),
                "有行可摆时那句灰字该收起来"
            );
        })
        .expect("窗口应能渲染");
    }

    /// 回归门：清单滑得动 —— 滚动偏移必须跨帧留住。
    ///
    /// `v_virtual_list` 每帧新建一份内部滚动句柄，不拿 `.track_scroll` 顶掉它，偏移就每帧
    /// 归零：手指滑了、下一帧弹回原处，屏幕上正是「上下滑不动」。三十本只画得下开头那几行，
    /// 所以判据是「滚一把第 20 本要进视口、第 0 本要出去」—— 每帧抹回 0 的话两头都不成立。
    #[gpui_kit::test]
    fn the_scan_list_keeps_its_scroll_offset(cx: &mut TestAppContext) {
        let (handle, shell) = boot(cx, false);
        let stack = shell.read_with(cx, |this, _| {
            this.stacks[Destination::Shelf.index()].clone()
        });
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("下载");
        std::fs::create_dir_all(&dir).unwrap();
        let report = ScanReport {
            candidates: (0..30)
                .map(|ix| {
                    let name = format!("{ix}.txt");
                    std::fs::write(dir.join(&name), "正文\n").unwrap();
                    Candidate {
                        origin: folio_core::Origin::Local(dir.join(&name)),
                        name: format!("{ix:02}.txt"),
                        size: Some(8),
                    }
                })
                .collect(),
            skipped: 0,
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
            shelf.update(cx, |shelf, cx| {
                shelf.feed_scan_event(TaskEvent::Finished(Ok(report)), cx)
            });

            // 架面先要**自己滚得动**，否则下面那条「卡里滚、架面不动」的断言是空的：
            // 往库里塞够一屏的副本，再逼架面重查一次。
            let store = crate::ui::services::app_store::handle(cx).expect("书库应已装上");
            for ix in 0..40 {
                let path = dir.join(format!("架上{ix:02}.txt"));
                std::fs::write(&path, "正文\n").unwrap();
                store
                    .add_book(
                        &path,
                        &folio_core::content::format::DocMeta {
                            title: format!("架上{ix:02}"),
                            format: folio_core::Format::Text,
                            size: 6,
                            ..Default::default()
                        },
                    )
                    .unwrap();
            }
            shelf.update(cx, |shelf, cx| shelf.force_reload(cx));

            // 浮层那张卡要先量一次触发器，虚拟列表要先有一次实测的视口才排得出行。
            for _ in 0..4 {
                window.render_frame(cx);
            }

            // 架面这一头先要**真的滚得动**，否则下面那条「架面不动」连对象都没有。
            assert_eq!(shelf.read(cx).card_count(), 40, "架上那 40 本要成为格子");

            assert!(
                window.try_find("shelf-scan-pick-0").is_some(),
                "前置：第一本在视口里"
            );
            assert!(
                window.try_find("shelf-scan-pick-10").is_none(),
                "前置：第 11 本还在视口外"
            );

            // 向下滚 400（gpui 的约定是向下为负）：行高 64、分组头 28，第 11 本的头落在
            // 668 —— 400 这一把刚好把它抬进这一屏，而第 1 本（28..92）已经滚出上沿。
            // 落点取第 0 行那一格的中心 —— 它压在列表那条 `overflow_scroll` 里面。
            let grid_before = shelf.read(cx).grid_offset_for_test();
            window.scroll(
                "shelf-scan-pick-0",
                ScrollDelta::Pixels(point(px(0.0), px(-400.0))),
                cx,
            );
            for _ in 0..4 {
                window.render_frame(cx);
            }

            // 卡里滚的这一把不该带动身后的架面。**这条在无头窗口里抓不住那个 bug**：
            // `window.scroll` 按元素中心派发的一次滚轮本来就没落到架面（摘掉 `.occlude()` /
            // `scroll_guard` 实测都照过）。真机那条路（触摸折出来的滚动）与「顶栏别跟着
            // 让位」分别由 `the_scan_card_takes_the_touch_scroll_and_the_shelf_does_not`
            // 与 `scrolling_the_scan_card_does_not_fold_the_nav_bar` 钉住；这一条留着是为了
            // 别让「清单自己滚不动」在将来某次改动里悄悄成立。
            assert_eq!(
                shelf.read(cx).grid_offset_for_test(),
                grid_before,
                "清单滚了一把，架面跟着动了 —— 卡盖着的那片格子不该收到这次滚轮"
            );

            assert!(
                window.try_find("shelf-scan-pick-10").is_some(),
                "滚一把之后第 11 本该进视口；不在这里就是偏移每帧被抹回 0"
            );
            assert!(
                window.try_find("shelf-scan-pick-0").is_none(),
                "第一本该滚出视口"
            );
        })
        .expect("窗口应能渲染");
    }

    /// 回归门：真机那条路（raw `TouchEvent` → 手势识别器）在扫描卡上滑。
    ///
    /// 与上一条是两回事，别删其一：`window.scroll` 是按元素中心**直发滚轮**，而真机上是
    /// 一根手指被识别器折成 `ScrollEvent`（落点是起手点、带 `touch_phase`、之后还有惯性），
    /// 走的是另一条派发。这条门钉住两头：卡里那一列**滚得动**（否则「架面不动」是空的），
    /// 架面的偏移**一动不动**；最后在卡外再滑一笔，架面要动 —— 证明触摸这条路本身是通的，
    /// 前面那条「不动」不是因为事件根本没送到。
    #[gpui_kit::test]
    fn the_scan_card_takes_the_touch_scroll_and_the_shelf_does_not(cx: &mut TestAppContext) {
        use gpui_kit::{InputEvent as _, Pixels, Point, TouchEvent, TouchId, TouchPhase};

        /// 真机那条入口：raw `TouchEvent` 交给 gpui 的手势识别器。
        fn touch(
            cx: &mut TestAppContext,
            handle: AnyWindowHandle,
            phase: TouchPhase,
            at: Point<Pixels>,
        ) {
            cx.update_window(handle, |_, window, cx| {
                window.dispatch_event(
                    TouchEvent {
                        id: TouchId(1),
                        phase,
                        position: at,
                        predicted_position: None,
                        force: None,
                    }
                    .to_platform_input(),
                    cx,
                );
            })
            .expect("事件应送达窗口");
            cx.run_until_parked();
        }

        fn render(cx: &mut TestAppContext, handle: AnyWindowHandle, frames: usize) {
            cx.update_window(handle, |_, window, cx| {
                for _ in 0..frames {
                    window.render_frame(cx);
                }
            })
            .expect("窗口应能渲染");
        }

        let (handle, shell) = boot(cx, false);
        let stack = shell.read_with(cx, |this, _| {
            this.stacks[Destination::Shelf.index()].clone()
        });
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("下载");
        std::fs::create_dir_all(&dir).unwrap();
        let report = ScanReport {
            candidates: (0..30)
                .map(|ix| {
                    let name = format!("{ix}.txt");
                    std::fs::write(dir.join(&name), "正文\n").unwrap();
                    Candidate {
                        origin: folio_core::Origin::Local(dir.join(&name)),
                        name: format!("{ix:02}.txt"),
                        size: Some(8),
                    }
                })
                .collect(),
            skipped: 0,
            truncated: false,
        };
        let shelf = stack
            .read_with(cx, |stack, _| {
                stack
                    .current()
                    .cloned()
                    .and_then(|top| top.downcast::<ShelfView>().ok())
            })
            .expect("书架栈底就是架面本身");

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            shelf.update(cx, |shelf, cx| {
                shelf.feed_scan_event(TaskEvent::Finished(Ok(report)), cx)
            });
            // 架面自己也要滚得动，否则「架面不动」那一半量的是空气。
            let store = crate::ui::services::app_store::handle(cx).expect("书库应已装上");
            for ix in 0..40 {
                let path = dir.join(format!("架上{ix:02}.txt"));
                std::fs::write(&path, "正文\n").unwrap();
                store
                    .add_book(
                        &path,
                        &folio_core::content::format::DocMeta {
                            title: format!("架上{ix:02}"),
                            format: folio_core::Format::Text,
                            size: 6,
                            ..Default::default()
                        },
                    )
                    .unwrap();
            }
            shelf.update(cx, |shelf, cx| shelf.force_reload(cx));
            for _ in 0..4 {
                window.render_frame(cx);
            }
        })
        .expect("窗口应能渲染");

        // 起手点取第 0 行那张卡的中心：它压在清单里面。
        let start = cx
            .update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window
                    .try_find("shelf-scan-pick-0")
                    .expect("第一本要在视口里")
                    .bounds()
                    .center()
            })
            .expect("窗口应能渲染");

        let grid_before = cx.update(|cx| shelf.read(cx).grid_offset_for_test());
        let scan_before = cx.update(|cx| shelf.read(cx).scan_offset_for_test());

        touch(cx, handle, TouchPhase::Started, start);
        touch(
            cx,
            handle,
            TouchPhase::Moved,
            start - point(px(0.0), px(120.0)),
        );
        touch(
            cx,
            handle,
            TouchPhase::Moved,
            start - point(px(0.0), px(240.0)),
        );
        touch(
            cx,
            handle,
            TouchPhase::Ended,
            start - point(px(0.0), px(240.0)),
        );
        render(cx, handle, 20);

        assert!(
            cx.update(|cx| shelf.read(cx).scan_offset_for_test()) > scan_before,
            "卡里那一列该跟着手指滚，实测 {} → {}",
            scan_before,
            cx.update(|cx| shelf.read(cx).scan_offset_for_test())
        );
        assert_eq!(
            cx.update(|cx| shelf.read(cx).grid_offset_for_test()),
            grid_before,
            "卡上的这一笔漏到了身后的架面 —— 它该到卡的清单为止"
        );
        // 反向：落在卡外（架面右侧）的同一笔，架面要动。
        let outside = point(px(800.0), px(300.0));
        touch(cx, handle, TouchPhase::Started, outside);
        touch(
            cx,
            handle,
            TouchPhase::Moved,
            outside - point(px(0.0), px(120.0)),
        );
        touch(
            cx,
            handle,
            TouchPhase::Ended,
            outside - point(px(0.0), px(120.0)),
        );
        render(cx, handle, 20);
        assert!(
            cx.update(|cx| shelf.read(cx).grid_offset_for_test()) > grid_before,
            "卡外这一笔该滚架面；没动就说明触摸这条路没送到，上面那条断言是空的"
        );
    }

    /// 回归门：滑浮层里的清单，导航栏**不该**让位。
    ///
    /// 让位那条监听挂在窗口的捕获相（`AppShell::nav_watch`），比面板自己的
    /// `stop_propagation` 早跑 —— `occlude` 与 `scroll_guard` 都拦不住它。它会把这笔
    /// 当成「读者在翻架面」，顶栏收起来、格子的上沿跟着抬，屏幕上就是「滑卡片，后面的
    /// 书架在动」。`scroll_guard` 每帧报上来的那几块（`ScrollShields`）就是给它判据用的。
    #[gpui_kit::test]
    fn scrolling_the_scan_card_does_not_fold_the_nav_bar(cx: &mut TestAppContext) {
        // 视口开窄：840 宽以上是 Expanded 档，那一档根本不让位（`note_scroll` 直接返回），
        // 1200 那种窗口上这条门是空的。
        let (handle, shell, _shelf, _tmp) = boot_scanned_shelf(cx, size(px(700.), px(600.)));

        let start = cx
            .update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window
                    .try_find("shelf-scan-pick-0")
                    .expect("第一本要在视口里")
                    .bounds()
                    .center()
            })
            .expect("窗口应能渲染");

        /// 在 `at` 处滚 `times` 下（向下 120，gpui 的约定是向下为负）。
        fn wheel(
            cx: &mut TestAppContext,
            handle: AnyWindowHandle,
            at: gpui_kit::Point<gpui_kit::Pixels>,
            times: usize,
        ) {
            for _ in 0..times {
                cx.update_window(handle, |_, window, cx| {
                    window.dispatch_event(
                        gpui_kit::PlatformInput::Scroll(gpui_kit::ScrollEvent {
                            position: at,
                            delta: gpui_kit::ScrollDelta::Pixels(point(px(0.0), px(-120.0))),
                            ..Default::default()
                        }),
                        cx,
                    );
                    window.render_frame(cx);
                })
                .expect("窗口应能渲染");
            }
        }

        wheel(cx, handle, start, 3);
        assert!(
            !cx.update(|cx| shell.read(cx).nav.folded()),
            "卡上的这一笔不是在翻架面：顶栏不该收起来（收起来就是「滑卡片，后面的书架在动」）"
        );

        // 卡外那一笔才是翻架面。它在的时候不让位，就说明上面那条只是「什么都没发生」。
        wheel(cx, handle, point(px(650.), px(300.)), 3);
        assert!(
            cx.update(|cx| shell.read(cx).nav.folded()),
            "卡外这一笔该让顶栏收起来；没让位说明这条门根本没接上"
        );
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

    /// 导航栏让位这件事现在**四屏共用一句判据**：目的地里面那块 `overflow_y_scroll` 的滚动
    /// 要冒到外壳的监听上（`on_scroll` 挂在 bubble 相）。这条钉的是接线本身 —— 挑「统计」
    /// 而不是架面：架面从前自己判过，统计是这次才接上的一屏，冒泡接不通就红在这里，
    /// 而屏幕上的表现只是「那一屏的栏不会收」。
    ///
    /// 窗口开在 700 宽：桌面上那是悬浮侧栏那一档（参与让位）。默认那扇 900 宽是常驻侧栏的
    /// Expanded 档，本来就不参与，在那儿测只会得到「什么都没发生」的假绿。
    ///
    /// 事件按**位置**发（不是按元素 id）：id 那条路只认登记了 `.test_support()` 的元素，
    /// 而这一屏的滚动容器是裸 `div` —— 按位置发才是真机上发生的事（指针在那里，滚动到那里）。
    #[gpui_kit::test]
    fn a_scroll_inside_a_destination_folds_the_navigation(cx: &mut TestAppContext) {
        use gpui_kit::{
            InputEvent as _, MouseMoveEvent, ScrollDelta, ScrollEvent, point, px, size,
        };

        cx.update(gpui_kit::init);
        let mut shell = None;
        let handle = cx
            .open_window(size(px(700.), px(900.)), |window, cx| {
                crate::ui::services::app_store::install(Store::in_memory().expect("内存库"), cx);
                ConfigHolder::install(confirmed_config(), cx);
                let view = cx.new(|cx| AppShell::boot(window, cx));
                shell = Some(view.clone());
                Root::new(view, window, cx)
            })
            .into();
        let shell = shell.expect("外壳应已建起");
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            shell.update(cx, |this, cx| this.go(Destination::Stats, window, cx));
            for _ in 0..3 {
                window.render_frame(cx);
            }
            let at = point(px(350.), px(450.));
            // 指针先落在那一屏上：`should_handle_scroll` 读的是命中与悬停态。
            window.dispatch_event(
                MouseMoveEvent {
                    position: at,
                    ..Default::default()
                }
                .to_platform_input(),
                cx,
            );
            // 命中表是**上一帧**算出来的：挪完指针不画一帧，`should_handle_scroll` 还按
            // 老位置判，事件会在监听器门口被拒（gpui 的 `move_pointer` 后面紧跟着就是一帧）。
            window.render_frame(cx);
            let scroll = |window: &mut gpui_kit::Window, cx: &mut gpui_kit::App, dy: f32| {
                window.dispatch_event(
                    ScrollEvent {
                        position: at,
                        delta: ScrollDelta::Pixels(point(px(0.0), px(dy))),
                        ..Default::default()
                    }
                    .to_platform_input(),
                    cx,
                );
                window.render_frame(cx);
            };
            assert!(!shell.read(cx).nav.folded(), "开局不该是让位状态");

            // gpui 的约定是向下为负：先回拨一点，不该让位。
            scroll(window, cx, 60.0);
            assert!(!shell.read(cx).nav.folded(), "往回拨不该让位");

            // 往下读过线（18 逻辑像素）：栏让开。
            scroll(window, cx, -40.0);
            assert!(
                shell.read(cx).nav.folded(),
                "统计那屏往下滚该让导航栏让位 —— 接不通就是冒泡没到外壳，屏幕上只是那一屏不会收"
            );

            // 往回拨够那 8 像素就放回来。
            scroll(window, cx, 20.0);
            assert!(!shell.read(cx).nav.folded(), "回拨该把栏放回来");
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
        open_test_reading(&shell, cx);
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

    /// 自动阅读是**两段式**：第一击（浮钮）只进入自动、直接开始，浮层整层收掉，设置栏
    /// 不上脸；运行中再点一次正文，才把自动那条工具栏叫出来。退出之后普通底栏回来。
    /// 整条走真实点击，钉的是「先开始、后配置」—— 从前那一版第一击就把速度滑条和
    /// 退出摆到读者脸上，书还没读到一个字。
    ///
    /// 顺带钉住浮钮的命中边界：那一排浮在卡片上缘之外，自己声明挡住身后的正文
    /// hitbox（见 `regions.rs::listen_fabs`）。点「自动」只落在浮钮上，正文不翻页。
    #[gpui_kit::test]
    fn auto_starts_without_settings_and_a_second_tap_opens_them(cx: &mut TestAppContext) {
        let (handle, shell) = boot(cx, false);
        cx.update(|cx| cx.set_reduce_motion(true));
        open_test_reading(&shell, cx);
        let frames = |cx: &mut TestAppContext| {
            for _ in 0..3 {
                cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                    .expect("窗口应能渲染");
                cx.run_until_parked();
            }
        };
        let has = |cx: &mut TestAppContext, id: &'static str| {
            cx.update_window(handle, |_, window, _| window.try_find(id).is_some())
                .expect("窗口应能渲染")
        };
        frames(cx);
        // 开浮层：点正文中心，普通底栏先露着，自动那条还不在。
        cx.update_window(handle, |_, window, cx| window.click("folio-reader", cx))
            .expect("窗口应能渲染");
        frames(cx);
        assert!(has(cx, "reader-bottom-bar"), "前置：普通底栏先露着");
        assert!(
            !has(cx, "reader-auto-bar"),
            "前置：自动没跑，那条工具栏不该在"
        );

        // 第一击点「自动」浮钮：自动开始，浮层整层收干净 —— 设置不上脸。
        // 浮钮挡住身后的正文 hitbox，这一击只落在浮钮上，正文不翻页。
        cx.update_window(handle, |_, window, cx| window.click("entry-auto", cx))
            .expect("窗口应能渲染");
        frames(cx);
        assert!(
            !has(cx, "reader-auto-bar"),
            "第一击只该开始，不该把设置顶到脸上"
        );
        assert!(
            !has(cx, "reader-bottom-bar"),
            "第一击该把浮层收干净，正文干净地读"
        );
        assert!(!has(cx, "entry-auto"), "浮钮随卡片一起不在场");

        // 第二击（运行中、正文任意处）：才叫出自动那条工具栏。
        cx.update_window(handle, |_, window, cx| window.click("folio-reader", cx))
            .expect("窗口应能渲染");
        frames(cx);
        assert!(
            has(cx, "reader-auto-bar"),
            "自动跑着时浮层该换成自动那条工具栏"
        );
        assert!(
            !has(cx, "reader-bottom-bar"),
            "自动那条在场时普通底栏该整个让位，而不是叠在下面"
        );
        assert!(
            has(cx, "auto-mode-page") && has(cx, "auto-mode-scroll"),
            "两格分段（翻页 / 滚动）是设置工具的一部分"
        );

        // 退出：自动停表，底栏回来。
        cx.update_window(handle, |_, window, cx| window.click("auto-exit", cx))
            .expect("窗口应能渲染");
        frames(cx);
        assert!(has(cx, "reader-bottom-bar"), "退出自动之后普通底栏该回来");
        assert!(
            !has(cx, "reader-auto-bar"),
            "自动停了，那条工具栏没有对象了"
        );
    }

    /// 整屏一张纸的另一半：**顶上状态栏那一条与底下手势条那一条也在正文的命中面里**。
    ///
    /// 「整屏一张纸」改的是纸面几何（`page_area` 从正文盒扩到整窗，见 `view::body` 的
    /// `sheet_area`，那一半的门在 `body/tests/swipe.rs` 的
    /// `a_curl_sheet_includes_the_header_and_footer_bands`）；这一条改的是**命中面**：
    /// 正文那一层从窗口顶铺到窗口底，所以两条带里的一击归正文，不归身后的架面。
    /// 哪天阅读层重新为安全区让出顶部（从前那一格 `pt(safe.top)`），这两条带立刻
    /// 不再是纸：点上去什么也不发生，屏幕上只留下「顶上那一条点不动」——
    /// 不报错、不动画，最难发现的那一类。
    ///
    /// 这里点的是**中间那一档**（开合浮层），不点左右：翻页要真的切出多页，
    /// 而「左三分之一 = 往回翻」这件事由 `chrome::zone` 自己那几条单元门钉，
    /// 两边合起来才是完整的一条链。
    #[gpui_kit::test]
    fn a_tap_on_either_system_band_reaches_the_body(cx: &mut TestAppContext) {
        let (handle, shell) = boot(cx, false);
        cx.update(|cx| cx.set_reduce_motion(true));
        open_test_reading(&shell, cx);
        let frames = |cx: &mut TestAppContext| {
            for _ in 0..3 {
                cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                    .expect("窗口应能渲染");
                cx.run_until_parked();
            }
        };
        frames(cx);
        let bar_open = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, _| {
                window.try_find("reader-bottom-bar").is_some()
            })
            .expect("窗口应能渲染")
        };
        assert!(!bar_open(cx), "前置：刚开书时浮层还没出来");
        let (width, height) = cx
            .update_window(handle, |_, window, _| {
                let size = window.viewport_size();
                (f32::from(size.width), f32::from(size.height))
            })
            .expect("量窗口");

        // 顶上那 2 像素（状态栏 / 挖孔那一条），横向取正中 → 浮层该开。
        cx.update_window(handle, |_, window, cx| {
            window.click_at("folio-reader", point(px(width * 0.5), px(2.0)), cx)
        })
        .expect("窗口应能渲染");
        frames(cx);
        assert!(bar_open(cx), "点状态栏那一条也要有反应：浮层该开起来");

        // 先把浮层收回正文那一档（浮层开着时那一击的归属是浮层，不是正文）。
        cx.update_window(handle, |_, window, cx| {
            window.click_at("folio-reader", point(px(width * 0.5), px(height * 0.5)), cx)
        })
        .expect("窗口应能渲染");
        frames(cx);
        assert!(
            !bar_open(cx),
            "前置：中间那一击要把浮层收掉，底下那一条才轮到正文接"
        );

        // 底下那 2 像素（手势条那一条）：同一档反过来该把浮层再开起来 —— 两侧都是纸。
        cx.update_window(handle, |_, window, cx| {
            window.click_at("folio-reader", point(px(width * 0.5), px(height - 2.0)), cx)
        })
        .expect("窗口应能渲染");
        frames(cx);
        assert!(bar_open(cx), "点手势条那一条也要有反应：浮层该开起来");
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
        open_test_reading(&shell, cx);
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

    /// 回归门：底部弹层开着时，返回先收那一层，绝不到「退出应用」那一步去。
    ///
    /// 读者报的是「笔记卡片开着，一下返回就回到系统主界面」。根因不是这一层没有键位，
    /// 而是键位排不上：gpui 把**无上下文**的绑键算成最深那一档，外壳后绑的
    /// `escape → Back` 永远压在 gpui-kit `base::Sheet` 的 `Cancel` 前面（见
    /// `components::dismiss_top` 的注释）。所以外壳在 `back()` 里自己问一句最上面是哪块浮层。
    #[gpui_kit::test]
    fn back_closes_the_open_sheet_before_it_reaches_the_root(cx: &mut TestAppContext) {
        use std::cell::Cell;
        use std::rc::Rc;

        use crate::ui::components::BottomSheetEvent;

        let (handle, shell) = boot(cx, false);
        let stack = shell.read_with(cx, |this, _| {
            this.stacks[Destination::Shelf.index()].clone()
        });
        // 一块真弹层，与架面那三块走同一条登记路（`BottomSheetState::closed` 登记进全局）。
        // 这条门要钉的是「返回先问浮层」这一句，不是某一屏的摆法。
        let sheet = cx.update(|cx| cx.new(BottomSheetState::closed));
        let dismissals = Rc::new(Cell::new(0));
        let sink = dismissals.clone();
        let _watch = cx.update(|cx| {
            cx.subscribe(&sheet, move |_, _: &BottomSheetEvent, _| {
                sink.set(sink.get() + 1)
            })
        });
        cx.update(|cx| sheet.update(cx, |sheet, cx| sheet.show(cx)));
        assert!(
            sheet.read_with(cx, |sheet, _| sheet.is_open()),
            "前置：浮层该开着"
        );

        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("窗口应能渲染");
        cx.simulate_keystrokes(handle, "escape");

        assert!(
            !sheet.read_with(cx, |sheet, _| sheet.is_open()),
            "返回该收掉浮层，而不是穿到栈底去（手机上那一步就是退出应用）"
        );
        assert_eq!(
            dismissals.get(),
            1,
            "这一下是读者按的：订阅方要收到 Dismissed，才能把「这一屏开着」的意图清掉"
        );
        assert_eq!(
            stack.read_with(cx, |state, _| state.depth()),
            1,
            "收浮层那一按不该顺手弹栈"
        );
        assert_eq!(
            shell.read_with(cx, |this, _| this.destination),
            Destination::Shelf,
            "也不该换目的地"
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
            1,
            "落回的是整架那一片（进了组的甲不在里面，只剩乙），不是组里那一本"
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
        open_test_reading(&shell, cx);
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
        open_test_reading(&shell, cx);
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
        open_test_reading(&shell, cx);
        let page = shell.read_with(cx, |this, _| {
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

    /// 回归门：手机端「我的」钻进展屏时，应用导航栏整条收起 = 那一屏占满整窗。
    ///
    /// 那一屏不在这几条目的地栈上（它是「我的」那一个目的地的栈底，自己攥着「现在站着哪一屏」），
    /// 所以底栏的判据除了栈深还要问它一句。钉的是渲染用的那一句 `AppShell::nav_gone`：底栏只长在
    /// 真机上（`SizeClass::of` 在桌面把窄窗口夹到 `Medium`），宿主进不到那一屏的画。
    ///
    /// 三个方向各防一种写反：判据漏掉设置那一问 → 钻进详情时底下那条栏还占着 56，读者看到的是
    /// 「同一屏的下半段换了内容」；不设目的地的门 → 停在详情里切去书架，那边的栏也没了；
    /// 退回根屏不重算 → 栏再也不回来。
    #[gpui_kit::test]
    fn drilling_into_the_account_settings_takes_the_whole_window(cx: &mut TestAppContext) {
        let (handle, shell) = boot(cx, false);
        let account = shell
            .read_with(cx, |this, _| this.account.clone())
            .expect("Compact 档的「我的」装的就是钻入式列表");

        let at = |cx: &mut TestAppContext| shell.read_with(cx, |this, cx| this.nav_gone(true, cx));
        cx.update_window(handle, |_, window, cx| {
            shell.update(cx, |this, cx| this.go(Destination::Account, window, cx));
            window.render_frame(cx);
        })
        .expect("窗口应能渲染");
        assert!(!at(cx), "站在根屏时底栏该在");

        cx.update(|cx| {
            account.update(cx, |account, cx| {
                account.drill_into_for_test(1, cx);
            })
        });
        assert!(at(cx), "钻进详情那一屏要吃满整窗");

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
        })
        .expect("窗口应能渲染");
        assert!(at(cx), "钻进详情那一屏要吃满整窗");

        // 退回根屏：判据跟着翻回 false（重画那一趟由外壳挂在 notify 上的观察者触发，
        // 这里只钉判据本身）。
        cx.update(|cx| {
            account.update(cx, |account, cx| {
                account.go_back(cx);
            })
        });
        assert!(!at(cx), "回到根屏底栏要跟着回来");

        // 停在详情里切去别的目的地：那边的栏不该被这一屏收走。
        cx.update(|cx| {
            account.update(cx, |account, cx| {
                account.drill_into_for_test(1, cx);
            })
        });
        assert!(at(cx), "前置：切走之前它确实钻进去了");
        cx.update_window(handle, |_, window, cx| {
            shell.update(cx, |this, cx| this.go(Destination::Shelf, window, cx));
            window.render_frame(cx);
        })
        .expect("窗口应能渲染");
        assert!(!at(cx), "「我的」停在详情里不该把书架的底栏也收掉");
    }

    /// 回归门：任何一块底部弹层开着，Compact 档那条**浮**栏整条收起 —— 模态那一层该盖住
    /// 导航栏，而不是被它压住下沿（外壳画完目的地才画那条栏）。
    ///
    /// 两个方向各防一种写反：判据里漏掉这一问 → 扫描清单升起来时底下那一条 56 还浮在它上面；
    /// 不先问档位 → 桌面上弹层一开侧栏就没了、整页横向抖一下（那两档的栏占布局行，本来不重叠）。
    /// 「光建起来不算」防的是第三种：登记了但没 `show`，读者还没看到任何一层，栏不该凭空消失。
    #[gpui_kit::test]
    fn an_open_bottom_sheet_takes_the_floating_bar_with_it(cx: &mut TestAppContext) {
        let (handle, shell) = boot(cx, false);
        let at = |compact: bool, cx: &mut TestAppContext| {
            shell.read_with(cx, move |this, cx| this.nav_gone(compact, cx))
        };
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("窗口应能渲染");
        assert!(!at(true, cx), "没有弹层时那条栏该在");

        let sheet = cx.update(|cx| cx.new(BottomSheetState::closed));
        assert!(!at(true, cx), "光建起来不算开着");

        cx.update(|cx| sheet.update(cx, |sheet, cx| sheet.show(cx)));
        assert!(at(true, cx), "手机上弹层开着，那条浮栏该整条收起");
        assert!(
            !at(false, cx),
            "桌面与平板那两档的侧栏不与弹层重叠，不该被这一问收掉"
        );

        cx.update(|cx| sheet.update(cx, |sheet, cx| sheet.hide(cx)));
        assert!(!at(true, cx), "关掉之后栏要跟着回来");
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
    /// 钉的是这三处接线的存在：`go(Stats)` 叫一次 `refresh`、快照拿到了才画那张图、
    /// 内容区与滚动层各自挂在元素 id 上。空库也算拿到了 —— 那一屏要说的是
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
                    window.try_find("stats-heatmap").is_some(),
                    window.try_find(("stats-halves", 0usize)).is_some(),
                    shell.read(cx).stats.read(cx).has_heatmap(),
                )
            })
            .expect("窗口应能渲染");
        assert!(drawn.0, "统计那一格应画出自己的根");
        assert!(drawn.1, "拿到快照就该画出那张热力图");
        assert!(
            drawn.2,
            "900 宽的内容区算得下两栏：短卡要并排，不许把手机那一列直接拉宽"
        );
        assert!(drawn.3, "切进来就要拿到一份快照，等下一帧再去查库就晚了");
    }

    // 一个库里必然不存在的 `book_id`。自增键从 1 起，所以登记一本拿到的就是 1 ——
    // 测试里「点开一本读不出来的书」不能撞上那本真的在册的，否则 `open_book` 会把
    // 源文件不在了的那本**连数据一起删**（那是它对「书从架上消失」的正确处置），
    // 后面那条会话就插不进外键了。

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
        let before = shell.read_with(cx, |this, cx| this.stats.read(cx).active_days());
        assert_eq!(before, Some(0), "内存库里此刻还没有一段阅读");

        // 那一行必须挂在库里真的存在的 `book_id` 上（外键），所以先登记一本。
        // 而**盖上来的那一层**故意用一个读不出来的 id：`open_book` 认不出库里的行
        // 就只写日志、不动别的，收层时那次重取照样发生 —— 这条门钉的是「收层会叫重取」，
        // 不是「书真能打开」。时间戳取现在附近：热力图看的是最近一年。
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
        open_test_reading(&shell, cx);
        store.record_session(book, now - 600, now, 0).unwrap();
        shell.update(cx, |this, cx| this.close_reading(cx));

        let after = shell.read_with(cx, |this, cx| this.stats.read(cx).active_days());
        assert_eq!(after, Some(1), "弹掉阅读页那次重取要把今天那一格带上来");
    }

    /// 五个区间都要真的画得出来。
    ///
    /// 「总」与「年」各带图表，空库时那几张图没数据就很容易在比例尺上空转；
    /// 这里塞两段真会话（今天 + 41 天前）然后逐档渲染，钉的是「有数据时也不 panic」，
    /// 以及月 / 周 / 年三档确实把热力图挂上了屏（外壳那扇回归门只看了默认那一档）。
    #[gpui_kit::test]
    fn every_stats_range_renders_with_data(cx: &mut TestAppContext) {
        let (handle, shell) = boot(cx, false);
        let store = cx.update(|cx| crate::ui::services::app_store::handle(cx).unwrap());
        let book = store
            .add_book(
                std::path::Path::new("/books/stats.epub"),
                &folio_core::DocMeta {
                    title: "Stats".into(),
                    authors: vec!["作者甲".into()],
                    format: folio_core::Format::Epub,
                    size: 1,
                    ..folio_core::content::format::DocMeta::default()
                },
            )
            .unwrap();
        let at = folio_core::infra::time::now();
        store.record_session(book, at - 3_000, at, 0).unwrap();
        store
            .record_session(book, at - 41 * 86_400 - 1_200, at - 41 * 86_400, 0)
            .unwrap();

        cx.update_window(handle, |_, window, cx| {
            shell.update(cx, |this, cx| this.go(Destination::Stats, window, cx));
            for index in 0..5 {
                shell.update(cx, |this, cx| {
                    this.stats
                        .update(cx, |stats, cx| stats.show_range_for_test(index, cx));
                });
                window.render_frame(cx);
                assert!(
                    window.try_find("folio-stats").is_some(),
                    "第 {index} 档应画出统计屏"
                );
                if index >= 2 {
                    assert!(
                        window.try_find("stats-heatmap").is_some(),
                        "第 {index} 档应画出热力图"
                    );
                }
            }
        })
        .expect("五档都应渲染成功");
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
