//! 日志反馈：一份快照，三种设备三种递法。
//!
//! ## 为什么这一格值得存在
//!
//! 一个读电子书的应用最容易出现的反馈是「某本书打不开」，而那种事**读者说不清、作者猜不着**：
//! 同一个 EPUB 在两台设备上走的是不同的容器分支。日志里有那些分支的现场，所以这一格做的事
//! 就是把现场递到作者手里，代价是读者少抄一次路径、少截一张图。
//!
//! ## 三种递法，各按各的设备能给什么
//!
//! - **Android**：交系统分享面板（`host::share_file` → FileProvider → `ACTION_SEND`）。
//!   那一头有真正的面板，读者自己挑邮件、网盘还是聊天工具。
//! - **桌面**：复制一份到下载目录 + 拉起一封 `mailto:` 草稿 + 把那串路径写进剪贴板。
//!   **`mailto:` 按 RFC 6068 带不了附件**，这不是本应用没做完，是协议没有那个位置 ——
//!   所以草稿正文里写清文件在哪，读者手工附上。Linux 上装了 `xdg-email` 时本来能带附件，
//!   这一版**没有**走那条：多一个探测分支、多一类「这台机器上到底算不算有邮件客户端」的
//!   判据，而收益只是省一次手工附加。留作后续。
//! - **没装邮件客户端**（`open_url` 回 `false`）：副本已经落盘了，于是那句提示从
//!   「去附文件」变成「文件在这儿，地址也给你复制了」—— 两条路都走得通，只是顺序不同。
//!
//! ## 一句必须说的隐私提醒
//!
//! 日志里**会有书名与文件路径**：`import:` 那些行写的是《书名》→ 落盘位置（实测
//! `folio-core/src/ingest/import/pipeline.rs:974`）。所以那一格把话摆在按下之前，而不是
//! 按下之后才让人发现递出去的是什么。快照只取**尾部**也是同一件事的另一半。

use std::path::PathBuf;

use folio_core::infra::logging;
use gpui_kit::{App, AppContext as _, Entity, Global, SharedString};

use crate::ui::host;
// 只有桌面那条分支要 `open_url`；移动端递的是分享面板，不碰这个模块。
#[cfg(not(mobile))]
use crate::ui::platform;
use crate::ui::services::config::ConfigHolder;
use crate::ui::services::l10n::tr;

/// 收件地址。
///
/// **写死在这里，不在配置里** —— 计划里那一格「可覆盖」留到有人需要改它时再做：多一个配置项
/// 就多一处「读者改了它，然后把日志发给一个不认识的人」的可能，而这件事的正确默认只有一个。
pub const FEEDBACK_ADDRESS: &str = "noke601508@outlook.com";

/// 快照带多少日志尾巴。256 KiB 是按「一次读一本书的现场够用、又不把整本历史递出去」定的，
/// 不是按 `max_bytes` 那一档定的（上限本身可以在配置里改）。
const SNAPSHOT_TAIL: u64 = 256 * 1024;

/// 那一格下面念的一句：按下去之后发生了什么。
#[derive(Debug, Clone, Default, PartialEq)]
pub enum FeedbackStatus {
    #[default]
    Idle,
    /// 正在读日志、拼快照。
    Working,
    /// Android：已经递给系统分享面板了。
    Shared,
    /// 桌面：副本落在哪。这句要把路径**写给读者看**，他得去那儿取附件。
    Copied(PathBuf),
    /// 桌面：副本落了盘，但那台设备没有认领 `mailto:` 的应用。
    NoMailClient(PathBuf),
    /// 日志没开（`logging.file = false`，或那一格根本没建出来）。
    NoLog,
    /// 写盘失败之类。带着原因，那是一句人话之外的唯一线索。
    Failed(String),
}

#[derive(Debug, Default)]
pub struct FeedbackState {
    status: FeedbackStatus,
}

impl FeedbackState {
    pub fn status(&self) -> &FeedbackStatus {
        &self.status
    }

    fn set(&mut self, status: FeedbackStatus, cx: &mut gpui_kit::Context<Self>) {
        self.status = status;
        cx.notify();
    }
}

pub struct FeedbackHolder(pub Entity<FeedbackState>);

