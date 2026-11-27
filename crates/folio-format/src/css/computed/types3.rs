//! Flex/Grid 的容器对齐与方向枚举，从 `computed.rs` 切出来守行数基线。

/// `flex-direction`：主轴方向（四档全收，`-reverse` 只反主轴）。它是**逻辑**方向：
/// `row` 沿行内轴，`column` 沿块轴，竖排由 [`WritingMode`] 投影成物理坐标。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FlexDirection {
    #[default]
    Row,
    RowReverse,
    Column,
    ColumnReverse,
}

impl FlexDirection {
    /// 主轴是否从终边往起边排。
    pub fn is_reverse(self) -> bool {
        matches!(self, Self::RowReverse | Self::ColumnReverse)
    }

    /// 主轴是否为横向（逻辑行内轴）。
    pub fn is_row(self) -> bool {
        matches!(self, Self::Row | Self::RowReverse)
    }
}

/// `flex-wrap`：主轴放不下时是否换行。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FlexWrap {
    #[default]
    NoWrap,
    Wrap,
}

/// `justify-self`：grid 子项在自身格子里的**行内轴**对齐。初值 `auto` 按 stretch 处理。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum JustifySelf {
    #[default]
    Auto,
    Start,
    End,
    Center,
    Stretch,
}

/// `align-items`：子项在**交叉轴**上的对齐；`baseline` 对首行字形取基线，无字块用块高。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AlignItems {
    #[default]
    Stretch,
    Start,
    End,
    Center,
    Baseline,
}

/// `justify-content`：子项在**主轴**上的分布；`space-around` 首尾半缝，`space-evenly` 首尾整缝。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum JustifyContent {
    #[default]
    Start,
    End,
    Center,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

/// `align-content`：多行 Flex 容器在**交叉轴**上的行间分布；目前只在 row + wrap 的多行场景生效。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AlignContent {
    #[default]
    Stretch,
    Start,
    End,
    Center,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}
