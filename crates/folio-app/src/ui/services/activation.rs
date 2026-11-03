//! 会员兑换的应用侧：一份状态、一个入口、一条「兑完了要写到期日并重绘」的链。
//!
//! 形状抄 [`update`]：状态挂 `Entity` 而不是 plain `Global`（`cx.global_mut` 不通知视图，
//! 异步回来那一刻必须让那一格自己变字）、出网跑在 `background_spawn` 上、翻句子等到
//! 拿到 `&App` 的那一刻。这一层不发元素树，也不知道谁在看它。
//!
//! ## 为什么判不了「付过钱」要留一条本地形状检查
//!
//! 付没付过钱只能由 `folio-redeem` 那支函数向爱发电查（api token 不能进客户端）。但
//! **形状**能在本地判：一句「订单号不像是爱发电的」不该花一次网络往返，也不该把读者粘进来
//! 的一整段订单描述送到服务端的日志里。所以 [`redeem_now`] 先本地判一次，判过了才出门。
//!
//! ## 到期日与那句状态在同一次更新里写完
//!
//! 顺序是有意的：先写 `Membership::until_day`，再换 [`Status`]。反过来的话，屏幕上会出现
//! 一帧「已到账」而上面那句到期日还没动 —— 那正是这一屏唯一能自证成交的地方。

use folio_core::infra::time;
use folio_core::net::redeem::{self, Outcome};
use gpui_kit::{App, AppContext as _, Context, Entity, Global, SharedString};

use super::config::ConfigHolder;
use super::sync::ensure_device_id;
use super::update::transport;

/// 兑换那一格当前该念哪一句。
///
/// `Failed` 里装的是 **core 那个错误**而不是已经翻好的句子：文案属于界面，
/// 「为什么兑不成」的判据属于协议层，两边各自演化时不该互相牵住。（同 [`update::Status`]）
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Status {
    /// 还没兑过（进程起来就是这个）。
    #[default]
    Idle,
    /// 请求在飞。按钮这一档同时禁用，防止双击把同一单交两次。
    Running,
    /// 兑成了。[`Outcome::Replay`] 也算这一档：权益在第一次就给过了。
    Done(Outcome),
    Failed(redeem::Error),
}

/// 兑换这一件事的全部状态：结果 + 那一格还没提交的草稿。
///
/// 草稿不住配置：它是「这一趟没填完」的一次性输入，落盘只会让读者的 config.json 里
/// 多出一个别人看不懂的订单号。
#[derive(Debug, Default)]
pub struct ActivationState {
    status: Status,
    draft: SharedString,
}

impl ActivationState {
    pub fn status(&self) -> &Status {
        &self.status
    }

    pub fn draft(&self) -> &SharedString {
        &self.draft
    }

    pub fn busy(&self) -> bool {
        matches!(self.status, Status::Running)
    }

    fn settle(&mut self, outcome: Result<Outcome, redeem::Error>, cx: &mut Context<Self>) {
        match outcome {
            Ok(got) => {
                if matches!(got, Outcome::Granted(_)) {
                    tracing::info!("会员兑换到账");
                }
                self.status = Status::Done(got);
            }
            Err(error) => {
                tracing::warn!("会员兑换失败：{error}");
                self.status = Status::Failed(error);
            }
        }
        cx.notify();
    }
}

pub struct ActivationHolder(pub Entity<ActivationState>);

impl Global for ActivationHolder {}

pub fn handle(cx: &App) -> Option<Entity<ActivationState>> {
    cx.try_global::<ActivationHolder>()
        .map(|holder| holder.0.clone())
}

/// 取句柄，没有就建一个（与 [`update::entity`] 同一条理由：设置页构造时手里就是
/// `&mut App`，不必等外壳先装）。
pub fn entity(cx: &mut App) -> Entity<ActivationState> {
    if let Some(entity) = handle(cx) {
        return entity;
    }
    let entity = cx.new(|_| ActivationState::default());
    cx.set_global(ActivationHolder(entity.clone()));
    entity
}

/// 订单号那一格的读与写。两端的渲染器都走设置页那份 `RowKind::Text` 规格，
/// 所以这两个函数就是它与这份状态之间唯一的接缝。
pub fn draft(cx: &App) -> SharedString {
    handle(cx)
        .map(|entity| entity.read(cx).draft().clone())
        .unwrap_or_default()
}

pub fn set_draft(value: SharedString, cx: &mut App) {
    let entity = entity(cx);
    entity.update(cx, |state, cx| {
        if state.draft != value {
            state.draft = value;
            cx.notify();
        }
    });
}

