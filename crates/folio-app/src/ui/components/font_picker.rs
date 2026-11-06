//! 系统字体选择器：一个可搜索、分两组（书籍字体 / 系统字体）的家族下拉。
//!
//! 它只回答「这台设备此刻注册了哪些家族」。列表在建实体那一刻取一次
//! （`TextSystem::all_font_names()` 枚举整张字体表、排序、去重，不是每帧做得起的活），
//! 之后由 `Combobox` 自己的搜索框过滤。字体表变宽（开书注册内嵌字体、移动端系统字体
//! 扫描完成，见 `platform::system_fonts`）时由 `font_primary::families_revision` 对账，
//! 版本变了就在下一次渲染里重取名单、重建分组 —— 已建的实体不必等重建。
//!
//! 分组：随书注册进进程的家族进「书籍字体」（`font_primary::is_book_family`），其余是
//! 「系统字体」；书籍组为空时整组不出现，那时它与一个普通下拉无异。当前值不在表里时
//! 补进它所属的那组第一项 —— 配置里手写了一个这台机器没有的名字时，触发器上仍要
//! 看得见它，而不是显示成一个空白下拉。
//!
//! 读写都走外面递进来的闭包：这个组件不认识 `ConfigHolder`，设置屏与阅读弹层可以各自
//! 把同一份值接到自己的落点上。

use std::rc::Rc;

use gpui_kit::component::combobox::{Combobox, ComboboxEvent, ComboboxState};
use gpui_kit::component::searchable_list::{
    SearchableGroup, SearchableListDelegate as _, SearchableVec,
};
use gpui_kit::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement, Render, Styled,
    Subscription, Window,
};

use crate::ui::foundation::font_primary;
use crate::ui::services::l10n;

pub struct FontPicker {
    state: Entity<ComboboxState<SearchableVec<SearchableGroup<String>>>>,
    /// 这台机器注册的家族（平铺，含补进去的当前值）。同步选中项时用它判「在不在表里」，
    /// 不在就**不碰**选中态 —— 否则每帧 `set_selected_values` 会把自己 notify 成死循环。
    families: Vec<String>,
    /// 建列表那一刻的字体表版本。`render` 每次对账，变了就重取重建。
    revision: u64,
    get: Rc<dyn Fn(&App) -> String>,
    _watch: Subscription,
}

/// 写回字体家族那一格的闭包。
pub type FontFamilySetter = Rc<dyn Fn(String, &mut App)>;

impl FontPicker {
    /// `get` / `set` 就是这一格值的读写口。
    pub fn new(
        get: Rc<dyn Fn(&App) -> String>,
        set: FontFamilySetter,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        let current = get(cx);
        // 随书字体里没有拉丁 `m` 的那几张（gpui 的 `load_family` 会剔掉，剔空就 panic）
        // 不进选择器：选中它也排不出来，只会让读者以为设置没生效。
        let (groups, families) = font_groups(
            font_primary::usable_names(cx.text_system().all_font_names()),
            &current,
            &l10n::tr(cx, "font_picker.group.book"),
            &l10n::tr(cx, "font_picker.group.system"),
        );
        let revision = font_primary::families_revision();
        let items = SearchableVec::new(groups);
        let selected = items.position(&current).into_iter().collect::<Vec<_>>();
        let state = cx.new(|cx| ComboboxState::new(items, selected, window, cx).searchable(true));
        // `Change` 每选中一项就发一次；单选下拉取第一个。`Confirm`（收起时）不重复写，
        // 否则收起一个没改过的下拉也会多落一次盘。
        let watch = cx.subscribe(
            &state,
            move |_, event: &ComboboxEvent<SearchableVec<SearchableGroup<String>>>, cx| {
                if let ComboboxEvent::Change(values) = event
                    && let Some(value) = values.first()
                {
                    set(value.clone(), cx);
                }
            },
        );
        Self {
            state,
            families,
            revision,
            get,
            _watch: watch,
        }
    }

    /// 字体表版本变了：重取名单、重建分组，把整份 delegate 换掉。
    ///
    /// `set_items` 不动选中态，索引可能还指着旧表的位置 —— 交给 `render` 下面那段
    /// 「`selected` 与 `current` 不一致就 `set_selected_values`」的既有对账归位。
    fn rebuild(&mut self, current: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (groups, families) = font_groups(
            font_primary::usable_names(cx.text_system().all_font_names()),
            current,
            &l10n::tr(cx, "font_picker.group.book"),
            &l10n::tr(cx, "font_picker.group.system"),
        );
        self.families = families;
        self.revision = font_primary::families_revision();
        self.state
            .update(cx, |state, cx| state.set_items(SearchableVec::new(groups), window, cx));
    }
}

