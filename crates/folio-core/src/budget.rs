//! 内存预算与降级顺序（`plans/基础设施.md` §5.3）。
//!
//! 三条缓存各有自己的封顶（章的解析产物、shaping、图片纹理），但**没有总上限、也没有
//! 降级顺序** —— 本模块给这两样一个说法。桌面端会一直不回答「内存不够怎么办」直到被系统
//! 杀掉；移动端则有内存压力回调，是唯一一个会强制你回答这件事的地方。
//!
//! **现状（诚实说明）**：这里只落**策略** —— 总上限怎么算、压力来了按什么顺序丢多少。
//! 真要执行，得各缓存先能报「我占了多少字节」，而它们目前报不出来：排版那层的块缓存按
//! **块数**封顶、图片在 gpui 的纹理 atlas 里、章源文本缓存是个没有账的 `HashMap`。所以
//! 这份策略**还没接到执行端**，接法是给每类缓存加一个 `bytes()` 与一个 `evict(fraction)`，
//! 再按 [`EvictStep::order`] 依次调用。

/// 物理内存里划给缓存的比例（§六待决 #4：按比例 + 绝对上限，两条取小）。
const PHYSICAL_RATIO: f64 = 0.1;

/// 绝对上限：再大的机器也不让缓存超过它 —— 防的是「物理内存报得比真实可用大一截」。
const ABSOLUTE_CAP: u64 = 256 * 1024 * 1024;

/// 物理内存拿不到时（桌面不查、权限不给）的兜底预算。
const FALLBACK: u64 = 64 * 1024 * 1024;

/// 缓存总预算（字节）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryBudget {
    /// 三条缓存加起来的上限。
    pub total: u64,
}

impl MemoryBudget {
    /// 按设备的物理内存算一份预算。
    ///
    /// `physical_bytes` 由平台层给（Android 从 `ActivityManager` 读、桌面可给 `None`）：
    /// `ratio × 物理内存` 与 [`ABSOLUTE_CAP`] **取小**；给不出物理内存就用 [`FALLBACK`]。
    pub fn for_device(physical_bytes: Option<u64>) -> Self {
        let by_ratio = physical_bytes
            .map(|bytes| (bytes as f64 * PHYSICAL_RATIO) as u64)
            .unwrap_or(FALLBACK);
        Self {
            // 夹在 [1, ABSOLUTE_CAP]：至少 1 字节（预算不许是 0，否则任何一次「按预算裁」
            // 都会把缓存清空），至多绝对上限。
            total: by_ratio.clamp(1, ABSOLUTE_CAP),
        }
    }

    /// 给某一类缓存分多少字节（§5.3：三条缓存各有封顶，加起来不超过 [`Self::total`]）。
    ///
    /// 比例是拍的：图片纹理最占地方（50%）、远章节的文本次之（30%）、shaping 最小（20%）。
    /// [`EvictStep::CurrentPage`] 分到 0 —— **当前页不设上限**，它是「最后才动」的那一份，
    /// 要腾地方从上面三类腾，不动它。
    pub fn share(&self, step: EvictStep) -> u64 {
        let (num, den): (u64, u64) = match step {
            EvictStep::Textures => (1, 2),
            EvictStep::DistantChapters => (3, 10),
            EvictStep::Shaping => (1, 5),
            EvictStep::CurrentPage => (0, 1),
        };
        self.total * num / den
    }
}

/// 内存压力的档位。Android 的 `onTrimMemory` 粗粒度映射过来；桌面端不给，就一直是 `Normal`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pressure {
    /// 没压力：什么都不用丢。
    #[default]
    Normal,
    /// 中等：丢掉一部分可再生的东西。
    Moderate,
    /// 危急：丢得更狠，但**当前页不动**（动了就是掉帧）。
    Critical,
}

impl Pressure {
    /// 这一档该丢掉「当前占用」的百分之多少。
    pub fn evict_fraction(self) -> f64 {
        match self {
            Self::Normal => 0.0,
            Self::Moderate => 0.25,
            Self::Critical => 0.5,
        }
    }
}

