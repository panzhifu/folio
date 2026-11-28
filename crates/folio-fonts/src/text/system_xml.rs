//! Android 系统字体字典（`fonts.xml` / `font_fallback.xml`）的解析 —— **系统自己
//! 登记的账本**：哪些文件组成哪个家族、家族有哪些别名、语言回退链怎么排。
//!
//! 字节级注册只认字体文件自己的 name 表；而系统对这个文件的真实叫法（family 名、
//! `arial` 这类别名、ttc 里按 index 取的那张脸、变体字重的轴映射）都在这份 XML 里。
//! 不读它，「sans-serif 指哪个文件」「这个文件还叫什么」就只能靠猜 —— 这正是扫描
//! 判重与别名归一缺的那一层。
//!
//! 各家 ROM 会改这份文件，AOSP 新版还在向 `font_config.json` 迁移 —— 所以这里是
//! **尽力解析**：打不开、不是合法 XML 都回空账本，扫描退回纯 name 表口径，绝不拦
//! 启动。解析不认识的元素直接跳过；`<font>` 里嵌的 `<axis>` 变体轴、`index`、
//! `postScriptName` 目前都不消费（子集化与选面各有各的层）。

use std::collections::HashMap;

/// 一份系统字体账本。两个视图都按**文件名**（fonts.xml 里的相对路径，调用方自己拼
/// 目录前缀）索引。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SystemFontConfig {
    /// 字体文件 → 直接引用它的家族名（`<family name=…>`）。lang-only 的无名家族
    /// 只记文件、不给名 —— 那些文件仍要参与判重，只是没有可选的家族名。
    pub names_by_file: HashMap<String, Vec<String>>,
    /// 别名 → 被指向的家族名（`<alias name="arial" to="sans-serif"/>`）。
    pub alias_targets: HashMap<String, String>,
}

impl SystemFontConfig {
    /// 一份文件的全部叫法：直接所属的家族名 + 指向这些家族的别名。
    /// 系统账本里没有的文件回空 —— 调用方退回 name 表自己解析出的名字。
    pub fn families_of(&self, file: &str) -> Vec<String> {
        let direct = self.names_by_file.get(file).cloned().unwrap_or_default();
        let mut out = direct.clone();
        for (alias, target) in &self.alias_targets {
            if direct.iter().any(|name| name == target) && !out.contains(alias) {
                out.push(alias.clone());
            }
        }
        out
    }

    /// 账本里登记过的全部文件（判重的种子：fork 白名单已经按字节注册过的那批
    /// 系统字体，名字从这份账本补齐 —— fontdb 那边每张脸只报第一个家族名）。
    pub fn files(&self) -> impl Iterator<Item = &String> {
        self.names_by_file.keys()
    }
}

/// 尽力解析一份系统字体账本。坏 XML / 空串都回空账本加一行日志。
pub fn parse_font_config(xml: &str) -> SystemFontConfig {
    let mut config = SystemFontConfig::default();
    let doc = match roxmltree::Document::parse(xml) {
        Ok(doc) => doc,
        Err(err) => {
            tracing::info!("system_xml: 系统字体账本解析不了，扫描退回纯 name 表口径：{err}");
            return config;
        }
    };
    for node in doc.descendants() {
        match node.tag_name().name() {
            "family" => {
                let family = node.attribute("name").unwrap_or_default();
                let mut extras: Vec<String> = Vec::new();
                // 老格式的 <nameset><name>：family 内的 name 元素也算这个家族的叫法。
                for nameset in node.children().filter(|n| n.tag_name().name() == "nameset") {
                    for name in nameset
                        .children()
                        .filter(|n| n.tag_name().name() == "name")
                    {
                        if let Some(text) = name.text().map(str::trim).filter(|s| !s.is_empty()) {
                            extras.push(text.to_owned());
                        }
                    }
                }
                for font in node.children().filter(|n| n.tag_name().name() == "font") {
                    // `<font>` 的首段文本就是文件名；后面跟着的 `<axis>` 等子元素不影响。
                    let Some(file) = font.text().map(str::trim).filter(|s| !s.is_empty()) else {
                        continue;
                    };
                    let entry = config.names_by_file.entry(file.to_owned()).or_default();
                    for name in std::iter::once(family).chain(extras.iter().map(String::as_str)) {
                        if !name.is_empty() && !entry.iter().any(|known| known == name) {
                            entry.push(name.to_owned());
                        }
                    }
                }
            }
            "alias" => {
                if let (Some(name), Some(to)) = (node.attribute("name"), node.attribute("to"))
                    && !name.is_empty()
                    && !to.is_empty()
                {
                    config.alias_targets.insert(name.to_owned(), to.to_owned());
                }
            }
            _ => {}
        }
    }
    config
}

