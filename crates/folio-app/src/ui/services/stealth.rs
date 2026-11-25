//! 低干扰阅读的应用服务。
//!
//! 状态机在这里，窗口调用经 `host` 的窄边界；视图只读快照。这样切换模式不会重建
//! `ReaderSession`，也不会让窗口事件散落在 render 里。

use folio_core::{Stealth, StealthMode};
use gpui_kit::{AnyWindowHandle, App, AppContext as _, Entity, Global, Subscription};

use crate::ui::host::{self, StealthCapabilities, StealthError};
use crate::ui::services::config::ConfigHolder;

/// 当前形态。隐藏时记住恢复后该回到哪一档。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StealthPhase {
    #[default]
    Off,
    Visible(StealthMode),
    Hidden {
        restore: StealthMode,
    },
}

/// 读者可见的运行时状态。热键内容不进日志，错误只存固定文案。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StealthState {
    pub phase: StealthPhase,
    pub capabilities: StealthCapabilities,
    pub last_error: Option<StealthError>,
}

/// 触发状态变化的事件。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StealthEvent {
    Toggle,
    BossHide,
    Restore,
    SetMode(StealthMode),
}

/// 纯转移：无窗口、无上下文，便于直接钉住状态机。
pub fn transition(state: StealthState, event: StealthEvent) -> StealthState {
    let mode = match state.phase {
        StealthPhase::Off => StealthMode::Off,
        StealthPhase::Visible(mode) => mode,
        StealthPhase::Hidden { restore } => restore,
    };
    let phase = match event {
        StealthEvent::Toggle => match state.phase {
            StealthPhase::Off | StealthPhase::Hidden { .. } => {
                StealthPhase::Visible(StealthMode::Quiet)
            }
            StealthPhase::Visible(StealthMode::Off) => StealthPhase::Visible(StealthMode::Quiet),
            StealthPhase::Visible(StealthMode::Quiet) => StealthPhase::Off,
        },
        StealthEvent::BossHide => match state.phase {
            StealthPhase::Hidden { .. } => state.phase,
            StealthPhase::Off => StealthPhase::Hidden {
                restore: StealthMode::Off,
            },
            StealthPhase::Visible(_) => StealthPhase::Hidden { restore: mode },
        },
        StealthEvent::Restore => match state.phase {
            StealthPhase::Hidden { restore } => StealthPhase::Visible(restore),
            _ => state.phase,
        },
        StealthEvent::SetMode(next) => match state.phase {
            StealthPhase::Hidden { .. } => StealthPhase::Hidden { restore: next },
            _ if next == StealthMode::Off => StealthPhase::Off,
            _ => StealthPhase::Visible(next),
        },
    };
    StealthState { phase, ..state }
}

/// 进程级句柄：shell 与阅读页各持一份 `Entity`，不直接拿 Global 里的实体。
pub struct StealthService {
    pub entity: Entity<StealthState>,
    pub handle: AnyWindowHandle,
    /// 配置观察者必须跟着进程活着；放在全局里，视图不必再各存一份订阅。
    _config_watch: Subscription,
}

impl Global for StealthService {}

/// 当前只读快照。
pub fn snapshot(cx: &App) -> Option<StealthState> {
    cx.try_global::<StealthService>()
        .map(|service| *service.entity.read(cx))
}

fn is_quiet_phase(phase: StealthPhase) -> bool {
    matches!(
        phase,
        StealthPhase::Visible(StealthMode::Quiet)
            | StealthPhase::Hidden {
                restore: StealthMode::Quiet
            }
    )
}

/// 当前是否在低干扰形态。隐藏时也要记住纸面曾经透明，恢复窗口时不会闪回不透明。
pub fn is_quiet(cx: &App) -> bool {
    snapshot(cx).is_some_and(|state| is_quiet_phase(state.phase))
}

/// 低干扰时正文纸与窗口底色保留的 alpha。不是把字也调淡：只让纸让出窗口背板。
pub fn content_opacity(cx: &App) -> f32 {
    if is_quiet(cx) {
        ConfigHolder::stealth(cx).opacity
    } else {
        1.0
    }
}

/// 把状态落到窗口。失败只记原因，不打断阅读。
fn apply_window(handle: AnyWindowHandle, state: &mut StealthState, cx: &mut App) {
    let transparent = is_quiet_phase(state.phase);
    let result = cx.update_window(handle, |_, window, cx| {
        if !state.capabilities.transparent_background {
            return Ok(());
        }
        host::stealth_transparent(transparent, window, cx)
    });
    match result {
        Ok(Ok(())) => state.last_error = None,
        Ok(Err(err)) => state.last_error = Some(err),
        Err(_) => state.last_error = Some(StealthError::NoWindow),
    }
}

