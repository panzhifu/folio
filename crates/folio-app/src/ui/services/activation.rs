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

use folio_core::MembershipTier;
use folio_core::infra::time;
use folio_core::net::redeem::{self, Outcome};
use gpui_kit::{App, AppContext as _, Context, Entity, Global, SharedString};

use super::config::ConfigHolder;
use super::sync::ensure_device_id;
use super::update::transport;

/// 创作者的爱发电主页。开通（含各处会员引导卡上那一颗）把读者交给这一页；
/// 成交之后的订单号兑换流程另算。
pub const AFDIAN_PAGE: &str = "https://ifdian.net/a/nokenice";

/// 今天，本地日序号。门禁与会员屏共用的那一格「现在」，偏移由平台给
/// （日界线跟着读者走，写死 UTC 会让跨日界线那一天的读数差一格）。
fn today() -> i64 {
    time::local_day(time::now(), crate::ui::host::local_offset_secs())
}

/// 这台安装**现在**算不算高级会员。全 app 门禁共用的那一格判据：买断位或天数档
/// 任一在就算（[`folio_core::Membership::tier_on`]）。各功能入口问这一格，
/// 不自己读第二遍配置 —— 读两次就可能出现两次答案不一样的那一帧。
pub fn premium(cx: &App) -> bool {
    ConfigHolder::membership(cx).tier_on(today()) == MembershipTier::Premium
}

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

/// 解除那一趟的落点。与兑换分开一档而不是并进 [`Status`]：两趟可以在同一屏上各自进行中，
/// 混在一个枚举里就会出现「兑换在飞时解除那句被顶掉」这种读起来像 bug 的帧。
#[derive(Debug, Clone, Default, PartialEq)]
pub enum ReleaseStatus {
    /// 还没按过。
    #[default]
    Idle,
    /// 请求在飞。那颗钮同时禁用，防止双击把同一趟发两次。
    Running,
    /// 落定的结果：删掉了几行，或这台名下本来就没有。
    Done(redeem::Release),
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
    release: ReleaseStatus,
    /// 确认面板开没开。它是「这一问还没答」的一次性状态，与草稿同一档，不落盘。
    confirm_release: bool,
}

impl ActivationState {
    pub fn status(&self) -> &Status {
        &self.status
    }

    pub fn draft(&self) -> &SharedString {
        &self.draft
    }

    pub fn release_status(&self) -> &ReleaseStatus {
        &self.release
    }

    pub fn confirming_release(&self) -> bool {
        self.confirm_release
    }

    fn releasing(&self) -> bool {
        matches!(self.release, ReleaseStatus::Running)
    }

    pub fn busy(&self) -> bool {
        matches!(self.status, Status::Running)
    }

