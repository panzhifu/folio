//! 导航与翻页共用的动效策略；配置归服务，元素与状态归各自视图。

use std::time::Duration;

use gpui_kit::App;
use gpui_kit::base::motion::{Easing, Transition};

use crate::ui::foundation::theme;
use crate::ui::services::config::ConfigHolder;

pub(crate) const NAV_TRANSITION: Duration = Duration::from_millis(280);

pub(crate) fn nav_transition(cx: &App) -> Transition {
    build_transition(nav_duration_for(cx.reduce_motion(), theme::calm_motion()))
}

pub(crate) fn nav_duration_for(reduce_motion: bool, calm: bool) -> Duration {
    if reduce_motion || calm {
        Duration::ZERO
    } else {
        NAV_TRANSITION
    }
}

fn build_transition(over: Duration) -> Transition {
    if over.is_zero() {
        Transition::new(over)
    } else {
        Transition::new(over).easing(Easing::EaseInOut)
    }
}

pub(crate) fn turn_duration(cx: &App) -> Duration {
    turn_duration_for(
        ConfigHolder::appearance(cx).turn_duration,
        theme::calm_motion(),
    )
}

pub(crate) fn turn_duration_for(configured: f32, calm: bool) -> Duration {
    if calm {
        Duration::ZERO
    } else {
        Duration::from_millis(configured.round() as u64)
    }
}

/// 起步时冻结时长，避免拖动设置时每帧重瞄同一趟动画。
pub(crate) fn turn_transition(over: Duration, cx: &App) -> Transition {
    build_transition(turn_over_for(
        over,
        cx.reduce_motion(),
        theme::calm_motion(),
    ))
}

pub(crate) fn turn_over_for(over: Duration, reduce_motion: bool, calm: bool) -> Duration {
    if reduce_motion || calm {
        Duration::ZERO
    } else {
        over
    }
}
