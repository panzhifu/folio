//! 系统字体选择器：一个可搜索的家族下拉。
//!
//! 它只回答「这台设备此刻注册了哪些家族」。列表在**建这个实体那一刻**取一次
//! （`TextSystem::all_font_names()` 枚举整张字体表、排序、去重，不是每帧做得起的活），
//! 之后由 `Combobox` 自己的搜索框过滤。当前值不在表里时补在第一项 —— 配置里手写了一个
//! 这台机器没有的名字时，触发器上仍要看得见它，而不是显示成一个空白下拉。
//!
//! 读写都走外面递进来的闭包：这个组件不认识 `ConfigHolder`，设置屏与阅读弹层可以各自
//! 把同一份值接到自己的落点上。

use std::rc::Rc;

use gpui_kit::component::combobox::{Combobox, ComboboxEvent, ComboboxState};
use gpui_kit::component::searchable_list::{SearchableListDelegate as _, SearchableVec};
use gpui_kit::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement, Render, Styled,
    Subscription, Window,
};

pub struct FontPicker {
    state: Entity<ComboboxState<SearchableVec<String>>>,
    /// 这台机器注册的家族（含补在最前面的当前值）。同步选中项时用它判「在不在表里」，
    /// 不在就**不碰**选中态 —— 否则每帧 `set_selected_values` 会把自己 notify 成死循环。
    families: Vec<String>,
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
        let families = with_current(cx.text_system().all_font_names(), &current);
        let items = SearchableVec::new(families.clone());
        let selected = items.position(&current).into_iter().collect::<Vec<_>>();
        let state = cx.new(|cx| ComboboxState::new(items, selected, window, cx).searchable(true));
        // `Change` 每选中一项就发一次；单选下拉取第一个。`Confirm`（收起时）不重复写，
        // 否则收起一个没改过的下拉也会多落一次盘。
        let watch = cx.subscribe(
            &state,
            move |_, event: &ComboboxEvent<SearchableVec<String>>, cx| {
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
            get,
            _watch: watch,
        }
    }
}

/// 系统字体表 + 当前值。当前值不在表里时补在第一项：配置里手写了一个这台机器没有的
/// 名字时，触发器上仍要看得见它，而不是显示成一个空白下拉。
fn with_current(mut installed: Vec<String>, current: &str) -> Vec<String> {
    if !current.trim().is_empty() && !installed.iter().any(|name| name == current) {
        installed.insert(0, current.to_string());
    }
    installed
}

impl Render for FontPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let current = (self.get)(cx);
        let selected = self.state.read(cx).selected_value();
        if selected.as_deref() != Some(current.as_str())
            && self.families.iter().any(|name| name == &current)
        {
            self.state.update(cx, |state, cx| {
                state.set_selected_values(std::slice::from_ref(&current), window, cx);
            });
        }
        // 占位文案每帧从当前 locale 取：切语言时这一格不用等元素重建。
        let placeholder = crate::ui::services::l10n::tr(cx, "font_picker.search");
        gpui_kit::div()
            .w_full()
            .child(Combobox::new(&self.state).search_placeholder(placeholder))
    }
}

#[cfg(test)]
mod tests {
    use super::with_current;

    #[test]
    fn a_current_font_missing_from_the_device_stays_visible() {
        let installed = vec!["Noto Serif".to_string(), "LXGW WenKai".to_string()];
        assert_eq!(
            with_current(installed.clone(), "Book Body"),
            vec![
                "Book Body".to_string(),
                "Noto Serif".to_string(),
                "LXGW WenKai".to_string(),
            ],
            "配置里的名字不在设备上时补在第一项，触发器不能是空的"
        );
        assert_eq!(
            with_current(installed.clone(), "Noto Serif"),
            installed,
            "已经在表里就不重复"
        );
        assert_eq!(
            with_current(installed.clone(), "  "),
            installed,
            "空值不补一项空白"
        );
    }
}
