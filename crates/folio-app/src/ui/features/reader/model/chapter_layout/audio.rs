//! 音频行的命中一问（块盒几何）。
//!
//! 单开一间是因为 `chapter_layout.rs` 已经贴着行数基线；判定与 `src` 折算的理由跟着
//! 方法走。

use super::Document;

impl Document {
    /// 这一击若落在一条音频行上，返回折成容器路径的资源 href。
    ///
    /// 音频行是空文本的块盒（排版层 `audio_rows` 短表里那条），命中按块盒几何判归属，
    /// 不走 [`Document::hit_test`](super::Document::hit_test) —— 那一条只认行盒文本，
    /// 音频块没有行，点在 48px 空档会被「夹到最近的行」落到相邻文字上。坐标与
    /// `hit_test` 同一空间（`to_layout` 折好的章内坐标；竖排下短表已投影成物理坐标）。
    /// 短表极短（一章通常 0~几条），线性扫就是正确的那一刀。
    pub fn audio_href_at(&self, x: f32, y: f32) -> Option<String> {
        let row = self.layout().audio_rows.iter().find(|row| {
            x >= row.x && x <= row.x + row.width && y >= row.y && y <= row.y + row.height
        })?;
        Some(folio_core::content::format::resolve_href(
            self.images.base_dir(),
            &row.src,
        ))
    }
}
