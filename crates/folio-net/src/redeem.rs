//! 会员兑换的协议层：一个爱发电订单号换回多少天会员。
//!
//! 判「这单真付过钱」只能在服务端做：爱发电的 api token 一旦进了客户端就等于公开，
//! 拿到它的任何人都能查这位读者的全部订单 —— 包括买了什么、付了多少、哪个账号买的。
//! 所以本地这一层只校形状、只发一次 POST，剩下的交给 `folio-redeem` 那支函数。
//!
//! 与 [`update`] 同一形状：收一个 [`Transport`] 而不是自己建连接，所以离线可测
//! （假传输 + 一段 fixture JSON），公共签名里没有 `Future`。

use serde::Deserialize;

use crate::{Method, Request, Response, Transport};

/// 兑换服务端的入口。挂在 CloudBase HTTP 网关的默认域名上，路径 `folio-redeem`。
///
/// 主机名里那段 `-1476411022` 是账号 UIN，不是可选装饰：不带它的 `shocked-roundworm-d1dqjn15d00708.ap-shanghai...`
/// 实测回 404 `INVALID_ENV`（2026-11-05），因为环境根本没解析到。
///
/// 写死而不是开一格配置：读者能填任意 URL 的话，那格就成了「把订单号发给谁」的开关 ——
/// 兑换请求带着订单号与设备 id，填错一格等于把这两样交给别人的服务器。
pub const ENDPOINT: &str = "https://shocked-roundworm-d1dqjn15d00708-1476411022.ap-shanghai.app.tcloudbase.com/folio-redeem/redeem";

/// 一次兑换的好结果。
///
/// `Bound` 不在这一档：它不是「兑成了什么」，而是「这单不是你的」，所以是错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// 这一单第一次被兑现，换到这么多天。
    Granted(i64),
    /// 这一单第一次被兑现，且是**买断档**：不做为期限，权益跟设备绑定走。
    ///
    /// 单独一档而不是并回 [`Outcome::Granted`]：天数那一档的「接在今天后面」整套算法
    /// 对买断没有意义，而台账与状态句也要各说各的话。
    GrantedLifetime,
    /// 这一单在**另一台**上兑过、后来被软解除，现在由这台认领过来。`days` 是**还剩下的那几天**
    /// —— 服务端按当初的发货时刻数出来，客户端接在今天后面。
    ///
    /// 单独一档而不是并回 [`Outcome::Granted`]：并回去就看不出「这一趟没发新货」，而更要紧的是
    /// 那一单的整笔天数**不许**再给一次 —— 给一次，「解除 → 重兑」每轮就白得一整年。
    Moved(i64),
    /// 同 [`Outcome::Moved`]，但认领的是一单**买断**：认领过来的是永久权益，不是剩下的天数。
    MovedLifetime,
    /// 同一台设备把同一单重交了一次：不加天数，因为第一次就已经给了。
    ///
    /// 单独一档而不是并回 [`Outcome::Granted(0)`]：界面要说的是「已经兑过了」，
    /// 而不是「兑到了 0 天」—— 后者读起来像被扣了钱。
    Replay,
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    /// 订单号不成形状。本地就能判出来，所以**不发请求**。
    #[error("订单号不像是爱发电的：请整段复制订单详情里的编号")]
    BadOrder,

    /// 账号下查不到这一单。
    #[error("查不到这个订单号：请到爱发电的订单里再核对一遍")]
    NotFound,

    /// 有这一单，但没付钱（待付、已退款、或平台还没结）。
    #[error("这一单还没有成交")]
    Unpaid,

    /// 成交了，但它不属于任何能换会员的方案。
    #[error("这一单不是会员方案")]
    WrongPlan,

    /// 这单已经被别的设备兑走了。
    #[error("这个订单号已经在另一台设备上兑过了")]
    Bound,

    /// 这一单的会员时间已经走完，搬不过来。
    ///
    /// 与 [`Error::Bound`] 分开：那一档是「这单还绑在别的设备上」，去找回原来那台；这一档是
    /// 「就算搬过来也是零天」，读者要做的是再买一次，而不是去找谁。
    #[error("这一单的会员时间已经用完了")]
    Spent,

    /// 服务端没配齐（缺 token、缺方案表、缺库密钥）。
    ///
    /// 与下面那档分开：这一档是作者这边的事，读者重试一百次也不会好，
    /// 所以界面那句不该催他再试一次。
    #[error("激活服务还没配置好，请过些时候再试或到爱发电私信作者")]
    Unconfigured,

    /// 上游（爱发电或数据库）出了问题。
    #[error("激活服务暂时不可用，请稍后再试")]
    Upstream,

    /// 请求根本没成为一次 HTTP 往返。
    #[error("连不上激活服务：{0}")]
    Unreachable(String),

    /// 回了不是 JSON，或 `state` 是这一层不认的名字。
    ///
    /// 不认的档位名一律算坏，不当「失败的一种」吞掉：服务端加了新状态而这里没跟上时，
    /// 宁可报「读不懂」，也不要悄悄显示成一句对不上症状的现成文案。
    #[error("激活服务的回应读不懂：{0}")]
    Malformed(String),
}

