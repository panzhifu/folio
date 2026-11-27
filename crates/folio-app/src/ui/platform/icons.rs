//! folio 的图标子集：`AllAssets` 把 lucide 全目录（1830 个 svg）整包嵌进 `.so`，而
//! folio 与 gpui-kit 组件真正问津的只有其中一百来个 —— 这一模块只嵌用得上的那些。
//!
//! 两层合成（[`FolioAssets`]）：
//! - [`FolioIcons`]：folio 源码里出现过的 `IconName` variant（86 个，build 时由宏
//!   `icon_assets!` 逐个嵌字节）；
//! - `gpui_kit::assets::Assets`：gpui-kit 组件的默认名单（`default-icons.txt`，106 个）
//!   —— 组件内部画的图标不经 folio 源码，必须从这里兜底。
//!
//! 未列出的路径回 `None`，屏上就是一块空白：往 UI 里引新图标时，**忘了把它加进下面
//! 的名单会安静地缺图**。`cargo run` 起一眼就是那条护栏（桌面同一份清单）。

use std::borrow::Cow;

use gpui_kit::assets::{Assets, icon_assets};
use gpui_kit::{AssetSource, Result, SharedString};

icon_assets!(
    FolioIcons,
    [
        Activity,
        ALargeSmall,
        Archive,
        ArrowDownUp,
        ArrowLeft,
        AudioLines,
        BookHeadphones,
        Bookmark,
        BookmarkCheck,
        BookOpen,
        Broom,
        Bug,
        CalendarDays,
        ChartColumn,
        Check,
        ChevronDown,
        ChevronLeft,
        ChevronRight,
        CircleEllipsis,
        CircleUser,
        Clock,
        Cloud,
        CloudDownload,
        CloudOff,
        CloudUpload,
        Columns3,
        Copy,
        Crown,
        Database,
        Download,
        EllipsisVertical,
        Eraser,
        EyeOff,
        FastForward,
        FileText,
        Flame,
        Folder,
        FolderOpen,
        FolderPlus,
        FoldVertical,
        Gauge,
        HardDrive,
        Headphones,
        Highlighter,
        Info,
        Languages,
        Library,
        ListChecks,
        ListFilter,
        ListTree,
        MapPin,
        Maximize,
        Maximize2,
        Minimize2,
        Minus,
        Moon,
        Network,
        NotebookPen,
        Palette,
        Pause,
        Play,
        Plus,
        Rewind,
        RotateCcw,
        ScanSearch,
        Search,
        Settings,
        Share2,
        SkipBack,
        SkipForward,
        SlidersHorizontal,
        Smartphone,
        Sparkles,
        SquareCheck,
        StickyNote,
        StickyNoteCheck,
        SunDim,
        SunMedium,
        Tag,
        Timer,
        Trash,
        Type,
        Underline,
        UnfoldVertical,
        WavesHorizontal,
        X,
    ]
);

/// folio 的完整资产源：子集优先，组件默认名单兜底。
pub(crate) struct FolioAssets;

impl AssetSource for FolioAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        // 注意是「值兜底」而不是 `Result::or_else`：子集对未列路径回的是
        // `Ok(None)`，那条不触发 or_else，组件默认名单就永远轮不到。默认名单那一层
        // 对**未知**路径回 `Err`（rust-embed 的口径），这里折回 `Ok(None)`——合成源
        // 保持子集宏「未列出 → None」的同一份语义，不把缺图变成一条错误。
        match FolioIcons.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => match Assets.load(path) {
                Ok(found) => Ok(found),
                Err(_) => Ok(None),
            },
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut out = FolioIcons.list(path)?;
        for entry in Assets.list(path)? {
            if !out.contains(&entry) {
                out.push(entry);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::FolioAssets;
    use gpui_kit::AssetSource as _;
    use gpui_kit::assets::{Assets, IconName};

    #[test]
    fn folio_icons_embed_and_component_defaults_fall_through() {
        let assets = FolioAssets;
        // folio 自己的图标：子集里真有字节。
        assert!(
            assets.load(&IconName::Play.path()).unwrap().is_some(),
            "Play 应在子集里"
        );
        // 组件默认名单里有、folio 子集没有的那一个：谓词问的是**子集**（合成源兜底后
        // 106 个默认图标全能取到，问它永远找不到「缺的」）。
        let default_only = Assets
            .list("icons/")
            .unwrap()
            .into_iter()
            .find(|path| super::FolioIcons.load(path).unwrap().is_none())
            .expect("默认名单里应存在子集没有的图标");
        // 合成源兜底之后它也在了。
        assert!(assets.load(&default_only).unwrap().is_some());
        // 谁都不认识的路径回 None，不 panic。
        assert!(assets.load("icons/not-a-real-icon.svg").unwrap().is_none());
    }
}
