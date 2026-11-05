//! AI：划词「问 AI」与 `.txt`「建议切章」共用的那一屏 —— 开不开、问哪只模型、往哪个端点问。
//!
//! ## 为什么单独一屏
//!
//! 这一组原先挂在「翻译」屏的末尾（`pages/translation.rs` 里 `ai.enabled` 就把端点那一组
//! 也摆出来）。两件事在配置里读的是**同一份端点**不假，可它们管的东西不一样：翻译那几格
//! 换的是「这段话由哪只引擎译成哪种语言」，AI 这一格换的是**划词卡片上有没有那一格**。
//! 后者是个入口开关，让读者在十来格里找一颗藏在别的屏末尾的开关，症状就是「功能没做」。
//!
//! ## 端点那一组不是抄的第二份
//!
//! 这里调的是 [`translation::openai_group`]，只换一个组标题。两屏改的都是
//! `infra::config::OpenAi` 那三个字段，而字段只有一份 —— 抄一份表单的后果是以后加一格
//! 只加进一屏，两屏就说两套话；那种分叉在屏上看不出来，要等到读者在另一屏发现自己
//! 填的东西没出现。门写在 `view/mod.rs` 的 `the_ai_and_translation_pages_edit_the_same_endpoint`：
//! 一屏写进去，另一屏读出来必须还是那一个值。
//!
//! ## 「AI 用的模型」与端点里那只模型的区别
//!
//! 端点那一组的模型是**翻译用的那只**，AI 这一格留空就跟随它；填了只换 AI 这一路。
//! 两格都在一屏上，所以两句说明都必须自指自说（不许写「上面那一组」—— 换档、搜索命中
//! 单行、或者以后挪组都会让那句话指错东西）。

use gpui_kit::{App, SharedString};

use super::super::*;
use super::translation;

pub(in crate::ui::features::settings) fn ai_page(cx: &App) -> Page {
    page(
        tr(cx, "page.ai"),
        IconName::Sparkles,
        vec![
            ai_group(cx),
            // 与翻译屏同一个构造器：地址、模型、密钥三格，字段共用一份。
            translation::openai_group(cx, tr(cx, "group.ai_endpoint")),
        ],
    )
}

/// AI 自己那一组：只有「要不要」与「用哪只模型」两格。
fn ai_group(cx: &App) -> Group {
    group(
        tr(cx, "group.ai"),
        vec![
            Row::toggle(
                tr(cx, "field.ai_enabled"),
                |cx| ConfigHolder::ai(cx).enabled,
                |on, cx| ConfigHolder::edit_ai(cx, |ai| ai.enabled = on),
            )
            .description(tr(cx, "field.ai_enabled.desc"))
            .keywords(&["ai", "ask", "gpt", "model", "问 ai", "人工智能", "助手"]),
            Row::text(
                tr(cx, "field.ai_model"),
                |cx| SharedString::from(ConfigHolder::ai(cx).model),
                |value, cx| ConfigHolder::edit_ai(cx, |ai| ai.model = value.to_string()),
            )
            .description(tr(cx, "field.ai_model.desc"))
            .keywords(&["ai", "model", "模型"]),
        ],
    )
}

#[cfg(test)]
mod tests {
    //! 这一屏钉的是两件事：**开关写的是哪一节**，以及**端点那一组与翻译屏是不是同一个
    //! 来源**。第二件才是这一屏存在的风险所在 —— 两屏各摆一份表单的话，以后加一格只加进
    //! 一屏，两屏就说两套话，而那种分叉在屏上看不出来。

    use super::ai_page;
    use crate::ui::features::settings::model::spec::{Page, Row, RowKind, Setter};
    use crate::ui::features::settings::view::ConfigHolder;
    use crate::ui::features::settings::view::pages::translation::translation_page;
    use crate::ui::services::l10n::tr;
    use folio_core::Config;
    use gpui_kit::{App, SharedString, TestAppContext};
    use std::rc::Rc;