    fn settle(&mut self, outcome: Result<Outcome, redeem::Error>, cx: &mut Context<Self>) {
        match outcome {
            Ok(got) => {
                match got {
                    Outcome::Granted(_) => tracing::info!("会员兑换到账"),
                    Outcome::GrantedLifetime => tracing::info!("会员买断到账（永久）"),
                    // 一句单独的：这一趟没发新货，只是把已有那一单挪到这台。日志里能分清
                    // 「又买了一次」与「换了台设备」，出问题时是两条不同的线索。
                    Outcome::Moved(_) => tracing::info!("会员兑换由另一台认领到这台"),
                    Outcome::MovedLifetime => tracing::info!("买断单由另一台认领到这台"),
                    Outcome::Replay => {}
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
    // 把订单号一起带回来：台账那一行要有它才能说清「这台名下是哪一单」，而解除那一趟
    // 之后读者可能要拿它再兑一次。
    let work = cx.background_spawn(async move {
        let got = redeem::redeem(transport.as_ref(), &order, &device);
        got.map(|outcome| (order, outcome))
    });
    let weak = state.downgrade();
    cx.spawn(async move |cx| {
        let outcome = work.await;
        // 三条到账的路在这一层分账：天数档（`Granted` / `Moved`）拿「从今天起接几天」这一个数
        // 走 `record` —— 刷不出天数的责任在服务端那一次 `daysLeft`，不在这里再算一遍；
        // 买断档（`GrantedLifetime` / `MovedLifetime`）没有天数可接，置永久位走 `record_lifetime`。
        enum Grant {
            Days(String, i64),
            Lifetime(String),
        }
        let grant = match &outcome {
            Ok((order, Outcome::Granted(days))) | Ok((order, Outcome::Moved(days))) => {
                Some(Grant::Days(order.clone(), *days))
            }
            Ok((order, Outcome::GrantedLifetime)) | Ok((order, Outcome::MovedLifetime)) => {
                Some(Grant::Lifetime(order.clone()))
            }
            _ => None,
        };
        // 在这个 `cx.update` 里才第一次拿到 `&mut App`：写配置与翻句子都只有 App 能做，
        // `AsyncApp` 这一层做不了（同 `services/update.rs` 里那条注释）。
        cx.update(|cx| {
            match grant {
                Some(Grant::Days(order, days)) => {
                    ConfigHolder::edit_membership(cx, |membership| {
                        membership.record(&order, days, today)
                    });
                }
                Some(Grant::Lifetime(order)) => {
                    ConfigHolder::edit_membership(cx, |membership| {
                        membership.record_lifetime(&order, today)
                    });
                }
                None => {}
            }
            let _ = weak.update(cx, |state, cx| {
                state.settle(outcome.map(|(_, got)| got), cx)
            });
        });
    })
    .detach();
}

/// 把「要不要解除」这一问摆到屏上。确认面板的**开与关**归这一层，按下之后的那一趟归
/// [`release_now`] —— 与「存储」那几颗清理钮同一形状（`pages/storage.rs` 的 `ask`）。
pub fn ask_release(cx: &mut App) {
    let state = entity(cx);
    state.update(cx, |state, cx| {
        state.confirm_release = true;
        cx.notify();
    });
}

pub fn cancel_release(cx: &mut App) {
    let state = entity(cx);
    state.update(cx, |state, cx| {
        state.confirm_release = false;
        cx.notify();
    });
}

/// 「解除这台设备的激活」那颗钮此刻的状态。没装实体就是 `Idle`：设置页第一帧就在读它。
pub fn release_status(cx: &App) -> ReleaseStatus {
    handle(cx)
        .map(|entity| entity.read(cx).release_status().clone())
        .unwrap_or_default()
}

/// 确认面板开没开。
pub fn confirming(cx: &App) -> bool {
    handle(cx).is_some_and(|entity| entity.read(cx).confirming_release())
}

/// 「解除这台设备的激活」。在飞时什么都不做（那颗钮同时禁用）。
///
/// 先清本地再报结果？不是。**服务端说标成了才算**：那边没标成而这边把权益抹了，读者的时间是
/// 真没了，而那一单在服务端还绑着这台 —— 他既享受不到也换不走，只能私信作者。顺序反过来
/// （先出网、回来才清）最坏只是「按了没反应，再按一次」。
pub fn release_now(cx: &mut App) {
    let state = entity(cx);
    if state.read(cx).busy() || state.read(cx).releasing() {
        return;
    }
    let Some(device) = ensure_device_id(cx) else {
        state.update(cx, |state, cx| {
            state.confirm_release = false;
            state.release =
                ReleaseStatus::Failed(redeem::Error::Malformed("设备 id 不成形状".to_owned()));
            cx.notify();
        });
        return;
    };
    state.update(cx, |state, cx| {
        state.confirm_release = false;
        state.release = ReleaseStatus::Running;
        cx.notify();
    });
    let transport = transport();
    let work = cx.background_spawn(async move { redeem::release(transport.as_ref(), &device) });
    let weak = state.downgrade();
    cx.spawn(async move |cx| {
        let outcome = work.await;
        cx.update(|cx| {
            if outcome.is_ok() {
                ConfigHolder::edit_membership(cx, |membership| membership.clear_activation());
            }
            let _ = weak.update(cx, |state, cx| match outcome {
                Ok(got) => {
                    tracing::info!("解除这台设备的激活");
                    state.release = ReleaseStatus::Done(got);
                    cx.notify();
                }
                Err(error) => {
                    tracing::warn!("解除激活失败：{error}");
                    state.release = ReleaseStatus::Failed(error);
                    cx.notify();
                }
            });
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

    use super::{ActivationState, ReleaseStatus, Status};
    use folio_core::net::redeem::{Error, Outcome, Release};
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
            (Status::Done(Outcome::Moved(30)), false),
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

    /// 设备 id 不成形状时解除不出门。
    ///
    /// 那一趟删的是「哪几行属于这台」，认不出这台就没什么可删的；更要紧的是**测试里一次
    /// 真请求都不该发** —— 解除会删掉读者在生产台账上的行，而 `ensure_device_id` 对空 id
    /// 会就地生成一个，所以这里给的是一个形状不对的 id，本地就该判掉。
    #[gpui_kit::test]
    fn a_release_without_a_usable_device_never_leaves_the_device(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx: &mut App| {
            let mut config = folio_core::Config::default();
            config.sync.device_id = "带斜杠/和空格的 id".to_owned();
            crate::ui::services::config::ConfigHolder::install(config, cx);
            let entity = super::entity(cx);
            super::ask_release(cx);
            assert!(
                entity.read(cx).confirming_release(),
                "那颗钮按下去该把这一问问出来"
            );
            super::release_now(cx);
            assert!(
                matches!(
                    entity.read(cx).release_status(),
                    ReleaseStatus::Failed(Error::Malformed(_))
                ),
                "落成了 {:?}",
                entity.read(cx).release_status()
            );
            assert!(
                !entity.read(cx).confirming_release(),
                "按下结果那颗钮之后面板就该收 —— 留着它等于同一问问了两遍"
            );
        });
    }

    /// 在飞的那一趟不重复出门：第二次调用直接返回，状态仍是 `Running`。
    ///
    /// 这条与「兑换在飞时不出门」同一形状。解除发两次不会双花（DELETE 是幂等的），但第二
    /// 趟回来会把第一趟那句结果盖掉，而读者看到的只是「按了一下，字闪了一下」。
    #[gpui_kit::test]
    fn a_release_in_flight_is_not_issued_twice(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx: &mut App| {
            crate::ui::services::config::ConfigHolder::install(folio_core::Config::default(), cx);
            let entity = super::entity(cx);
            entity.update(cx, |state, _| state.release = ReleaseStatus::Running);
            super::release_now(cx);
            assert_eq!(
                *entity.read(cx).release_status(),
                ReleaseStatus::Running,
                "在飞时第二次调用该原地返回"
            );
            assert!(entity.read(cx).releasing());
        });
    }

    /// 取消只收面板：不发请求、不动配置、也不留一句结果。
    #[gpui_kit::test]
    fn cancelling_the_release_question_changes_nothing(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx: &mut App| {
            crate::ui::services::config::ConfigHolder::install(folio_core::Config::default(), cx);
            let entity = super::entity(cx);
            super::ask_release(cx);
            super::cancel_release(cx);
            assert!(!entity.read(cx).confirming_release());
            assert_eq!(
                *entity.read(cx).release_status(),
                ReleaseStatus::Idle,
                "取消之后不该留下一句结果占住那一格"
            );
            assert_eq!(
                folio_core::Config::default().membership,
                crate::ui::services::config::ConfigHolder::membership(cx),
                "没答的那一问不该动到会员那一节"
            );
        });
    }

    /// 兑换与解除各走各的档位：兑换在飞时解除那句不该被顶掉，反之也一样。
    #[gpui_kit::test]
    fn the_two_actions_keep_their_own_status(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx: &mut App| {
            crate::ui::services::config::ConfigHolder::install(folio_core::Config::default(), cx);
            let entity = super::entity(cx);
            entity.update(cx, |state, _| {
                state.status = Status::Running;
                state.release = ReleaseStatus::Done(Release::NothingBound);
            });
            assert!(entity.read(cx).busy(), "兑换那一趟还在飞");
            assert!(!entity.read(cx).releasing(), "解除那一趟已经落定");
            assert_eq!(
                super::release_status(cx),
                ReleaseStatus::Done(Release::NothingBound),
                "解除那句不该被顶掉"
            );
        });
    }
}
