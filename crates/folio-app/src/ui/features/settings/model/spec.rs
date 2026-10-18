//! 设置页的中立规格：两端共用的**唯一数据源**。
//!
//! 为什么需要这一层：`SettingField` 的取值/赋值闭包是 `pub(crate)`，桌面 `Settings`
//! 组件是唯一读得到它们的渲染器 —— 手机端想吃同一份数据，就必须让数据先落在一份
//! 两边都读得到的形状上。于是页面文件只产出这里的 [`Page`] / [`Group`] / [`Row`]，
//! 桌面映射回 `SettingPage`（见 `mod.rs` 的 `pages`），手机（`compact.rs`）直接渲染；
//! 一个字段的取值闭包、说明与关键词只写一遍，两端不会各说一套。
//!
//! 规格按**每次渲染**现建（和过去的页面数组一样），所以文案取的是当时的全局 locale ——
//! 切语言后两端都跟着换词。

use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::{AnyElement, App, SharedString};

use crate::ui::services::l10n::tr;

/// 一个数值字段的规格：标签键、说明键、搜索关键词，以及可填范围与步长。
///
/// 范围必须在场：`NumberFieldOptions` 的夹取与手机端步进都靠它兜底 ——
/// 面板里填进 9000 号字会把排版算成零高或无限宽。
#[derive(Clone, Copy)]
pub(crate) struct NumberSpec {
    pub title: &'static str,
    pub description: &'static str,
    pub keywords: &'static [&'static str],
    pub min: f32,
    pub max: f32,
    pub step: f32,
}

/// 一行设置项：标题 + 说明 + 搜索关键词 + 一种控件（[`RowKind`]）。
pub(crate) struct Row {
    pub title: SharedString,
    pub desc: Option<SharedString>,
    pub keywords: &'static [&'static str],
    pub kind: RowKind,
}

/// 写入器那一格：四个带写入器的行型形状相同，只差新值的类型。写全
/// `Rc<dyn Fn(SharedString, &mut App)>` 那一族会被 clippy 报 `type_complexity`，
/// 而这一格在四个地方重复，起个名字比数它的复杂度便宜。
pub(crate) type Setter<A> = Rc<dyn Fn(A, &mut App)>;

/// 行的控件种类。真源是 [`crate::ui::services::config::ConfigHolder`]，
/// 这里只是把「读哪个字段、写哪个字段」钉死。
///
/// 取值与赋值装的是**装箱闭包**而不是函数指针：「浅深各用哪套主题」那一行的 getter
/// 要带着「我是浅侧还是深侧」走（见 `appearance.rs` 的 `theme_item`），函数指针装不下。
/// 两端的渲染器都只要求 `Fn`，装箱不损失什么 —— 规格每次渲染重建，闭包也就跟着重建。
pub(crate) enum RowKind {
    /// 开关。
    Toggle {
        get: Rc<dyn Fn(&App) -> bool>,
        set: Setter<bool>,
    },
    /// 多选一。`options` 是 (值, 标签) 对 —— 值是写回配置的形态，标签是显示文字；
    /// `scrollable` 只对桌面有意义（弹层是否滚动），手机的弹层本来就滚。
    ///
    /// `push` 只管手机端那一端怎么交：`false` 是**浮层菜单**（就地弹出、点中即收），
    /// `true` 是**推一屏勾选**（整屏都是命中区，适合项多或名字长的）。桌面两端都渲染成
    /// 同一个 `Select`：这条差别是版式的，不是数据的。
    Dropdown {
        options: Rc<Vec<(SharedString, SharedString)>>,
        scrollable: bool,
        push: bool,
        get: Rc<dyn Fn(&App) -> SharedString>,
        set: Setter<SharedString>,
    },
    /// 数值输入。桌面上是数字输入框，手机上是 −/+ 步进行。
    ///
    /// 单位是 f32，与 `config::Reader` 里的字段一致；桌面那侧要 f64，在 `mod.rs`
    /// 映射时换算，规格本身不跟着组件的口味走。
    Number {
        spec: NumberSpec,
        get: Rc<dyn Fn(&App) -> f32>,
        set: Setter<f32>,
    },
    /// 自由文本（字体家族、书库位置）。
    Text {
        get: Rc<dyn Fn(&App) -> SharedString>,
        set: Setter<SharedString>,
    },
    /// 一行「读得出但改不了」的事实：标签在左，值在右（版本、commit 那几行）。
    Info { value: SharedString },
    /// 自定义渲染行：桌面与手机都直接调用这段闭包摆内容。「选择文件夹…」按钮、
    /// 传书状态这类**没有标题位**的行用 [`Row::custom`]；「配置文件在哪儿」这类
    /// 标题 + 内容的行用 [`Row::custom_titled`]。
    Custom {
        title: Option<SharedString>,
        render: Rc<dyn Fn(&App) -> AnyElement>,
    },
}