    /// 按**行标题**找一行。按序号找的话，任何一屏加一行就把断言指到别的行上去；
    /// 屏名进 panic 是为了说清少的是哪一屏那一格。
    fn row<'a>(page: &'a Page, which: &str, title: &str) -> &'a Row {
        page.groups
            .iter()
            .flat_map(|group| group.rows.iter())
            .find(|item| item.title.as_str() == title)
            .unwrap_or_else(|| panic!("{which} 没有标题为「{title}」的行"))
    }

    /// 取一行的文本读写两格。不是文本行就直接红 —— 行型换了要重写这条断言，
    /// 而不是让它悄悄什么都不验。
    /// 取值那一格的形状：`Setter<A>` 已经住在规格里（`model::spec`），读的这一格只有
    /// 测试要用，就在这里起名字 —— 两条都写全会被 clippy 报 `type_complexity`。
    type Getter = dyn Fn(&App) -> SharedString;

    fn text(row: &Row) -> (&Rc<Getter>, &Setter<SharedString>) {
        match &row.kind {
            RowKind::Text { get, set } => (get, set),
            _ => panic!("这一行该是文本行（行型换了要重写这条断言，别让它空跑）"),
        }
    }

    /// 「AI 屏填的地址与密钥」必须就是「翻译屏读到那两格」的那一份配置。
    #[gpui_kit::test]
    fn the_ai_and_translation_pages_edit_the_same_endpoint(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx: &mut App| {
            ConfigHolder::install(Config::default(), cx);
            // 翻译屏那组端点只在选中 OpenAI 兼容那一档时摆（引擎私有的参数藏在别的档不该
            // 露出来），所以这一条要先把引擎换过去 —— AI 屏那一组是**恒摆**的，它不看翻译
            // 用哪只引擎。
            ConfigHolder::edit_translation(cx, |translation| {
                translation.engine = folio_core::TranslationEngine::OpenAi;
            });
            let base = tr(cx, "field.translation_openai_base").to_string();
            let key = tr(cx, "field.translation_openai_key").to_string();
            let ai = ai_page(cx);
            let translated = translation_page(cx);

            let (_, set_base) = text(row(&ai, "AI 屏", &base));
            set_base(SharedString::from("https://api.example.test/v1"), cx);
            let (read_base, _) = text(row(&translated, "翻译屏", &base));
            assert_eq!(
                read_base(cx).as_str(),
                "https://api.example.test/v1",
                "AI 屏写进去的地址，翻译屏读出来是别的 —— 两屏在改两份配置"
            );

            let (_, set_key) = text(row(&ai, "AI 屏", &key));
            set_key(SharedString::from("sk-from-ai-page"), cx);
            assert_eq!(
                ConfigHolder::translation(cx).openai.api_key,
                "sk-from-ai-page",
                "AI 屏填的密钥没落到那份共用配置上"
            );
        });
    }

    /// 开关与模型写的是 `Config.ai` 那一节；改 AI 的模型不许动翻译那格的模型。
    #[gpui_kit::test]
    fn the_ai_switch_writes_the_ai_section(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx: &mut App| {
            ConfigHolder::install(Config::default(), cx);
            let page = ai_page(cx);
            let switch = row(&page, "AI 屏", tr(cx, "field.ai_enabled").as_str());
            let (get, set) = match &switch.kind {
                RowKind::Toggle { get, set } => (get, set),
                _ => panic!("那一行该是个开关（行型换了要重写这条断言，别让它空跑）"),
            };
            assert!(!get(cx), "开局该是关的：默认关是隐私口径的一部分");
            set(true, cx);
            assert!(ConfigHolder::ai(cx).enabled, "开关没写进 `Config.ai`");

            let (read_model, write_model) =
                text(row(&page, "AI 屏", tr(cx, "field.ai_model").as_str()));
            write_model(SharedString::from("big-model"), cx);
            assert_eq!(read_model(cx).as_str(), "big-model");
            assert_eq!(ConfigHolder::ai(cx).model, "big-model");
            // 出厂那只翻译用的模型不该被 AI 这一格带动（两个字段，一处真相各自成立）。
            assert_eq!(
                ConfigHolder::translation(cx).openai.model,
                Config::default().translation.openai.model,
                "改 AI 的模型把翻译那格的模型也动了"
            );
        });
    }
}