/// 从配置与平台能力建初始状态。偏好开着时直接落到对应形态。
///
/// 配置观察者也在这里挂上并放进全局：它要活到进程结束，视图不该为它再存一格。
pub fn install(handle: AnyWindowHandle, cx: &mut App) -> Entity<StealthState> {
    let prefs: Stealth = ConfigHolder::stealth(cx);
    let initial = StealthState {
        phase: if prefs.enabled && prefs.mode != StealthMode::Off {
            StealthPhase::Visible(prefs.mode)
        } else {
            StealthPhase::Off
        },
        capabilities: host::stealth_capabilities(cx),
        last_error: None,
    };
    let mut applied = initial;
    apply_window(handle, &mut applied, cx);
    let entity = cx.new(|_| applied);
    let weak = entity.downgrade();
    let watch = cx.observe_global::<ConfigHolder>(move |cx| {
        let Some(service) = weak.upgrade() else {
            return;
        };
        let prefs = ConfigHolder::stealth(cx);
        let preferred = if prefs.enabled {
            prefs.mode
        } else {
            StealthMode::Off
        };
        service.update(cx, |state, cx| {
            let before = state.phase;
            state.phase = apply_mode(before, preferred);
            if before != state.phase {
                apply_window(handle, state, cx);
                cx.notify();
            }
        });
        // 透明度也在这条观察里：形态没变、只拖了透明度时纸面仍要重画。
        cx.refresh_windows();
    });
    cx.set_global(StealthService {
        entity: entity.clone(),
        handle,
        _config_watch: watch,
    });
    entity
}

/// 系统把窗口重新带回前台时调用：只把隐藏态收回可见态，其余形态不动。
/// 最小化后从任务栏/系统手势恢复不会派发应用内动作，外壳的窗口激活观察走这里。
pub fn restore_from_activation(cx: &mut App) {
    let hidden =
        snapshot(cx).is_some_and(|state| matches!(state.phase, StealthPhase::Hidden { .. }));
    if hidden {
        dispatch_with_window(StealthEvent::Restore, cx);
    }
}

fn apply_mode(phase: StealthPhase, preferred: StealthMode) -> StealthPhase {
    match phase {
        StealthPhase::Hidden { .. } => phase,
        StealthPhase::Off if preferred == StealthMode::Off => StealthPhase::Off,
        StealthPhase::Off => StealthPhase::Visible(preferred),
        StealthPhase::Visible(_) if preferred == StealthMode::Off => StealthPhase::Off,
        StealthPhase::Visible(_) => StealthPhase::Visible(preferred),
    }
}

/// 从当前窗口的上下文分派。外壳的全局动作在窗口派发里跑，不能再借同一个窗口，
/// 所以只在服务内部用句柄更新；外壳只传语义事件。
pub fn dispatch_with_window(event: StealthEvent, cx: &mut App) {
    let (handle, entity) = {
        let Some(service) = cx.try_global::<StealthService>() else {
            return;
        };
        (service.handle, service.entity.clone())
    };
    dispatch(handle, &entity, event, cx);
}

/// 分派一个事件并把结果落窗。shell 的 action 走这里。
pub fn dispatch(
    handle: AnyWindowHandle,
    entity: &Entity<StealthState>,
    event: StealthEvent,
    cx: &mut App,
) {
    entity.update(cx, |state, cx| {
        let next = transition(*state, event);
        if next == *state {
            return;
        }
        let was_hidden = matches!(state.phase, StealthPhase::Hidden { .. });
        let now_hidden = matches!(next.phase, StealthPhase::Hidden { .. });
        *state = next;
        apply_window(handle, state, cx);
        if was_hidden != now_hidden {
            cx.update_window(handle, |_, window, cx| {
                let result = if now_hidden {
                    host::stealth_hide(window, cx)
                } else {
                    host::stealth_restore(window, cx)
                };
                if let Err(err) = result {
                    state.last_error = Some(err);
                }
            })
            .ok();
        }
        cx.notify();
        cx.refresh_windows();
    });
}

#[cfg(test)]
mod tests {
    use super::{StealthEvent, StealthPhase, StealthState, transition};
    use crate::ui::host::StealthCapabilities;
    use folio_core::StealthMode;

    fn state(phase: StealthPhase) -> StealthState {
        StealthState {
            phase,
            capabilities: StealthCapabilities::default(),
            last_error: None,
        }
    }

    #[test]
    fn toggle_cycles_off_quiet_off() {
        let off = state(StealthPhase::Off);
        let quiet = transition(off, StealthEvent::Toggle);
        assert_eq!(quiet.phase, StealthPhase::Visible(StealthMode::Quiet));
        let back = transition(quiet, StealthEvent::Toggle);
        assert_eq!(back.phase, StealthPhase::Off);
    }

    #[test]
    fn boss_hide_remembers_the_mode_and_restore_returns_to_it() {
        let hidden = transition(
            state(StealthPhase::Visible(StealthMode::Quiet)),
            StealthEvent::BossHide,
        );
        assert_eq!(
            hidden.phase,
            StealthPhase::Hidden {
                restore: StealthMode::Quiet
            }
        );
        assert_eq!(
            transition(hidden, StealthEvent::Restore).phase,
            StealthPhase::Visible(StealthMode::Quiet)
        );
    }

    #[test]
    fn hidden_only_changes_the_restore_mode() {
        let hidden = state(StealthPhase::Hidden {
            restore: StealthMode::Quiet,
        });
        let changed = transition(hidden, StealthEvent::SetMode(StealthMode::Off));
        assert_eq!(
            changed.phase,
            StealthPhase::Hidden {
                restore: StealthMode::Off
            }
        );
    }
}
