//! `ComputedStyle` 的派生查询：背景图出口与塑造器特性表。
//!
//! 从 `computed.rs` 切出来只因为那个文件贴着行数上限；`impl` 仍在同一 crate 内。

use super::{BackgroundLayer, ComputedStyle, FontKerning, TextRendering};

impl ComputedStyle {
    /// 这一块的背景里**真带图**的那些层，按 CSS 书写顺序（先写的在屏幕上更靠上）。
    /// 图层表里允许「有摆法、没有图」的惰性层，这里只留唯一的出口。
    pub fn background_pictures(&self) -> impl Iterator<Item = &BackgroundLayer> {
        self.background_layers
            .iter()
            .filter(|layer| layer.image.is_some())
    }

    /// 交给塑造器的 OpenType 特性表：`text-rendering` 的速度档 → `font-kerning` →
    /// `font-feature-settings`，后写覆盖先写。空表是常态（塑造器默认开 `kern`/`liga`）。
    pub fn shaping_features(&self) -> Vec<(String, u32)> {
        let mut out: Vec<(String, u32)> = Vec::new();
        let mut set = |tag: &str, value: u32| {
            if let Some(slot) = out.iter_mut().find(|(t, _)| t.eq_ignore_ascii_case(tag)) {
                slot.1 = value;
            } else {
                out.push((tag.to_string(), value));
            }
        };
        if self.text_rendering == TextRendering::OptimizeSpeed {
            set("kern", 0);
            set("liga", 0);
            set("calt", 0);
        }
        match self.font_kerning {
            FontKerning::Auto => {}
            FontKerning::Normal => set("kern", 1),
            FontKerning::None => set("kern", 0),
        }
        for (tag, value) in self.font_features.iter() {
            set(tag, *value);
        }
        out
    }
}
