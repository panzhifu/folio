//! 架面那一片格子**怎么排**：一行几本、格间距、封面高、整片滚动。
//!
//! 一格长什么样在 `components/book_card`；这一份只回答「这一屏能放几本、每本多大」。
//! 全是纯函数/常数 —— 列数算错不会报错，只会让架面在某个窗口宽上突然变成一格一本，
//! 所以它们能单独被断言（见 `columns_for` 的那三条）。

use super::super::components::book_card::{COVER_RATIO, Card, GroupCard, TILE_SLOTS};
use super::super::components::book_menu;
use super::scan::ScanPhase;
use crate::ui::components::{
    BottomSheetEvent, BottomSheetState, SearchBox, SearchBoxEvent, empty_state, muted_label,
};
use crate::ui::foundation::error::{Context as _, report};
use crate::ui::services::config::ConfigHolder;
use crate::ui::services::l10n::{tf, tr};
use crate::ui::services::library::{delete_book, pick_dir, suggested_dir, target_dir};
use crate::ui::services::services;
use crate::ui::shell::navigation::{RAIL_WIDTH, SIDEBAR_WIDTH, SizeClass};
use folio_core::net::remote::Transport;
use folio_core::store::Collection;
use folio_core::{
    Cancel, ImportOutcome, ImportReport, Imported, PathsSource, Store, TaskEvent, UrlSource,
    submit_import,
};
use gpui_kit::base::NavStackState;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::InputState;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{Selectable, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, Image, InteractiveElement,
    IntoElement, ParentElement, PathPromptOptions, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Task, TestSupportExt as _,
    UniformListScrollHandle, WeakEntity, Window, div, px, uniform_list,
};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

pub(in crate::ui::features::shelf) const COLUMNS: u16 = 3;

/// 网格间距，与 `grid` 的 `gap_2` 同一档（默认 rem 下 8 逻辑像素）。
pub(in crate::ui::features::shelf) const GAP: f32 = 8.0;

/// 桌面上一格封面的最小宽度（逻辑像素）。
///
/// 比手机的 112 宽：鼠标桌面上书名要一眼读完，而不是靠点进去看，而一格多宽，
/// 标题那一行就能塞下几个字。再往下加一列会开始把长书名截成看不懂的半句。
pub(in crate::ui::features::shelf) const MIN_CARD: f32 = 168.0;

/// 网格与搜索行两侧的留白（逻辑像素）。和顶部那条章导航的 `px_3` 不同一档，
/// 因为这里是整片卡片的边，视觉上要比一条文字栏松。
pub(in crate::ui::features::shelf) const GUTTER: f32 = 12.0;

/// 一横排的量：几列，以及一格封面该多高。
///
/// 封面高必须由格宽**在这里算出来**，不能留给 `aspect_ratio` 在排版时按宽度推：
/// `uniform_list` 量行高时给的是 **min-content 宽度**（`measure_item(None, ..)`），
/// 而书名那一行是 `whitespace_nowrap` —— 它的 min-content 宽就是整条不折行的标题宽。
/// 于是量出来的那一格有 300 多逻辑像素宽，封面按 2:3 推出 500 多的高，行高比真实的
/// 两还多，第二排直接被推到屏幕外（真机 1080×2400 实测：量出 544，实际 236）。
/// 写死高度让行高与测量时给的宽度无关，这个坑就填上了。
#[derive(Clone, Copy)]
pub(in crate::ui::features::shelf) struct GridSpec {
    pub(in crate::ui::features::shelf) columns: u16,
    pub(in crate::ui::features::shelf) cover_height: f32,
}

/// 网格真正能用到的宽度：扣掉导航栏与两侧留白。
///
/// `columns_for` 与 [`grid_spec`] 都从这一个式子取数：两处各算一份的话，改一侧忘另一侧
/// 会表现为「列数对了但每格宽得莫名其妙」。
pub(in crate::ui::features::shelf) fn available_width(width: f32, size: SizeClass) -> f32 {
    let nav = match size {
        SizeClass::Compact => 0.0,
        SizeClass::Medium => RAIL_WIDTH,
        SizeClass::Expanded => SIDEBAR_WIDTH,
    };
    (width - nav - GUTTER * 2.0).max(0.0)
}

/// 一横排该排成什么样：列数按 [`columns_for`]，封面高按那个列数均出来的格宽。
pub(in crate::ui::features::shelf) fn grid_spec(width: f32, size: SizeClass) -> GridSpec {
    let columns = columns_for(width, size);
    // n 格之间只有 n−1 条间距，所以只扣一条 `GAP * (n−1)`。
    let card = (available_width(width, size) - GAP * f32::from(columns - 1)) / f32::from(columns);
    GridSpec {
        columns,
        cover_height: card / COVER_RATIO,
    }
}

/// 一行放几本：手机固定 [`COLUMNS`]，桌面与平板按**内容区实际宽度**排到每格
/// 不低于 [`MIN_CARD`]。
///
/// 3 列写死在桌面上是不成立的：1600 宽的窗口会给出三格 440 像素的封面，
/// 一屏只看得见两本书 —— 而「我的书都在哪儿」正是架面要回答的问题。
///
/// 导航栏的宽度从 `shell::navigation` 取：那两个常数是阅读页预留版心用的同一份，
/// 在这里再写一遍数字的话，改一侧忘另一侧只会表现为「书架的格子宽得莫名其妙」。
///
/// `pub` 只为测试能钉住这三档的列数 —— 列数算错不会报错，只会让架面在某个窗口宽上
/// 突然变成一格一本。
pub fn columns_for(width: f32, size: SizeClass) -> u16 {
    if size.is_compact() {
        return COLUMNS;
    }
    // 第 n 格要占 n 个格宽加 n−1 条间距，所以分子补回一条 GAP。
    let fitted = ((available_width(width, size) + GAP) / (MIN_CARD + GAP)).floor() as u16;
    fitted.max(1)
}