impl Global for FeedbackHolder {}

pub fn handle(cx: &App) -> Option<Entity<FeedbackState>> {
    cx.try_global::<FeedbackHolder>()
        .map(|holder| holder.0.clone())
}

pub fn entity(cx: &mut App) -> Entity<FeedbackState> {
    if let Some(entity) = handle(cx) {
        return entity;
    }
    let entity = cx.new(|_cx| FeedbackState::default());
    cx.set_global(FeedbackHolder(entity.clone()));
    entity
}

/// 那颗「反馈日志」按钮唯一的入口。
///
/// 与 `update::with_notes` 不同，这里不需要 `&mut Window`：那条要在异步回来之后开一层
/// 浮层，而这一条的落点全在应用之外（分享面板、下载目录、邮件草稿）。
pub fn send(cx: &mut App) {
    let holder = entity(cx);
    // 日志没开这件事要先问，别等后台读盘失败再说 —— 那时读者看到的是一句「读不到」，
    // 而真正的原因是配置里 `logging.file` 关着，那是两件完全不同的事。
    let Some(path) = logging::log_file(&ConfigHolder::snapshot(cx).logging) else {
        holder.update(cx, |state, app| state.set(FeedbackStatus::NoLog, app));
        return;
    };
    if holder.read(cx).status() == &FeedbackStatus::Working {
        return; // 连点两下不该读两遍盘
    }
    holder.update(cx, |state, app| state.set(FeedbackStatus::Working, app));

    // 时间戳取**一次**：文件名与头部那行 `taken:` 说的必须是同一时刻，取两次会在跨分钟的那一
    // 瞬间给出一份「文件名 13:30、内容写 13:31」的快照。
    let stamp = folio_core::infra::time::now();
    let work = cx.background_spawn(async move {
        logging::feedback_snapshot(stamp, &path, SNAPSHOT_TAIL)
            .map(|bytes| (logging::snapshot_name(stamp), bytes))
    });
    holder.update(cx, move |_state, app| {
        app.spawn(async move |this: gpui_kit::WeakEntity<FeedbackState>, cx| {
            let outcome = work.await;
            cx.update(|cx| {
                let Some(this) = this.upgrade() else { return };
                match outcome {
                    Ok((name, bytes)) => deliver(this, name, bytes, cx),
                    Err(error) => {
                        tracing::warn!("读日志快照失败：{error}");
                        this.update(cx, |state, app| {
                            state.set(FeedbackStatus::Failed(error.to_string()), app)
                        });
                    }
                }
            });
        })
        .detach();
    });
}

/// 快照到手之后怎么递出去 —— 两端的分支只有这一处。
fn deliver(holder: Entity<FeedbackState>, name: String, bytes: Vec<u8>, cx: &mut App) {
    #[cfg(mobile)]
    {
        let _ = &name;
        host::share_file(&bytes, "text/plain", &name, cx);
        holder.update(cx, |state, app| state.set(FeedbackStatus::Shared, app));
    }
    #[cfg(not(mobile))]
    {
        let Some(path) = host::save_copy(&bytes, &name) else {
            holder.update(cx, |state, app| {
                state.set(FeedbackStatus::Failed(format!("写 {name} 失败")), app)
            });
            return;
        };
        let subject = format!("folio {} 反馈", env!("CARGO_PKG_VERSION"));
        let body = format!(
            "folio {v} / {os}\n\n日志副本已存到：{path}\n请把这个文件作为附件发出来（邮件草稿带不了附件）。\n\n问题描述：\n",
            v = env!("CARGO_PKG_VERSION"),
            os = std::env::consts::OS,
            path = path.display(),
        );
        let opened = platform::open_url(&mailto(FEEDBACK_ADDRESS, &subject, &body));
        // 路径进剪贴板：那句「请把这个文件作为附件」要能被直接粘进文件选择框，
        // 而不是让读者用眼睛抄一串 `/home/…/folio-log-…txt`。
        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
            path.display().to_string(),
        ));
        let status = if opened {
            FeedbackStatus::Copied(path)
        } else {
            tracing::warn!(
                "这台设备没有认领 mailto: 的应用；副本已在 {}",
                path.display()
            );
            FeedbackStatus::NoMailClient(path)
        };
        holder.update(cx, |state, app| state.set(status, app));
    }
}

