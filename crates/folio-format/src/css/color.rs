//! 一格 RGBA 颜色。

/// 一格颜色：RGBA，各 0–255。
///
/// 字段摊开而不是数组：表里的色值一眼读得出通道。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba8 {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba8 {
    /// 不透明的一格。
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// 感知亮度（Rec. 709 系数）压成的灰，alpha 原样留着。
    ///
    /// 与壳那边 `theme.rs::desaturate_hex`、`style.rs::desaturate` 同一把尺：水墨档
    /// 只拿掉色相，不把深底变成浅底、也不让浅字在灰底上失掉对比。
    pub fn luma(self) -> Self {
        let gray =
            (0.2126 * f32::from(self.r) + 0.7152 * f32::from(self.g) + 0.0722 * f32::from(self.b))
                .round() as u8;
        Self {
            r: gray,
            g: gray,
            b: gray,
            a: self.a,
        }
    }
}