impl Row {
    pub(in crate::ui::features::settings) fn toggle(
        title: SharedString,
        get: impl Fn(&App) -> bool + 'static,
        set: impl Fn(bool, &mut App) + 'static,
    ) -> Self {
        Self {
            title,
            desc: None,
            keywords: &[],
            kind: RowKind::Toggle {
                get: Rc::new(get),
                set: Rc::new(set),
            },
        }
    }

    /// 一行「多选一」，手机端用浮层菜单就地选。
    pub(in crate::ui::features::settings) fn dropdown(
        title: SharedString,
        options: Vec<(SharedString, SharedString)>,
        scrollable: bool,
        get: impl Fn(&App) -> SharedString + 'static,
        set: impl Fn(SharedString, &mut App) + 'static,
    ) -> Self {
        Self::choice_with(false, title, options, scrollable, get, set)
    }

    /// 同一份「多选一」，手机端改成**推一屏勾选**：项多、名字长的时候，浮层要盖掉半屏，
    /// 而推入式的每一行都能整行命中 44。桌面不受这一格影响。
    pub(in crate::ui::features::settings) fn choice(
        title: SharedString,
        options: Vec<(SharedString, SharedString)>,
        scrollable: bool,
        get: impl Fn(&App) -> SharedString + 'static,
        set: impl Fn(SharedString, &mut App) + 'static,
    ) -> Self {
        Self::choice_with(true, title, options, scrollable, get, set)
    }

    fn choice_with(
        push: bool,
        title: SharedString,
        options: Vec<(SharedString, SharedString)>,
        scrollable: bool,
        get: impl Fn(&App) -> SharedString + 'static,
        set: impl Fn(SharedString, &mut App) + 'static,
    ) -> Self {
        Self {
            title,
            desc: None,
            keywords: &[],
            kind: RowKind::Dropdown {
                options: Rc::new(options),
                scrollable,
                push,
                get: Rc::new(get),
                set: Rc::new(set),
            },
        }
    }

    pub(in crate::ui::features::settings) fn text(
        title: SharedString,
        get: impl Fn(&App) -> SharedString + 'static,
        set: impl Fn(SharedString, &mut App) + 'static,
    ) -> Self {
        Self {
            title,
            desc: None,
            keywords: &[],
            kind: RowKind::Text {
                get: Rc::new(get),
                set: Rc::new(set),
            },
        }
    }

    pub(in crate::ui::features::settings) fn info(title: SharedString, value: impl Into<SharedString>) -> Self {
        Self {
            title,
            desc: None,
            keywords: &[],
            kind: RowKind::Info {
                value: value.into(),
            },
        }
    }

    pub(in crate::ui::features::settings) fn custom(render: impl Fn(&App) -> AnyElement + 'static) -> Self {
        Self {
            title: SharedString::default(),
            desc: None,
            keywords: &[],
            kind: RowKind::Custom {
                title: None,
                render: Rc::new(render),
            },
        }
    }

