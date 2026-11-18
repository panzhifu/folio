/// iOS/OHOS 由系统管理进程；桌面和 Android 可经现有有序退出路径结束应用。
pub(crate) const CAN_QUIT_FOR_RESTORE: bool = cfg!(any(not(mobile), target_os = "android"));

/// 发生损坏时停止启动，避免曾经恢复的新库失效后静默回滚旧数据。
pub(super) fn activate() -> bool {
    if let Err(error) = folio_core::backup::activate_restore_on_startup() {
        eprintln!("Folio 无法激活恢复书库；已保留全部数据：{error:#}");
        log::error!("无法激活恢复书库；已停止启动，未回滚旧库：{error:#}");
        return false;
    }
    true
}