/// 分组：随书注册的进「书籍字体」（排在前面），其余进「系统字体」；空组不出现。
/// 当前值不在表里时补进它所属的那组第一项 —— 分组判据同为 [`font_primary::is_book_family`]。
///
/// 交回 `(分组, 平打名单)`：平打名单给选中同步判「在不在表里」用，顺序与分组一致。
fn font_groups(
    installed: Vec<String>,
    current: &str,
    book_title: &str,
    system_title: &str,
) -> (Vec<SearchableGroup<String>>, Vec<String>) {
    let mut book = Vec::new();
    let mut system = Vec::new();
    for name in installed {
        if font_primary::is_book_family(&name) {
            book.push(name);
        } else {
            system.push(name);
        }
    }
    let mut flat = Vec::with_capacity(book.len() + system.len() + 1);
    flat.extend(book.iter().cloned());
    flat.extend(system.iter().cloned());
    if !current.trim().is_empty() && !flat.iter().any(|name| name == current) {
        let group = if font_primary::is_book_family(current) {
            &mut book
        } else {
            &mut system
        };
        group.insert(0, current.to_string());
        flat.insert(0, current.to_string());
    }
    let mut groups = Vec::new();
    if !book.is_empty() {
        groups.push(SearchableGroup::new(book_title).items(book));
    }
    if !system.is_empty() || groups.is_empty() {
        groups.push(SearchableGroup::new(system_title).items(system));
    }
    (groups, flat)
}

impl Render for FontPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let current = (self.get)(cx);
        // 字体表变宽了（开书注册内嵌字体、移动端系统字体扫描完成）→ 重取名单重建分组。
        if self.revision != font_primary::families_revision() {
            self.rebuild(&current, window, cx);
        }
        let selected = self.state.read(cx).selected_value();
        if selected.as_deref() != Some(current.as_str())
            && self.families.iter().any(|name| name == &current)
        {
            self.state.update(cx, |state, cx| {
                state.set_selected_values(std::slice::from_ref(&current), window, cx);
            });
        }
        // 占位文案每帧从当前 locale 取：切语言时这一格不用等元素重建。
        let placeholder = l10n::tr(cx, "font_picker.search");
        gpui_kit::div()
            .w_full()
            .child(Combobox::new(&self.state).search_placeholder(placeholder))
    }
}

#[cfg(test)]
mod tests {
    use super::font_groups;
    use crate::ui::foundation::font_primary::note_book_family;

    /// 书籍组在前、系统组在后；书籍组为空时整组不出现（与一个普通下拉无异）。
    /// 组内保持 `all_font_names()` 递进来的排序。
    #[test]
    fn a_registered_book_font_gets_its_own_group_on_top() {
        // 名字取只在本测试里用的串，避免与并行测试互踩登记表。
        note_book_family("Folio Test Picker Book Serif");

        let (groups, flat) = font_groups(
            vec![
                "LXGW WenKai".to_string(),
                "Folio Test Picker Book Serif".to_string(),
                "Noto Serif".to_string(),
            ],
            "LXGW WenKai",
            "书籍字体",
            "系统字体",
        );
        assert_eq!(groups.len(), 2, "书籍组与系统组都在场");
        assert_eq!(groups[0].title, "书籍字体");
        assert_eq!(
            groups[0].items,
            vec!["Folio Test Picker Book Serif".to_string()]
        );
        assert_eq!(groups[1].title, "系统字体");
        assert_eq!(
            groups[1].items,
            vec!["LXGW WenKai".to_string(), "Noto Serif".to_string()]
        );
        assert_eq!(
            flat,
            vec![
                "Folio Test Picker Book Serif".to_string(),
                "LXGW WenKai".to_string(),
                "Noto Serif".to_string(),
            ],
            "平打名单与分组同序，给选中同步判「在不在表里」"
        );

        let (groups, _) = font_groups(
            vec!["Noto Serif".to_string()],
            "Noto Serif",
            "书籍字体",
            "系统字体",
        );
        assert_eq!(groups.len(), 1, "没有随书字体时只剩系统组");
        assert_eq!(groups[0].title, "系统字体");
    }

    /// 当前值不在表里时补进它所属那组的第一项：普通名字进系统组，随书名字进书籍组。
    /// 配置里手写了一个这台机器没有的名字时，触发器不能是空白下拉。
    #[test]
    fn a_current_font_missing_from_the_device_stays_visible() {
        let installed = vec!["Noto Serif".to_string(), "LXGW WenKai".to_string()];

        let (groups, flat) = font_groups(installed.clone(), "Book Body", "书籍字体", "系统字体");
        assert_eq!(
            flat,
            vec![
                "Book Body".to_string(),
                "Noto Serif".to_string(),
                "LXGW WenKai".to_string(),
            ]
        );
        assert_eq!(groups[0].title, "系统字体", "Book Body 不是随书注册的");
        assert_eq!(groups[0].items[0], "Book Body");

        note_book_family("Folio Test Picker Missing Book Face");
        let (groups, flat) = font_groups(
            installed.clone(),
            "Folio Test Picker Missing Book Face",
            "书籍字体",
            "系统字体",
        );
        assert_eq!(groups[0].title, "书籍字体", "随书名字补进书籍组第一项");
        assert_eq!(groups[0].items[0], "Folio Test Picker Missing Book Face");
        assert_eq!(flat[0], "Folio Test Picker Missing Book Face");

        let (_, flat) = font_groups(installed.clone(), "Noto Serif", "书籍字体", "系统字体");
        assert_eq!(flat, installed, "已经在表里就不重复");

        let (_, flat) = font_groups(installed, "  ", "书籍字体", "系统字体");
        assert_eq!(flat.len(), 2, "空值不补一项空白");
    }
}
