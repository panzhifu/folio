//! 搜索框：两端共用的一颗「找东西」输入框。
//!
//! 把书架那条搜索框收成组件，是因为「点在框外面就退编辑态」这件事必须在框自己身上：
//! 外壳只知道焦点在不在某个输入框里（`field_holds_focus`，为返回键写的判据），不知道
//! 这个框画在哪一格，接不住「点空白」这种按坐标落点的手势。收进组件之后，将来任何
//! 一页（笔记、统计、正文全文检索）拿去用，行为都是同一份。
//!
//! 底下仍是 gpui-kit 的 [`Input`]：外观（搜索图标、可一键清空）交给它，这里只补
//! 两件它不管的事 ——
//!
//! * **点在外面退焦点**。软键盘只在焦点离开输入框时才收（`gpui-mobile` 的
//!   `text_input_state_changed(FocusLost)` 是唯一那条路），所以「收键盘」就是
//!   「退焦点」。判点用 gpui 的 `on_mouse_down_out`：它挂在框自己的 hitbox 上，
//!   点在框里（选词、挪光标、清空按钮）不会触发，点在框外任何地方都触发 ——
//!   点书卡、点空白、点导航栏，全部一致。
//! * **词变了往外报**。调用方订阅 [`SearchBoxEvent`] 拿查询词，不必再低头看
//!   gpui-kit 的 `InputEvent`。
//!
//! 尺寸走 [`Sizable`]：手机上一条满幅的框用默认档（拇指要按得中），塞进桌面
//! 工具栏时调 `.small()` 与旁边的按钮同高。

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{Icon, Sizable, Size};
use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, Focusable as _, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, Styled, Subscription, Window, div,
};

/// 搜索框向外报的事。
pub enum SearchBoxEvent {
    /// 查询词变了。清空（框上的 × 或全删）也走这一条，词就是空串。
    Change(SharedString),
}

/// 一颗搜索框。整颗是 [`Entity`] 挂在调用方身上，`render` 时把它当孩子放进去即可。
pub struct SearchBox {
    input: Entity<InputState>,
    size: Size,
    _watch: Subscription,
}

impl SearchBox {
    pub fn new(
        placeholder: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let _watch = cx.subscribe(&input, |_, state, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                cx.emit(SearchBoxEvent::Change(state.read(cx).value()));
            }
        });
        Self {
            input,
            size: Size::default(),
            _watch,
        }
    }

    /// 当前查询词。
    pub fn value(&self, cx: &App) -> SharedString {
        self.input.read(cx).value()
    }

    /// 把焦点放进框里。手机上焦点一进框软键盘就出来，不必再调什么「弹键盘」。
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }
}

impl EventEmitter<SearchBoxEvent> for SearchBox {}

impl Sizable for SearchBox {
    fn with_size(mut self, size: impl Into<Size>) -> Self {
        self.size = size.into();
        self
    }
}

impl Render for SearchBox {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let handle = self.input.read(cx).focus_handle(cx).clone();
        // `w_full` 不能省：[`Input`] 的根是 `size_full`（按容器铺满），容器不给定宽
        // 它就缩成内容宽。组件撑满调用方给的格子，宽多少是调用方的排版决定。
        div()
            .w_full()
            .on_mouse_down_out(move |_, window, cx| {
                // 焦点不在框里时不动别人：焦点落在别的控件上，一次框外的点击
                // 不该顺手把它也吹掉。
                if handle.is_focused(window) {
                    window.blur(cx);
                }
            })
            .child(
                Input::new(&self.input)
                    .with_size(self.size)
                    .cleanable(true)
                    .prefix(Icon::new(IconName::Search).small()),
            )
    }
}