/// 服务端那一格回应的形状。`days` 只有 `granted` / `moved` / `replay` 三档用得到，缺了按 0；
/// `released` 只有解除那两档用得到；`lifetime` 只在买断档出现，缺了按 false（老服务端没有
/// 这一格，天数档照旧）。
#[derive(Debug, Deserialize)]
struct Wire {
    ok: bool,
    state: String,
    #[serde(default)]
    days: i64,
    #[serde(default)]
    released: i64,
    #[serde(default)]
    lifetime: bool,
}

/// 订单号的形状：爱发电的 `out_trade_no` 是一串 29 位数字。
///
/// 收成 16–40 位纯数字是给平台留改动余地，同时把「粘进来一整段订单描述」这种
/// 最常见的误用在本地挡掉 —— 省一次往返，也别让脏字符串进服务端的日志。
pub fn valid_order(order: &str) -> bool {
    let bytes = order.as_bytes();
    (16..=40).contains(&bytes.len()) && bytes.iter().all(u8::is_ascii_digit)
}

/// 兑一次。`device` 是这台安装的稳定 id（`folio_model::sync` 那一格），服务端拿它绑「这单归谁」。
pub fn redeem(transport: &dyn Transport, order: &str, device: &str) -> Result<Outcome, Error> {
    let order = order.trim();
    if !valid_order(order) {
        return Err(Error::BadOrder);
    }
    if device.is_empty() {
        return Err(Error::Malformed("没有可用的设备 id".to_owned()));
    }
    let body = serde_json::json!({ "order": order, "device": device }).to_string();
    let request = Request::new(Method::Post, ENDPOINT.to_owned())
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .body(body.into_bytes());
    let response = transport
        .request(request)
        .map_err(|error| Error::Unreachable(error.to_string()))?;
    parse(&response)
}

/// 把回应的 JSON 翻成 [`Outcome`] / [`Error`]。
///
/// 单独公开出来是为了让「一段 JSON 意味着什么」这一格能被测试钉住 —— 它才是这一层的
/// 全部判断，网络那一段没有分支。
pub fn parse(response: &Response) -> Result<Outcome, Error> {
    // 服务端一律回 200 + 一个 state 名（含失败），所以状态码这里只用来认「不是它回的话」。
    if response.status != 200 {
        return Err(Error::Upstream);
    }
    let wire: Wire = serde_json::from_slice(&response.body)
        .map_err(|error| Error::Malformed(format!("响应不是预期的 JSON：{error}")))?;
    classify(wire)
}

/// 一次解除的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Release {
    /// 这台设备名下原本挂着这么多笔激活，现在都被标成已解除：那些单可以在新设备上认领。
    Released(i64),
    /// 这台设备名下本来就没有行。当成「已经干净」而不是错误 —— 调用方要的就是一个干净的
    /// 这台，而连按第二次那颗钮正好落在这里。把它报成失败会让读者以为没解除成功，再按第三下。
    NothingBound,
}

