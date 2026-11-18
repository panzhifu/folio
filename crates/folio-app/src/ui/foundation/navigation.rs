//! 导航的尺寸与动作契约；平台选择在 host，元素组装在 shell。

pub const NAV_WIDTH: f32 = 96.0;
pub const NAV_MARGIN: f32 = 12.0;
pub const NAV_RADIUS: f32 = 28.0;
pub const BAR_HEIGHT: f32 = 56.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SizeClass {
    Compact,
    Medium,
    Expanded,
}

impl SizeClass {
    pub fn for_width(width: f32, touch_navigation: bool) -> Self {
        match (width, touch_navigation) {
            (width, true) if width < 600.0 => Self::Compact,
            (width, false) if width >= 840.0 => Self::Expanded,
            _ => Self::Medium,
        }
    }

    pub fn is_compact(self) -> bool {
        self == Self::Compact
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SelectionAction {
    Exit,
    ToggleAll,
    Group(i64),
    Info,
    Upload,
    Delete,
}

pub fn reserved_width(size: SizeClass) -> f32 {
    match size {
        SizeClass::Compact => 0.0,
        SizeClass::Medium => NAV_WIDTH + NAV_MARGIN * 2.0,
        SizeClass::Expanded => NAV_WIDTH,
    }
}

pub fn bar_offset(safe_bottom: f32) -> f32 {
    NAV_MARGIN + safe_bottom
}

pub fn clearance(safe_bottom: f32) -> f32 {
    BAR_HEIGHT + NAV_MARGIN * 2.0 + safe_bottom
}

#[cfg(test)]
mod tests {
    use crate::ui::foundation::navigation::{SizeClass, clearance, reserved_width};

    #[test]
    fn navigation_breakpoints_preserve_desktop_and_touch_contracts() {
        assert_eq!(SizeClass::for_width(320.0, false), SizeClass::Medium);
        assert_eq!(SizeClass::for_width(840.0, false), SizeClass::Expanded);
        assert_eq!(SizeClass::for_width(599.0, true), SizeClass::Compact);
        assert_eq!(SizeClass::for_width(600.0, true), SizeClass::Medium);
        assert_eq!(SizeClass::for_width(1024.0, true), SizeClass::Medium);
        assert_eq!(reserved_width(SizeClass::Compact), 0.0);
        assert_eq!(clearance(24.0) - clearance(0.0), 24.0);
    }
}