/// 拼一条 `mailto:`。**必须自己转义**：主题与正文里有中文、空格、冒号、换行，
/// 一个没转义的 `&` 或 `?` 会把正文的后半截变成另一个查询参数。
///
/// 空格编码成 `%20` 而不是 `+`：`+` 只在 `application/x-www-form-urlencoded` 里是空格，
/// 而 URL 的查询串按 RFC 3986 没有这个约定，各家邮件客户端对 `+` 的读法并不一致。
pub fn mailto(address: &str, subject: &str, body: &str) -> String {
    let mut out = String::from("mailto:");
    out.push_str(&encode(address));
    out.push_str("?subject=");
    out.push_str(&encode(subject));
    out.push_str("&body=");
    out.push_str(&encode(body));
    out
}

/// RFC 3986 的未保留字符之外一律 `%XX`。
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// 那一格下面念的那句。文案住在服务层与 `update::status_label` 同一条理由：
/// 桌面与紧凑两屏都要念同一句，两处各写一遍迟早只改一处。
pub fn status_label(status: &FeedbackStatus, cx: &App) -> SharedString {
    match status {
        FeedbackStatus::Idle => tr(cx, "feedback.idle"),
        FeedbackStatus::Working => tr(cx, "feedback.working"),
        FeedbackStatus::Shared => tr(cx, "feedback.shared"),
        FeedbackStatus::Copied(_) => tr(cx, "feedback.copied"),
        FeedbackStatus::NoMailClient(_) => tr(cx, "feedback.no_mail"),
        FeedbackStatus::NoLog => tr(cx, "feedback.no_log"),
        FeedbackStatus::Failed(_) => tr(cx, "feedback.failed"),
    }
}

/// 那句提示后面要不要再补一行路径。`Copied` 与 `NoMailClient` 那两档路径是**读者要用的信息**，
/// 不是日志里的细节，所以单独摆一行；其余几档没有路径可摆。
pub fn status_path(status: &FeedbackStatus) -> Option<&std::path::Path> {
    match status {
        FeedbackStatus::Copied(path) | FeedbackStatus::NoMailClient(path) => Some(path),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mailto_line_escapes_everything_that_could_end_the_body() {
        let url = mailto(
            "someone@example.test",
            "folio 0.1.1 反馈 & 一个问题",
            "路径：/home/x/folio-log.txt?a=1\n第二行",
        );
        assert!(
            url.starts_with("mailto:someone%40example.test?subject="),
            "{url}"
        );
        // `&` 不转义的话，正文从那里就被切成第二个参数了。
        assert!(url.contains("%26"), "主题里的 & 必须转义：{url}");
        assert!(
            !url[url.find("&body=").unwrap()..].contains(' '),
            "正文里不该有裸空格"
        );
        assert!(url.contains('%'), "{url}");
        assert!(
            !url.contains('+'),
            "`+` 在 URL 查询串里不等于空格，各家读法不一致：{url}"
        );
        // 换行是控制字符，留着会让很多客户端把草稿截断在第一行。
        assert!(!url.contains('\n'), "草稿正文里不该有裸换行：{url}");
    }

    #[test]
    fn the_address_is_the_one_the_author_asks_for() {
        // 这串字会被读者粘进邮件客户端，抄错一位就发到一个不存在的地方，而且没人会知道。
        assert_eq!(FEEDBACK_ADDRESS, "noke601508@outlook.com");
        assert!(mailto(FEEDBACK_ADDRESS, "s", "b").starts_with("mailto:"));
    }

    #[test]
    fn only_the_two_desktop_outcomes_carry_a_path() {
        assert!(status_path(&FeedbackStatus::Shared).is_none());
        assert!(status_path(&FeedbackStatus::NoLog).is_none());
        let path = std::path::PathBuf::from("/tmp/folio-log.txt");
        assert_eq!(
            status_path(&FeedbackStatus::Copied(path.clone())),
            Some(path.as_path())
        );
        assert_eq!(
            status_path(&FeedbackStatus::NoMailClient(path.clone())),
            Some(path.as_path())
        );
    }
}