/// 解除**这台设备**名下的激活。`device` 就是兑换时送上去的那一个：服务端靠它认「哪几行是这台」。
///
/// 这里不收订单号，是有意的：本机从来没存过它（草稿落盘只会让 config.json 多出一串别人看不懂
/// 的数字，那条决定写在 `folio-app` 的 `services/activation.rs` 顶上），而重装之后连草稿都没了 ——
/// 要求交出订单号，等于让最需要这个功能的人正好交不出来。
pub fn release(transport: &dyn Transport, device: &str) -> Result<Release, Error> {
    if device.is_empty() {
        return Err(Error::Malformed("没有可用的设备 id".to_owned()));
    }
    let body = serde_json::json!({ "device": device, "action": "release" }).to_string();
    let request = Request::new(Method::Post, ENDPOINT.to_owned())
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .body(body.into_bytes());
    let response = transport
        .request(request)
        .map_err(|error| Error::Unreachable(error.to_string()))?;
    parse_release(&response)
}

/// 解除那一段回应的形状。与 [`parse`] 分开，因为两趟的档位不是一套：解除不向爱发电查单，
/// 所以 `not_found` / `unpaid` / `plan` / `bound` 出现在这里只可能是两边形状对不上。
pub fn parse_release(response: &Response) -> Result<Release, Error> {
    if response.status != 200 {
        return Err(Error::Upstream);
    }
    let wire: Wire = serde_json::from_slice(&response.body)
        .map_err(|error| Error::Malformed(format!("响应不是预期的 JSON：{error}")))?;
    classify_release(wire.ok, &wire.state, wire.released)
}

fn classify(wire: Wire) -> Result<Outcome, Error> {
    let Wire {
        ok,
        state,
        days,
        lifetime,
        ..
    } = wire;
    match (ok, state.as_str()) {
        // 买断档先行：它带 days=0（天数算法对它没有意义），按天数那一档的 `days > 0`
        // 去挡会把它错杀成「读不懂」。
        (true, "granted") if lifetime => Ok(Outcome::GrantedLifetime),
        (true, "granted") if days > 0 => Ok(Outcome::Granted(days)),
        // 认领那一档同样要求 `days > 0`：服务端在剩下 0 天时回的是 `spent`，走到这一档而
        // 天数又是 0，说明两边形状对不上 —— 宁可报读不懂，也不要悄悄记一笔空账。
        // 买断的认领不带天数，先行。
        (true, "moved") if lifetime => Ok(Outcome::MovedLifetime),
        (true, "moved") if days > 0 => Ok(Outcome::Moved(days)),
        (true, "replay") => Ok(Outcome::Replay),
        (false, "not_found") => Err(Error::NotFound),
        (false, "unpaid") => Err(Error::Unpaid),
        (false, "plan") => Err(Error::WrongPlan),
        (false, "bound") => Err(Error::Bound),
        (false, "spent") => Err(Error::Spent),
        (false, "unconfigured") => Err(Error::Unconfigured),
        // 上游、写库、异常三种坏法对读者是同一句话，但对得上不同的排查方向，所以档位留着，
        // 在这里收成一条：界面那一层不需要知道是爱发电没回还是库写坏了。
        (false, "upstream" | "storage" | "internal") => Err(Error::Upstream),
        // bad_code / bad_device / bad_body / route / method 都只会在客户端自己坏了时出现，
        // 走到这里说明两边对不上形状 —— 按读不懂处理，而不是假装是一次正常的拒绝。
        _ => Err(Error::Malformed(format!("不认识的状态 {state}"))),
    }
}

