//! 翻页决策的薄壳。实现在 [`folio_core::reader::turn`](folio_core::reader::turn)
//! （§3.6 下沉：章界兜底、连续滚动的步长与夹取在 core 离线可测）；
//! app 里 `model::turn::TurnIntent` 等旧路径照旧可用。

pub use folio_core::reader::turn::{PagingFacts, TurnIntent, TurnPlan};
