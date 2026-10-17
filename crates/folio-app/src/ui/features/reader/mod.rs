//! 阅读功能：一套渲染、三种摆法。
//!
//! 一纵切切成几块，各管一段：
//!
//! - [`session`] —— 一个会话一个实体：状态（模式、滚动/页号、外壳占位）与命令入口。
//! - [`book`] —— 当前打开的书：章节表、当前章、本章的排版状态。
//! - [`document`] —— 一章的 XHTML → `Layout` → 切片（层叠、断行、分片、坐标映射）。
//! - [`measure`] —— `Measure` 的 gpui 实现与 [`measure::gpui_font`]（度量与绘制同源）。
//! - [`book_fonts`] —— 这本书的内嵌字体别名表（声明名 → 设备家族名）。
//! - [`modules`] —— 会话的三个功能模块：导航 / 位置 / 目录。
//! - [`scene`] —— 排版结果 → 绘制单元（`GlyphRun` + 图片）。
//! - [`image_atlas`] —— 书里的图与 GPU 图集之间那层**会回收**的缓存（谁持有像素、什么时候放掉）。
//! - [`image_viewer`] —— 把一张图摊满整屏看：状态、纯几何与那一层浮层。
//! - [`view`] —— `impl Render`：把会话状态摆成元素、接住滚轮。
//! - [`selection_menu`] —— 选中的就地菜单（图标浮条 + 颜色胶囊）与它的摆位判断。
//! - [`note_editor`] —— 选中之后就地为这一段写笔记的那张便签。
//! - [`share_card`] —— 把选中的那一段连同书名与位置渲成一张图递给系统分享面板（离屏那一趟在 `platform` 那侧）。
//! - [`turn`] —— 翻页决策（连续 / 分页 / 双页、章界），纯函数，宿主直接测。
//! - [`page`] —— 盖在整窗上的那一屏：干净态两条小字 + 浮出的顶栏与底栏（挂在它自己那条子栈的栈底）。
//! - [`chrome`] —— 纯判断（分区、面板档位、模式退档、进度念法），无 gpui，宿主测试直接跑。
//! - [`toc_tree`] —— 目录树的纯判断（摊平、折叠、当前行、搜索），同上。
//! - [`toc_panel`] —— 目录那一屏自己那个实体（弹层、书头、搜索、尺寸表都在它手里），阅读页只留句柄。
//! - [`bands`] —— 干净态那两条读数带（顶上章名、底下位置与进度）。
//! - [`paint`] —— 字形级绘制元素（`shape_line` / `paint_glyph`）。
//!
//! 这里**没有 `cfg`**：以前桌面是「`TextView` + 虚拟列表」、移动是「自绘字形」两套实现，
//! 同一本书的行走两条不同的路；合成一套之后，平台上只剩两件事不同，而两件都不在这一层 ——
//!
//! - 外壳占的是宽还是高：[`Reserve`] 两个轴都带着，谁用哪个由模式自己决定。
//! - 正文家族在这台设备上解析不解析得出来：`crate::ui::host`。

mod bands;
mod book;
mod book_fonts;
mod chrome;
pub mod document;
mod fixed;
mod image_atlas;
mod image_viewer;
mod measure;
mod modules;
mod note_editor;
mod page;
mod paint;
mod perf;
mod scene;
mod selection_menu;
mod session;
mod share_card;
mod toc_panel;
mod toc_tree;
mod turn;
mod view;

pub use chrome::{Arrow, Reserve};
pub use page::ReadingPage;
pub use session::ReaderSession;