/// 降级顺序里的一步：压力来了**先丢哪个**。
///
/// 顺序照 §5.3：先丢纹理、再丢 shaping、再丢远的章节，**最后才动当前页**。
/// 每一步都是**可再生产物** —— 丢了下次用得上时重新算/重新取，只是慢一点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvictStep {
    /// 图片纹理（gpui 的 atlas / [`crate::format::shrink_image`] 那一层）。
    Textures,
    /// shaping 结果（字体度量缓存）。
    Shaping,
    /// 远章节的解析产物（离当前章远的那些章源文本）。
    DistantChapters,
    /// 当前页 —— **最后才动**，动了就是掉帧。
    CurrentPage,
}

impl EvictStep {
    /// 从先到后的完整顺序。
    pub fn order() -> [EvictStep; 4] {
        [
            EvictStep::Textures,
            EvictStep::Shaping,
            EvictStep::DistantChapters,
            EvictStep::CurrentPage,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::{ABSOLUTE_CAP, EvictStep, FALLBACK, MemoryBudget, Pressure};

    /// 预算 = min(比例 × 物理内存, 绝对上限)；小内存机器走比例那一条。
    #[test]
    fn the_budget_is_the_smaller_of_ratio_and_cap() {
        // 1 GiB 物理内存 → 102 MiB（比例那条更小）。
        assert_eq!(
            MemoryBudget::for_device(Some(1024 * 1024 * 1024)).total,
            1024 * 1024 * 1024 / 10
        );
        // 16 GiB 物理内存 → 撞绝对上限。
        assert_eq!(
            MemoryBudget::for_device(Some(16 * 1024 * 1024 * 1024)).total,
            ABSOLUTE_CAP
        );
        // 物理内存报得极小也别给 0。
        assert!(MemoryBudget::for_device(Some(1)).total >= 1);
    }

    /// 拿不到物理内存就退回兜底值。
    #[test]
    fn a_missing_physical_size_falls_back() {
        assert_eq!(MemoryBudget::for_device(None).total, FALLBACK);
    }

    /// 压力越大丢得越多，且 `Normal` 一点不丢。
    #[test]
    fn pressure_maps_to_a_monotone_fraction() {
        assert_eq!(Pressure::Normal.evict_fraction(), 0.0);
        assert!(Pressure::Moderate.evict_fraction() > Pressure::Normal.evict_fraction());
        assert!(Pressure::Critical.evict_fraction() > Pressure::Moderate.evict_fraction());
        assert!(
            Pressure::Critical.evict_fraction() < 1.0,
            "当前页那一档还留着"
        );
        assert_eq!(Pressure::default(), Pressure::Normal);
    }

    /// 分摊：三类封顶加起来正好是总预算，当前页不占（它是「最后才动」的那一份）。
    #[test]
    fn the_shares_add_up_and_spare_the_current_page() {
        let budget = MemoryBudget::for_device(Some(4 * 1024 * 1024 * 1024)); // 撞上限 → 256 MiB
        let sum = budget.share(EvictStep::Textures)
            + budget.share(EvictStep::DistantChapters)
            + budget.share(EvictStep::Shaping);
        assert!(
            sum <= budget.total && budget.total - sum <= 3,
            "三类封顶该分完（整数取整留几字节余数没关系）：sum={sum} total={}",
            budget.total
        );
        assert_eq!(budget.share(EvictStep::CurrentPage), 0, "当前页不设上限");
        assert!(budget.share(EvictStep::Textures) > budget.share(EvictStep::DistantChapters));
        assert!(budget.share(EvictStep::DistantChapters) > budget.share(EvictStep::Shaping));
    }

    /// 降级顺序钉死：纹理 → shaping → 远章节 → 当前页。
    #[test]
    fn the_eviction_order_is_fixed() {
        assert_eq!(
            EvictStep::order(),
            [
                EvictStep::Textures,
                EvictStep::Shaping,
                EvictStep::DistantChapters,
                EvictStep::CurrentPage,
            ]
        );
    }
}
