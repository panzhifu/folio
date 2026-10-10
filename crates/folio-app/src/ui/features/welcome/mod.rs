//! 首次启动的欢迎页：进应用之前先把书库位置说定。
//!
//! 它回答的是和书架那块「导入前确认」同一个问题（书拷到哪儿），只是在时间上更早 ——
//! 位置没确认过（[`folio_core::Library::is_confirmed`] 为假）时，外壳画的是这一屏而不是
//! 主界面。所以这一屏**没有取消**：要么用系统文件夹选择器挑一个，要么一键用建议位置。
//!
//! 不写 `cfg(mobile)`：两端都在首次启动时走这一屏，差别（安全区、选择器解析出的路径）
//! 由外壳与宿主管，见 `shell::AppShell` 与 `shared::library`。
//!
//! 两端的控件都用普通按钮，不用浮层：浮层在 Android 上现在点不住（见 `plans/facts.md`），
//! 而这一步是进应用的前提，点不动就等于进不去。

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{Icon, Sizable, v_flex};
use gpui_kit::{
    Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement, Render, Styled, Task,
    TestSupportExt as _, Window, div, px,
};

use crate::ui::shared::config_model::ConfigHolder;
use crate::ui::shared::l10n::tr;
use crate::ui::shared::library::{pick_dir, suggested_dir};

/// 内容列的两侧留白（逻辑像素）。手机上让按钮离屏幕边有距离，不至于顶到边。
const GUTTER: f32 = 24.0;

/// 内容列的最大宽度：桌面上把按钮拉满整屏会显得空，读起来也散。
const COLUMN: f32 = 420.0;

pub struct WelcomeView {
    /// 系统文件夹选择器那条异步链。`Task` 一 drop 任务就取消，所以要活到选完 ——
    /// 选中结果由 [`pick_dir`] 自己写进配置，外壳观察到「位置已确认」后把这一屏换掉。
    _pick: Option<Task<()>>,
}

impl WelcomeView {
    pub fn boot(_cx: &mut Context<Self>) -> Self {
        Self { _pick: None }
    }

    /// 用系统文件夹选择器挑一个书库目录。
    ///
    /// 复用 [`pick_dir`]：它打开 `prompt_for_paths{directories:true}`，选中即写
    /// `ConfigHolder::edit_library`。这里不必知道选没选中 —— 选完配置变了，外壳自然会重画。
    fn pick(&mut self, cx: &mut Context<Self>) {
        self._pick = Some(pick_dir(cx));
    }

    /// 一键用建议位置：`<数据根>/books`。Android 上落在应用专属外部目录，始终写得进去，
    /// 是选择器解析不出真实路径时的兜底（见 `shared::library::candidate_dirs`）。
    fn use_default(&mut self, cx: &mut Context<Self>) {
        let dir = suggested_dir();
        ConfigHolder::edit_library(cx, |library| {
            library.dir = dir.to_string_lossy().into_owned()
        });
    }
}

impl Render for WelcomeView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pick_entity = cx.entity().clone();
        let default_entity = cx.entity().clone();
        let pick_label = tr(cx, "welcome.pick");
        let default_label = tr(cx, "welcome.use_default");
        v_flex()
            .id("folio-welcome")
            .size_full()
            .items_center()
            .justify_center()
            .gap_4()
            .px(px(GUTTER))
            // 一段说明文字居中而不是靠左：这一屏整体是居中的一列，左对齐的段落会显得歪。
            .text_center()
            .child(Icon::new(IconName::BookOpen).large())
            .child(
                div()
                    .text_xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr(cx, "welcome.title")),
            )
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(COLUMN))
                    .gap_2()
                    // primary 表达的是「这一组决定里默认要走的那一条」：这里是真·选择文件夹。
                    .child(
                        Button::new("welcome-pick")
                            .primary()
                            .icon(IconName::FolderOpen)
                            .label(pick_label.clone().to_string())
                            .accessibility_label(pick_label)
                            .on_click(move |_, _, cx| {
                                pick_entity.update(cx, |this, cx| this.pick(cx))
                            }),
                    )
                    .child(
                        Button::new("welcome-use-default")
                            .ghost()
                            .label(default_label.clone().to_string())
                            .accessibility_label(default_label)
                            .on_click(move |_, _, cx| {
                                default_entity.update(cx, |this, cx| this.use_default(cx))
                            }),
                    ),
            )
            // 外壳的宿主测试按这一格确认「首次启动停在欢迎页」（同 `SettingsView`）。
            .test_support()
    }
}