/// 「兑换」。已经在飞时什么都不做（按钮那边同时禁用）。
///
/// 设备 id 是这一单的归属对象：服务端拿它绑「这单归哪台安装」，所以没有它就不能出门 ——
/// 宁可当场报错，也不要发一次「匿名」的兑换，那会在台账上留一个谁也认领不上的洞。
pub fn redeem_now(cx: &mut App) {
    let state = entity(cx);
    if state.read(cx).busy() {
        return;
    }
    let order = state.read(cx).draft().trim().to_string();
    if !redeem::valid_order(&order) {
        state.update(cx, |state, cx| {
            state.status = Status::Failed(redeem::Error::BadOrder);
            cx.notify();
        });
        return;
    }
    let Some(device) = ensure_device_id(cx) else {
        state.update(cx, |state, cx| {
            state.status = Status::Failed(redeem::Error::Malformed("设备 id 不成形状".to_owned()));
            cx.notify();
        });
        return;
    };
    // 「今天」在出发这一刻取一次，回来时用它算到期日：一趟兑换走上几秒，跨日界线的话
    // 回来再取会让「买到的第一天」少一天或多一天。
    let today = time::local_day(time::now(), crate::ui::host::local_offset_secs());

    state.update(cx, |state, cx| {
        state.status = Status::Running;
        cx.notify();
    });

    let transport = transport();
    let work =
        cx.background_spawn(async move { redeem::redeem(transport.as_ref(), &order, &device) });
    let weak = state.downgrade();
    cx.spawn(async move |cx| {
        let outcome = work.await;
        let days = match &outcome {
            Ok(Outcome::Granted(days)) => Some(*days),
            _ => None,
        };
        // 在这个 `cx.update` 里才第一次拿到 `&mut App`：写配置与翻句子都只有 App 能做，
        // `AsyncApp` 这一层做不了（同 `services/update.rs` 里那条注释）。
        cx.update(|cx| {
            if let Some(days) = days {
                ConfigHolder::edit_membership(cx, |membership| {
                    membership.until_day = membership.extended_on(days, today)
                });
            }
            let _ = weak.update(cx, |state, cx| state.settle(outcome, cx));
        });
    })
    .detach();
}

#[cfg(test)]
mod tests {
    //! 这一层只钉两件它自己说了算的事：**草稿的读写走的是同一份实体**、以及
    //! **形状不对时不出门**。
    //!
    //! 「一段 JSON 意味着什么」在 `folio_core::net::redeem` 那边钉；「到期日接在哪一天」
    //! 在 `folio_core::infra::config` 那边钉。这里不重复测它们，也不测真发请求那一条 ——
    //! 兑换会花读者在爱发电的那一单，测试里不该有任何一次真兑换。

    use super::{ActivationState, Status};
    use folio_core::net::redeem::{Error, Outcome};
    use gpui_kit::{App, SharedString, TestAppContext};

    /// 草稿进得去、出得来，而且**换值才通知**：每次按键都重建一屏的话，输入框会跟着抖。
    #[gpui_kit::test]
    fn the_draft_round_trips_through_the_entity(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx: &mut App| {
            let entity = super::entity(cx);
            assert_eq!(super::draft(cx), SharedString::default(), "没装过就是空的");

            super::set_draft(SharedString::from("202106232138371083454010626"), cx);
            assert_eq!(super::draft(cx).as_str(), "202106232138371083454010626");
            assert_eq!(
                entity.read(cx).draft().as_str(),
                "202106232138371083454010626"
            );
        });
    }

    /// 没装实体时 `draft` 回空而不是 panic：设置页第一帧就在读它，那时外壳还没装。
    #[gpui_kit::test]
    fn reading_the_draft_before_the_entity_exists_is_empty_not_fatal(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx: &mut App| {
            assert!(super::handle(cx).is_none());
            assert_eq!(super::draft(cx), SharedString::default());
        });
    }

    #[gpui_kit::test]
    fn a_misshapen_order_never_leaves_the_device(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx: &mut App| {
            crate::ui::services::config::ConfigHolder::install(folio_core::Config::default(), cx);
            let entity = super::entity(cx);
            super::set_draft(SharedString::from("还没付钱的那一串"), cx);
            super::redeem_now(cx);
            // 本地就判掉了，状态直接落到 BadOrder —— 这一趟不该有任何网络请求在飞。
            assert!(
                matches!(entity.read(cx).status(), Status::Failed(Error::BadOrder)),
                "状态落成了 {:?}",
                entity.read(cx).status()
            );
            assert!(!entity.read(cx).busy());
        });
    }

    /// `busy` 只认在飞那一档：重交（`Replay`）与到账都是 settled，按钮该恢复可点。
    #[test]
    fn only_an_in_flight_redeem_counts_as_busy() {
        for (status, busy) in [
            (Status::Idle, false),
            (Status::Running, true),
            (Status::Done(Outcome::Granted(365)), false),
            (Status::Done(Outcome::Replay), false),
            (Status::Failed(Error::NotFound), false),
        ] {
            assert_eq!(
                ActivationState {
                    status,
                    ..Default::default()
                }
                .busy(),
                busy
            );
        }
    }
}