fn classify_release(ok: bool, state: &str, released: i64) -> Result<Release, Error> {
    match (ok, state) {
        (true, "released") => Ok(Release::Released(released)),
        (true, "nothing_bound") => Ok(Release::NothingBound),
        (false, "unconfigured") => Err(Error::Unconfigured),
        // 与兑换同一口径：上游、写库、异常对读者是同一句话，但排查方向不同，所以档位留着，
        // 在这一层收成一条。
        (false, "upstream" | "storage" | "internal") => Err(Error::Upstream),
        // bad_device / bad_body / route / method，以及兑换那一段才有的档位（not_found、
        // unpaid、plan、bound）走到这里，都说明两边对不上形状 —— 按读不懂处理。
        _ => Err(Error::Malformed(format!(
            "解除的回应里出现了不认识的状态 {state}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    //! 这一层钉三件事：**订单号的形状在本地判**（不该为一句中文提示发一次网络请求）、
    //! **每个服务端档位落到哪一句**、以及**不认的档位名不许被悄悄吞掉**。
    //!
    //! 「天数从哪儿累加」不在这里 —— 那是界面拿到 [`Outcome::Granted`] 之后与
    //! `Membership::until_day` 的算术，见 `folio-app` 的 `services/activation.rs`。

    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use super::{Outcome, Release, parse, parse_release, redeem, release, valid_order};
    use crate::{Cancel, Method, Request, Response, Sink, Transport};

    /// 一次性假传输：把要回的东西排好队，同时留下发出去的请求供用例检查。
    struct Mock {
        answers: Arc<Mutex<VecDeque<Response>>>,
        seen: Arc<Mutex<Vec<Request>>>,
    }

    impl Mock {
        fn new(bodies: &[&str]) -> Self {
            Self {
                answers: Arc::new(Mutex::new(
                    bodies
                        .iter()
                        .map(|b| Response::new(200, b.as_bytes().to_vec()))
                        .collect(),
                )),
                seen: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn sent(&self) -> Vec<Request> {
            self.seen.lock().unwrap().clone()
        }
    }

    impl Transport for Mock {
        fn request(&self, req: Request) -> crate::Result<Response> {
            self.seen.lock().unwrap().push(req.clone());
            self.answers
                .lock()
                .unwrap()
                .pop_front()
                .map(Ok)
                .expect("假传输的响应已用尽：用例少给了一个")
        }

        fn download(
            &self,
            _req: Request,
            _sink: &mut dyn Sink,
            _cancel: &Cancel,
        ) -> crate::Result<u64> {
            unreachable!("兑换只有普通请求，没有流式下载")
        }
    }

    /// 29 位数字：服务端文档里 `out_trade_no` 的真实长度。
    const ORDER: &str = "202106232138371083454010626";

    fn response(json: &str) -> Response {
        Response::new(200, json.as_bytes().to_vec())
    }

    #[test]
    fn the_order_shape_is_digits_only_and_locally_judged() {
        assert!(valid_order(ORDER));
        // 16 位以下与 40 位以上都不认：太短多半是编号的后半截，太长多半是整段描述被粘进来。
        assert!(!valid_order("123456789012345"));
        assert!(!valid_order(&"7".repeat(41)));
        // 粘了空白、带了「订单号：」前缀、或者混进中文，都在本地就判掉。
        assert!(!valid_order(" 202106232138371083454010626 "));
        assert!(!valid_order("订单号202106232138371083454010626"));
        assert!(!valid_order(""));
    }

    #[test]
    fn a_misshapen_order_never_reaches_the_network() {
        let mock = Mock::new(&[]);
        let err = redeem(&mock, "不是订单号", "dev-1").unwrap_err();
        assert_eq!(err.to_string(), super::Error::BadOrder.to_string());
        assert!(
            mock.sent().is_empty(),
            "形状不对还发请求，等于把这句中文发到服务端去"
        );
    }

    #[test]
    fn the_request_carries_the_order_and_the_device() {
        let mock = Mock::new(&[r#"{"ok":true,"state":"granted","days":365}"#]);
        let outcome = redeem(&mock, ORDER, "dev-42").unwrap();
        assert_eq!(outcome, Outcome::Granted(365));
        let sent = mock.sent().remove(0);
        assert_eq!(sent.method, Method::Post);
        assert!(sent.url.starts_with("https://"), "订单号不许走明文");
        let body: serde_json::Value = serde_json::from_slice(&sent.body).unwrap();
        assert_eq!(body["order"], ORDER);
        assert_eq!(body["device"], "dev-42");
    }

    /// 服务端那一段数字前后带空白的（复制粘贴常见病），进请求之前就该去掉。
    #[test]
    fn surrounding_whitespace_is_trimmed_before_the_request() {
        let mock = Mock::new(&[r#"{"ok":true,"state":"granted","days":30}"#]);
        assert_eq!(
            redeem(&mock, &format!("  {ORDER}\n"), "dev-1").unwrap(),
            Outcome::Granted(30)
        );
        // 请求照发（trim 之后它是合法的），但送出去的必须是 trim 过的那一串 ——
        // 带空白的编号在服务端那次精确匹配里查不到单。
        let sent = mock.sent().remove(0);
        let body: serde_json::Value = serde_json::from_slice(&sent.body).unwrap();
        assert_eq!(body["order"], ORDER);
    }

    /// 每一档落点。这条是这一层的全部判断，逐档写而不是抽两档：界面那些句子各对应
    /// 一件事，混档会说出对不上症状的话（比如把「这单不是你的」报成「网络不好」）。
    #[test]
    fn every_server_state_lands_on_its_own_sentence() {
        let cases: &[(&str, &str)] = &[
            (r#"{"ok":false,"state":"not_found"}"#, "查不到"),
            (r#"{"ok":false,"state":"unpaid"}"#, "还没有成交"),
            (r#"{"ok":false,"state":"plan"}"#, "不是会员方案"),
            (r#"{"ok":false,"state":"bound"}"#, "已经在另一台设备"),
            (r#"{"ok":false,"state":"spent"}"#, "已经用完"),
            (r#"{"ok":false,"state":"unconfigured"}"#, "还没配置好"),
            (r#"{"ok":false,"state":"upstream"}"#, "暂时不可用"),
            (r#"{"ok":false,"state":"storage"}"#, "暂时不可用"),
            (r#"{"ok":false,"state":"internal"}"#, "暂时不可用"),
        ];
        for (json, expect) in cases {
            let err = parse(&response(json)).unwrap_err();
            assert!(
                err.to_string().contains(expect),
                "{json} 落到了「{err}」，句里没有「{expect}」"
            );
        }
    }

    #[test]
    fn a_replay_is_a_success_without_days() {
        assert_eq!(
            parse(&response(r#"{"ok":true,"state":"replay","days":0}"#)).unwrap(),
            Outcome::Replay
        );
        // 重交不该报成「兑到 0 天」：读者看到 0 会以为自己白付了一次。
        assert!(parse(&response(r#"{"ok":true,"state":"granted","days":0}"#)).is_err());
    }

    #[test]
    fn an_unknown_state_is_read_as_malformed_not_as_a_refusal() {
        // 服务端将来加了新档位，这里必须炸出来，而不是悄悄落到某一句现成的话上。
        let err = parse(&response(r#"{"ok":false,"state":"newly_added"}"#)).unwrap_err();
        assert!(matches!(err, super::Error::Malformed(_)), "落成了 {err}");
        assert!(parse(&response("not json at all")).is_err());
        assert!(matches!(
            parse(&Response::new(503, Vec::new())).unwrap_err(),
            super::Error::Upstream
        ));
    }

    /// 认领那一档带的是**还剩下的天数**，不是当初那一整笔。剩下 0 天时服务端不走这一档
    /// （它回 `spent`），所以「moved 而 days 是 0」只可能是两边形状对不上 —— 按读不懂处理，
    /// 而不是悄悄记一笔空账。
    #[test]
    fn a_takeover_carries_the_days_left_and_never_a_fresh_grant() {
        assert_eq!(
            parse(&response(r#"{"ok":true,"state":"moved","days":200}"#)).unwrap(),
            Outcome::Moved(200),
        );
        assert!(
            matches!(
                parse(&response(r#"{"ok":true,"state":"moved","days":0}"#)).unwrap_err(),
                super::Error::Malformed(_)
            ),
            "0 天的 moved 该按读不懂处理"
        );
    }

    /// 买断档带 `days: 0`（天数算法对它没有意义），落在自己的两个结果上；
    /// 老服务端没有 `lifetime` 这一格，天数档照旧 —— serde 缺省 false 就是要保这条退路。
    #[test]
    fn a_lifetime_order_is_its_own_outcome_and_days_stay_days() {
        assert_eq!(
            parse(&response(
                r#"{"ok":true,"state":"granted","days":0,"lifetime":true}"#
            ))
            .unwrap(),
            Outcome::GrantedLifetime
        );
        assert_eq!(
            parse(&response(
                r#"{"ok":true,"state":"moved","days":0,"lifetime":true}"#
            ))
            .unwrap(),
            Outcome::MovedLifetime
        );
        assert_eq!(
            parse(&response(r#"{"ok":true,"state":"granted","days":30}"#)).unwrap(),
            Outcome::Granted(30)
        );
        assert!(
            matches!(
                parse(&response(
                    r#"{"ok":true,"state":"granted","days":0,"lifetime":false}"#
                ))
                .unwrap_err(),
                super::Error::Malformed(_)
            ),
            "不带 lifetime 的 0 天 granted 仍是两边形状对不上"
        );
    }

    /// 解除送出去的是**设备 id 加一个 action**，没有订单号：本机从来没存过那一串（见
    /// `services/activation.rs` 顶上那条），而这一趟的判据本来就是「哪几行属于这台」。
    #[test]
    fn the_release_carries_the_device_and_no_order() {
        let mock = Mock::new(&[r#"{"ok":true,"state":"released","released":1}"#]);
        assert_eq!(release(&mock, "dev-42").unwrap(), Release::Released(1));
        let sent = mock.sent().remove(0);
        assert_eq!(sent.method, Method::Post);
        assert!(sent.url.starts_with("https://"), "设备 id 不许走明文");
        let body: serde_json::Value = serde_json::from_slice(&sent.body).unwrap();
        assert_eq!(body["device"], "dev-42");
        assert_eq!(body["action"], "release");
        assert!(body.get("order").is_none(), "解除不该带订单号");
    }

    /// 没有设备 id 就不出门：那会在台账上留一个谁也认领不上的洞（与兑换同一条理由）。
    #[test]
    fn a_release_without_a_device_never_reaches_the_network() {
        let mock = Mock::new(&[]);
        assert!(matches!(
            release(&mock, "").unwrap_err(),
            super::Error::Malformed(_)
        ));
        assert!(mock.sent().is_empty());
    }

    /// 解除那一段每一档的落点。`nothing_bound` 是**成功**而不是错误：调用方要的就是一个
    /// 干净的这台，把它报成失败会让读者再按一次那颗钮。
    #[test]
    fn every_release_state_lands_where_it_should() {
        let ok_cases: &[(&str, Release)] = &[
            (
                r#"{"ok":true,"state":"released","released":2}"#,
                Release::Released(2),
            ),
            (
                r#"{"ok":true,"state":"nothing_bound","released":0}"#,
                Release::NothingBound,
            ),
            // 老服务端没带 `released` 这一格也认：缺了按 0，落进「解除了一笔但数不清」而不是坏掉。
            (r#"{"ok":true,"state":"released"}"#, Release::Released(0)),
        ];
        for (json, want) in ok_cases {
            assert_eq!(&parse_release(&response(json)).unwrap(), want, "{json}");
        }
        let err_cases: &[(&str, &str)] = &[
            (r#"{"ok":false,"state":"unconfigured"}"#, "还没配置好"),
            (r#"{"ok":false,"state":"storage"}"#, "暂时不可用"),
            (r#"{"ok":false,"state":"internal"}"#, "暂时不可用"),
            // 兑换那一段才有的档位出现在解除的回应里 = 两边形状对不上，按读不懂处理。
            (r#"{"ok":false,"state":"not_found"}"#, "不认识的状态"),
        ];
        for (json, expect) in err_cases {
            let err = parse_release(&response(json)).unwrap_err();
            assert!(
                err.to_string().contains(expect),
                "{json} 落到了「{err}」，句里没有「{expect}」"
            );
        }
        assert!(matches!(
            parse_release(&Response::new(503, Vec::new())).unwrap_err(),
            super::Error::Upstream
        ));
    }
}