#[cfg(test)]
mod tests {
    use super::{SystemFontConfig, parse_font_config};

    /// 样例取自 AOSP `data/fonts/fonts.xml` 的三种真实形态：带名的家族、lang-only
    /// 的无名家族、familyset 层的别名。
    const SAMPLE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<familyset version="23">
    <family name="sans-serif">
        <font weight="400" style="normal">Roboto-Regular.ttf</font>
        <font weight="700" style="normal">Roboto-Regular.ttf</font>
    </family>
    <alias name="sans-serif-light" to="sans-serif" weight="300" />
    <alias name="arial" to="sans-serif" />
    <family lang="zh-Hans">
        <font weight="400" style="normal" index="2" postScriptName="NotoSansCJKjp-Regular">
            NotoSansCJK-Regular.ttc
            <axis tag="wght" stylevalue="400"/>
        </font>
    </family>
</familyset>"#;

    /// 带名家族：文件收下家族名与指向它的全部别名；变体轴不污染文件名。
    #[test]
    fn a_named_family_collects_its_files_and_aliases() {
        let config = parse_font_config(SAMPLE);
        // 别名来自 HashMap 迭代，只比集合不比顺序。
        let mut families = config.families_of("Roboto-Regular.ttf");
        families.sort();
        assert_eq!(
            families,
            vec![
                "arial".to_string(),
                "sans-serif".to_string(),
                "sans-serif-light".to_string(),
            ]
        );
        assert_eq!(
            config.alias_targets.get("sans-serif-light").unwrap(),
            "sans-serif"
        );
    }

    /// lang-only 的无名家族：文件参与判重但没有家族名；`<axis>` 子元素不把文件名
    /// 吃成带轴的长串。
    #[test]
    fn a_nameless_family_records_the_file_without_names() {
        let config = parse_font_config(SAMPLE);
        let names = config.names_by_file.get("NotoSansCJK-Regular.ttc").unwrap();
        assert!(names.is_empty(), "无名家族不给名：{names:?}");
        assert!(config.families_of("NotoSansCJK-Regular.ttc").is_empty());
        assert!(config.files().any(|file| file == "NotoSansCJK-Regular.ttc"));
    }

    /// 老格式 `<nameset><name>`：family 内的 name 元素也算家族名。
    #[test]
    fn legacy_nameset_names_are_collected() {
        let config = parse_font_config(
            r#"<familyset>
                <family name="sans-serif">
                    <nameset><name lang="zh-CN">思源黑体</name></nameset>
                    <font weight="400" style="normal">Roboto-Regular.ttf</font>
                </family>
            </familyset>"#,
        );
        assert_eq!(
            config.families_of("Roboto-Regular.ttf"),
            vec!["sans-serif".to_string(), "思源黑体".to_string()]
        );
    }

    /// 尽力解析：坏 XML 回空账本 —— 扫描退回纯 name 表口径。
    #[test]
    fn malformed_xml_yields_an_empty_config() {
        let config = parse_font_config("<familyset><family>");
        assert_eq!(config, SystemFontConfig::default());
        assert_eq!(parse_font_config("").families_of("anything.ttf"), Vec::<String>::new());
    }
}
