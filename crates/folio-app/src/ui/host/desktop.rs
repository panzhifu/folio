//! 桌面端没有系统栏 / 输入法 inset：窗口是普通桌面窗口，chrome 由窗口管理器给。
//!
//! 字体回退与移动端不同：桌面的 GPUI 自带一份回退表（`.ZedSans` / Helvetica / Noto /
//! DejaVu…），要不到偏好家族时交给它自己退，而不是挑一个 `all_font_names()` 报出来的名字
//! —— 那个名单里第一个可能是符号字体。

use gpui_kit::{App, Window};

use super::{BackAtRoot, Host, Insets, find_family, log_no_family};

pub(super) struct DesktopHost;

impl Host for DesktopHost {
    fn safe_area(&self, _window: &Window) -> Insets {
        Insets::ZERO
    }

    fn ime_insets(&self, _window: &Window) -> Insets {
        Insets::ZERO
    }

    fn overlay_controls(&self) -> bool {
        true
    }

    fn back_at_root(&self) -> BackAtRoot {
        // 桌面的窗口归窗口管理器与那条标题栏管：返回键弹完栈就该停在那儿，
        // 把整个进程关掉是错的动作，也没有「丢回后台」这一说 —— 那一条是任务栈的概念。
        BackAtRoot::Stay
    }

    fn long_press(&self) -> bool {
        // 鼠标不产生 `TouchEvent`，gpui 那份长按状态机因此永远不会进 `LongPressing`。
        false
    }

    fn mouse_wheel(&self) -> bool {
        // 桌面的滚轮是一条独立手势，该归缩放：查看器里拖动已经管平移，方向键管切图。
        true
    }

    fn picked_files_are_temporary(&self) -> bool {
        // 桌面文件选择器交回的就是读者自己的文件路径，导入只拷不删。
        false
    }

    fn share_file(&self, bytes: &[u8], _mime: &str, name: &str) {
        // 桌面没有系统级分享面板：落到下载目录并写一行日志，读者自己去取。
        write_to_downloads(bytes, name);
    }

    fn save_image(&self, bytes: &[u8], _mime: &str, name: &str) {
        write_to_downloads(bytes, name);
    }

    fn body_family(&self, wanted: &str, cx: &App) -> String {
        match find_family(wanted, cx) {
            Some(hit) => hit,
            None => {
                // 桌面落到这里意味着偏好家族和整串回退名都不在设备里。日志把名单打出来，
                // 是要看「这台机器到底有什么」，不是要猜一个 —— 猜的那一个可能是符号字体。
                log_no_family(cx);
                wanted.to_string()
            }
        }
    }
}

/// 桌面版「分享 / 下载 / 日志副本」的落点：`~/Downloads`（拿不到就退到系统临时目录）。
///
/// 桌面没有系统分享面板，「分享」与「下载」因此是同一件事 —— 把文件放一个读者找得到的
/// 地方。失败只写日志：一次分享没落盘不该把阅读页拖退。
///
/// 返回落到的路径（失败 `None`）：反馈那一屏要把这串字**写给读者看**（他得知道去哪找那个
/// 文件来附到邮件上），所以这里不能只留一行日志。
pub(crate) fn write_to_downloads(bytes: &[u8], name: &str) -> Option<std::path::PathBuf> {
    let dir = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .map(|home| home.join("Downloads"))
        .filter(|dir| dir.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    let path = dir.join(name);
    match std::fs::write(&path, bytes) {
        // 文案不写「图片」：这一条现在也收日志文件。
        Ok(()) => {
            tracing::info!("已写到 {}", path.display());
            Some(path)
        }
        Err(err) => {
            tracing::warn!("写 {} 失败：{err}", path.display());
            None
        }
    }
}