#[cfg(all(test, not(mobile)))]
mod tests {
    //! 「点外面退编辑态」是这颗组件存在的理由，也是最容易悄悄坏掉的一处：
    //! 判点挂在 hitbox 上，外层布局一改（框换了个容器、外面多包一层）它就可能
    //! 失聪或失明 —— 失聪是点了不退，失明是点框自己也在退，后者比前者更糟
    //! （框根本没法输入）。两头都钉住。

    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui_kit::component::{Root, v_flex};
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        AnyWindowHandle, App, AppContext as _, Context, Entity, InteractiveElement as _,
        IntoElement, ParentElement as _, Render, SharedString, Styled as _, Subscription,
        TestAppContext, Window, div, point, px, size,
    };

    use super::{SearchBox, SearchBoxEvent};

    /// 一块顶上放着搜索框、下面整片空白的测试台。空白区不做任何事 ——
    /// 它就是「点在框外面」的那个落点。
    struct SearchHost {
        search: Entity<SearchBox>,
        changes: Rc<RefCell<Vec<SharedString>>>,
        /// 订阅必须活着事件才来：`Subscription` 一 drop 就退订。
        _watch: Subscription,
    }

    impl SearchHost {
        fn boot(window: &mut Window, cx: &mut Context<Self>) -> Self {
            let search = cx.new(|cx| SearchBox::new("找一本书", window, cx));
            let changes: Rc<RefCell<Vec<SharedString>>> = Rc::default();
            let sink = changes.clone();
            let _watch = cx.subscribe(&search, move |_, _, event: &SearchBoxEvent, _| {
                let SearchBoxEvent::Change(query) = event;
                sink.borrow_mut().push(query.clone());
            });
            Self {
                search,
                changes,
                _watch,
            }
        }

        /// 把焦点放进框里。走组件自己的 `focus`，与生产路径同一条。
        fn focus_search(&self, window: &mut Window, cx: &mut App) {
            self.search
                .update(cx, |search, cx| search.focus(window, cx));
        }
    }

    impl Render for SearchHost {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            v_flex()
                .size_full()
                .child(div().w_full().child(self.search.clone()))
                .child(div().id("blank-below").flex_1())
        }
    }

    fn boot(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<SearchHost>) {
        cx.update(gpui_kit::init);
        let mut host = None;
        let handle: AnyWindowHandle = cx
            .open_window(size(px(400.), px(600.)), |window, cx| {
                let view = cx.new(|cx| SearchHost::boot(window, cx));
                host = Some(view.clone());
                Root::new(view, window, cx)
            })
            .into();
        (handle, host.expect("测试台应已建起"))
    }

    /// 焦点先真真切切进框，再交回调用方断言 —— 前置失败要在这里炸，而不是
    /// 让后面的断言替它背锅。
    fn focus_and_assert(
        cx: &mut TestAppContext,
        handle: AnyWindowHandle,
        host: &Entity<SearchHost>,
    ) {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            host.update(cx, |host, cx| host.focus_search(window, cx));
            assert!(window.focused(cx).is_some(), "搜索框应真的拿到焦点");
        })
        .expect("窗口应能渲染");
    }

    /// 回归门：点在框外的空白上，编辑态要退掉 —— 焦点走了，软键盘才有路收。
    ///
    /// 手机上的软键盘只在焦点离开输入框时才收，所以「点空白没反应」在真机上的
    /// 表现就是键盘钉在屏上，整片下半屏被它压着。
    #[gpui_kit::test]
    fn clicking_blank_space_gives_up_the_search_focus(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        focus_and_assert(cx, handle, &host);

        cx.update_window(handle, |_, window, cx| {
            // 窗口中段（框的下方）落一次点击。
            let at = point(px(200.), px(400.));
            window.drag(at, at, cx);
            assert!(window.focused(cx).is_none(), "点在框外的空白上应退掉编辑态");
        })
        .expect("窗口应能渲染");
    }

    /// 回归门：点在框里**不**退 —— 没有这条，框就是一颗点一下就失焦的摆设，
    /// 连字都输不进去。判点用错边（`on_mouse_down_out` 写成 `on_mouse_down`）
    /// 时这条会红。
    #[gpui_kit::test]
    fn clicking_the_input_itself_keeps_the_focus(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        focus_and_assert(cx, handle, &host);

        cx.update_window(handle, |_, window, cx| {
            // 框钉在测试台顶部，正中就是框里。
            let at = point(px(200.), px(18.));
            window.drag(at, at, cx);
            assert!(window.focused(cx).is_some(), "点在框里不该退焦点");
        })
        .expect("窗口应能渲染");
    }

    /// 回归门：输入要一路报到 `SearchBoxEvent::Change` —— 调用方（书架）就靠
    /// 这一条重查库，事件断了的表现是「打字不筛书」。
    #[gpui_kit::test]
    fn typing_reports_the_query_through_change_events(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        focus_and_assert(cx, handle, &host);

        cx.update_window(handle, |_, window, cx| {
            window.input("ab", cx);
        })
        .expect("窗口应能渲染");

        let changes = host.read_with(cx, |host, _| host.changes.borrow().clone());
        // `InputEvent::Change` 是不带载荷的「值变了，来读」：两个字符各触发一条，
        // 交付批次里读到的都是**当前**值 —— 同一批 flush 里后到的键会把前面的
        // 词顶掉。调用方（书架）要的恰好是最新值，所以这里钉的是条数与最终值，
        // 不钉中间那条是 "a" 还是 "ab"（那是 flush 时机，不是行为）。
        assert_eq!(changes.len(), 2, "两个字符应报两次 Change");
        assert_eq!(
            changes.last().map(|q| q.as_ref()),
            Some("ab"),
            "最后一次 Change 读到的应是框里当前的词"
        );
        let value = host.read_with(cx, |host, cx| host.search.read(cx).value(cx));
        assert_eq!(value, "ab", "value() 应读出框里当前的词");
    }
}