    pub(in crate::ui::features::settings) fn custom_titled(
        title: SharedString,
        render: impl Fn(&App) -> AnyElement + 'static,
    ) -> Self {
        Self {
            title: SharedString::default(),
            desc: None,
            keywords: &[],
            kind: RowKind::Custom {
                title: Some(title),
                render: Rc::new(render),
            },
        }
    }

    pub(in crate::ui::features::settings) fn description(mut self, desc: impl Into<SharedString>) -> Self {
        self.desc = Some(desc.into());
        self
    }

    pub(in crate::ui::features::settings) fn keywords(mut self, keywords: &'static [&'static str]) -> Self {
        self.keywords = keywords;
        self
    }
}

/// 一组 = 标题 + 若干行。
pub(crate) struct Group {
    pub title: SharedString,
    pub rows: Vec<Row>,
}

/// 一页 = 标题 + 图标 + 若干组。
pub(crate) struct Page {
    pub title: SharedString,
    pub icon: IconName,
    pub groups: Vec<Group>,
}

/// 一组设置行的快捷构造：标题 + 行。
pub(crate) fn group(title: impl Into<SharedString>, rows: Vec<Row>) -> Group {
    Group {
        title: title.into(),
        rows,
    }
}

/// 一页的快捷构造：标题 + 图标 + 组。
pub(crate) fn page(title: impl Into<SharedString>, icon: IconName, groups: Vec<Group>) -> Page {
    Page {
        title: title.into(),
        icon,
        groups,
    }
}

/// 一行数值字段。与 [`Row`] 上那一族构造器的区别是标题、说明、关键词都从
/// [`NumberSpec`] 里按当前 locale 现译，所以多收一个 `cx`；范围与步长也住在
/// 规格里，两端的夹取才不会各说一套。
pub(crate) fn number(
    cx: &App,
    spec: NumberSpec,
    get: impl Fn(&App) -> f32 + 'static,
    set: impl Fn(f32, &mut App) + 'static,
) -> Row {
    Row {
        title: tr(cx, spec.title),
        desc: Some(tr(cx, spec.description)),
        keywords: spec.keywords,
        kind: RowKind::Number {
            spec,
            get: Rc::new(get),
            set: Rc::new(set),
        },
    }
}

/// 手机根屏那一行右端的「当前值」：按行序取这一页里**第一个读得出值**的行。
///
/// 值是从那一行自己的 getter 与同一份选项表里取的，不是新抄的一份：改完设置回根屏，
/// 那格跟着变，不会有两处各说一套。取不到就返回 `None`，根屏那一行只留标题与箭头 ——
/// 「挂哪个字段当一页的代表值」这条规则只写在这一个地方，两端都不必再猜。
///
/// 已知代价：一页只露一个数，所以「文字」那页露的是字号而看不到行距；数值为 0 的
/// 那一档（最大行长 0＝铺满）在这里念出来是个「0」。
pub(crate) fn page_summary(page: &Page, cx: &App) -> Option<SharedString> {
    page.groups
        .iter()
        .flat_map(|group| group.rows.iter())
        .find_map(|row| match &row.kind {
            RowKind::Info { value } => (!value.is_empty()).then(|| value.clone()),
            RowKind::Dropdown { options, get, .. } => {
                let current = get(cx);
                Some(
                    options
                        .iter()
                        .find(|(value, _)| *value == current)
                        .map(|(_, label)| label.clone())
                        .unwrap_or(current),
                )
            }
            // 显示格式与步进行同一句：整数步长念整数，小数步长念两位。
            RowKind::Number { spec, get, .. } => {
                let value = get(cx);
                Some(
                    if spec.step >= 1.0 {
                        format!("{value:.0}")
                    } else {
                        format!("{value:.2}")
                    }
                    .into(),
                )
            }
            RowKind::Text { get, .. } => {
                let value = get(cx);
                (!value.is_empty()).then_some(value)
            }
            RowKind::Toggle { .. } | RowKind::Custom { .. } => None,
        })
}
