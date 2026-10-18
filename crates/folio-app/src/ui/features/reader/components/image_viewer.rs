//! 图片放大查看：把一张图摊满整屏，能缩放、能拖着看、能左右走到整本书的下一张，
//! 顶栏那一行给分享 / 下载 / 「第几张 / 共几张」/ 定位原文 / 关闭。
//!
//! **关闭只走两条路**：右上角那颗「关闭」，与系统返回（移动端的返回键 / 边缘滑动，
//! 外壳的 `dismiss_chrome` 先收这一层）。点空白**不**关 —— 缩着缩着松手的一下不该
//! 把读者退出去，而移动端没有右键撤销这一档。
//!
//! 手势与键鼠两端的含义：
//!
//! - **触屏**：手指平移（平台把它翻成 `ScrollWheelEvent`）在铺满那一档横扫切一张、
//!   放大之后两轴平移，而**一趟手势只切一张**（见 [`Looking::swiped`]）；捏合缩放。
//! - **桌面**：滚轮缩放（横向滚轮切图）、按住拖动平移、左右方向键切图、`ctrl`/`cmd` +
//!   滚轮也缩放。轮盘与手指走的是同一个事件通道，判据在 `crate::ui::host::has_mouse_wheel`。
//!
//! 入口有两条，各按内容的形态定：
//!
//! - **可重排正文里的插图**：单击开。那一击原本只是「开合浮层」（`chrome::zone` 中间
//!   那一档），让给放大更值钱；图外的字与图行之间的空白照旧开合浮层、照旧翻页。
//! - **定版式那一页**（CBZ / 逐页 SVG 漫画）：页几乎占满屏，单击就是翻页本身，所以
//!   移动端**长按**开、桌面**单击图内**开（桌面没有长按这一档，判据在
//!   `crate::ui::host::supports_long_press`；翻页让给页外的留白与音量键）。
//!
//! **左右那一趟走的是整本书**：[`ImageSlot`] 那一列由会话在开书时折一次
//! （可重排那一支读 `chapters.images_json`，定版式那一支读页表），计数、滑动、
//! 定位原文三个都从同一列里取，所以「第 3 张」在屏幕上、在滑动里、在跳转里是同一张。
//! 横向那一趟**只在铺满那一档切图**，放大之后横向是平移着看细节。
//!
//! 状态挂在一个独立实体上，而不是塞进 [`ReaderSession`]：这一层由阅读页画在最上层
//! （要盖住顶栏底栏与那两块弹层），而 gpui 里非视图实体的 `cx.notify()` 只有订阅方
//! 收得到 —— 阅读页 `cx.observe` 它，于是手势改的是同一个数、重画的是整屏，
//! 形状与 `widgets::BottomSheetState` 一致。
//!
//! 清晰度就是正文那份像素：`ui::foundation::images` 把字节缩到长边 2048 才交出去
//! （sprite atlas 的纹理上限，超了手机上静默画空），所以放大倍率的上限按「一个源像素
//! 对一个 CSS 像素」算，不多给一分糊。摊开看的那一张与正文里的是**同一份像素**
//! （[`crate::ui::features::reader::model::image_atlas`] 按容器路径存着），放大不额外占一格图集；左右那两张由这一层
//! 每帧 [`ImageAtlas::prepare`] 预取，解好了才上屏，所以横扫过去看到的是下一张而不是黑。
//! 分享与递去相册的那份**不是**这一份像素，而是容器里的原始字节（见 `export_image`）。
//!
//! 系统那两条栏不在这一层管：书挂着的那一屏本来就沉浸（`shell::immersive_nav` ⇒
//! Android 藏状态栏与导航栏、桌面窗口真全屏），而这一层向上长回 `top_inset` 那一段
//! （阅读页那一层的盒从系统栏下面起，见 `shell::reading_top_inset`），黑底因此盖到
//! 屏幕最顶。要让开的只有顶上那条**挖孔**的 inset —— 按钮摆在它下面。
//!
//! 这一层**画在**哪儿的宽高由 `canvas` 的绘制回调回写（与 `selection.rs` 同一条路）：
//! 推算出来的数与安全区、外壳内缩都有关，量出来的才是准的。回写落在 `Cell` 而不是实体里
//! —— 绘制期改实体是另一码事（见 `selection.rs` 里那条「监听只活到本帧」的纪律）。

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::{Icon, h_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EntityId, FocusHandle,
    InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ObjectFit, ParentElement, PinchEvent, ScrollDelta,
    ScrollWheelEvent, SharedString, StatefulInteractiveElement as _, Styled, StyledImage,
    TestSupportExt as _, TouchPhase, Window, canvas, div, hsla, img, px,
};

use crate::ui::features::reader::model::session::{ReaderSession, Source};
use crate::ui::foundation::images::DecodeRequest;
use crate::ui::services::l10n::{tf, tr};

use crate::ui::features::reader::model::image_atlas::ImageAtlas;

/// 整本书那一列图里的一条。查看器左右滑走的就是这一列，「第 n 张 / 共 m 张」念的
/// 也是它 —— 一列之内**每张都画得出来**（建列时解过一遍，见 `Book::build_image_sequence`）。
///
/// `Debug` 是手写的：`request` 里那条取字节的链子是 `dyn Fn`，derived 打不出来，
/// 而断言失败时最想看的恰恰是 `href` 与出处那两格。
#[derive(Clone)]
pub struct ImageSlot {
    /// 解析后的**容器路径**：取字节、进图集查格子都按它问。
    pub href: String,
    /// 取字节 + 解码那趟活儿，键是 `<本书 token>/<容器路径>`。
    ///
    /// 存这一格而不是存像素：解好的那份归图集管（它会回收），查看器每帧去问一次
    /// （见 [`crate::ui::features::reader::model::image_atlas::ImageAtlas::painted`]）。摊开看着的那几张因此与
    /// 正文里的是同一份像素，切到下一张也不多占一格。
    pub request: DecodeRequest,
    /// **固有**像素尺寸（`(0, 0)` = 表里没写：SVG 与 manifest 缺宽高那些）。
    /// 放大上限按它算，见 [`max_scale`]。
    pub natural: (f32, f32),
    /// manifest 声明的媒体类型（`image/png` 那一格）。分享与下载递给系统时按它给
    /// intent 的类型；表里没写时是空串，由系统自己认。
    pub mime: String,
    /// 递给系统的那个文件名（带扩展名）。
    ///
    /// 名字里带上「这一列里的第几张」：桌面那一条落 `~/Downloads`，同一本书里两张同名
    /// 的图（`image1.png` 在画册书里成打地出现）若不带这个前缀，第二张会把第一张盖掉，
    /// 而读者以为只存了一张。
    pub name: String,
    /// 这一张在正文里的出处。「定位原文」跳的就是它。
    pub place: Place,
}

/// 一张图的出处。两支内容各有各的坐标轴，硬合成一格就会有一支在说谎。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// 可重排正文：第 `chapter` 章、第 `offset` 个**可见字符**（与 `Locator` 同一根轴）。
    /// 这个偏移是导入那一趟扫出来的（`content::scan::ImageUse`），不经排版。
    Reflow { chapter: usize, offset: usize },
    /// 定版式：第 `page` 页。那一支每张图就是一整页，没有「页里的第几个字」这一说。
    Fixed { page: usize },
}

impl std::fmt::Debug for ImageSlot {
    /// 打得出的是「这是哪一张、在哪儿」；那条链子不进输出（它没有可读的形状）。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImageSlot")
            .field("href", &self.href)
            .field("natural", &self.natural)
            .field("mime", &self.mime)
            .field("name", &self.name)
            .field("place", &self.place)
            .finish_non_exhaustive()
    }
}

impl PartialEq for ImageSlot {
    /// 那两条 `assert_eq!` 要比的是「同一张图、同一个出处」，键与链子不算身份：
    /// 同一本书重开一次是一个新 token，比出来是「不等」毫无意义。
    fn eq(&self, other: &Self) -> bool {
        self.href == other.href
            && self.natural == other.natural
            && self.mime == other.mime
            && self.name == other.name
            && self.place == other.place
    }
}

/// 递给系统的那个文件名：`{第几张}_{原名字}.{扩展名}`。
///
/// 那个序号前缀是必要的，不是好看：一本画册里 `image1.png` 这种重名图成打地出现，
/// 而桌面上这一条落 `~/Downloads`、Android 上落相册的 `Pictures/Folio`，两处都是
/// 「同名的后一张把前一张盖掉」—— 读者的文件名归系统管，我们只负责递一个不撞的。
///
/// 扩展名先按 mime 给，mime 是空（表里没写）才回退到容器路径自己那一条后缀：
/// EPUB3 里 `fallback=` 那一族就是「键叫 .svg、字节其实是 png」（见
/// `content::format::epub::decodable_fallback`），按路径给后缀会递错格式。
pub(in crate::ui::features::reader) fn file_name(index: usize, href: &str, mime: &str) -> String {
    let base = href.rsplit('/').next().unwrap_or(href);
    // 去后缀用**最后一个**点：`chapter1.2.png` 的名字本身就带点，按第一个点切会把
    // 「.2」当成后缀丢掉。开头就是点的那种（`.hidden`）没有后缀可去，整串当名字。
    let stem = match base.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => base,
    };
    let ext = match mime.split(';').next().unwrap_or("").trim() {
        mime if mime.eq_ignore_ascii_case("image/jpeg") => Some("jpg"),
        mime if mime.eq_ignore_ascii_case("image/png") => Some("png"),
        mime if mime.eq_ignore_ascii_case("image/gif") => Some("gif"),
        mime if mime.eq_ignore_ascii_case("image/webp") => Some("webp"),
        mime if mime.eq_ignore_ascii_case("image/bmp") => Some("bmp"),
        mime if mime.eq_ignore_ascii_case("image/svg+xml") => Some("svg"),
        _ => None,
    }
    .or_else(|| {
        // mime 没给或是别的一类：认容器路径自己那一条后缀，只收这一小批常见值。
        let tail = base.rsplit('.').next()?;
        ["png", "jpg", "jpeg", "gif", "webp", "bmp", "svg"]
            .iter()
            .map(|known| (*known, tail.eq_ignore_ascii_case(known)))
            .find(|(_, hit)| *hit)
            .map(|(known, _)| known)
    });
    match ext {
        Some(ext) => format!("{index:03}_{stem}.{ext}"),
        None => format!("{index:03}_{stem}"),
    }
}

/// 缩小的下限：`1.0` 就是等比铺满那一格，再小没有意义。
const MIN_SCALE: f32 = 1.0;
/// 固有尺寸未知（SVG、manifest 没写宽高）时给的可放倍数。
///
/// 这时候算不出「一个源像素对一个 CSS 像素」在哪，只能给一个不会让读者撞上硬顶的经验值。
const UNKNOWN_MAX_SCALE: f32 = 3.0;
/// 桌面滚轮一格折算多少 CSS 像素（`ScrollDelta::Lines` 那一种）。
const WHEEL_STEP: f32 = 24.0;
/// `ctrl` + 滚轮一格的放大倍数。触控板的捏合在 X11 上要 compositor 支持，这条是兜底。
const ZOOM_STEP: f32 = 0.15;
/// 按下之后移动多少算「在拖」（CSS 像素）。手指在图上抖一下不该当成一次手势。
const DRAG_SLOP: f32 = 8.0;
/// 铺满那一档上，横向攒过多少 CSS 像素算「横扫切一张」。
///
/// 比 [`DRAG_SLOP`] 大一个量级是有意的：8 像素判定「这不是一个手势」，而这一个判定
/// 「这是要换一张」。中间那一段留给读者改主意 —— 拖一半觉得不对，往回拖就退回去了。
const SWIPE_TRIGGER: f32 = 60.0;
/// 顶栏那一行每颗按钮的宽（逻辑像素）：图标 + 两三字标签都要放得下。
///
/// 44 是这一路一直用的最小可点目标（正文的分区、选中菜单那一行、目录那几颗都是它）：
/// 摊开看图的正是手指头在屏幕上点得最随便的场合，按钮再小就要读者去瞄准了。
const CONTROL_W: f32 = 52.0;
/// 那颗的高：图标 20 + 2 的缝 + 一行 10 的字，再留一点上下呼吸。
const CONTROL_H: f32 = 44.0;
/// 图标本身画多大。
const CONTROL_ICON: f32 = 20.0;
/// 那颗下面的标签字号。比图标小一档：它是提示，不是要被读的主体。
const CONTROL_LABEL: f32 = 10.0;
/// 那一行距**安全区下沿**再往下多少（逻辑像素）。
const CONTROL_TOP: f32 = 6.0;
/// 那一行左右各留多少（免得最边上那颗压在屏幕圆角与挖孔那一侧）。
const CONTROL_SIDE: f32 = 12.0;
/// 计数那一格的字号。比图标小一档：它是「现在第几张」的说明，不是要点的目标。
const COUNTER_TEXT: f32 = 14.0;
/// 那一行的墨色：黑底上的白。写死不取主题色 —— 这一层是盖在书上的一层黑，
/// 跟着主题换亮换暗会把它读成「书自己的一屏」。
const CONTROL_INK: gpui_kit::Hsla = hsla(0.0, 0.0, 1.0, 1.0);

/// 一张图等比缩进 `area` 后的尺寸（居中由摆位那边负责，这里只算大小）。
///
/// 固有尺寸未知（0）时铺满那一格：与定版式页表缺宽高时同一条兜底（`view.rs` 里 `aspect`
/// 那条式子）——猜视口比例总比算不出框强。视口还没量出来时给 0，那一帧不画。
pub fn fit_size(area: (f32, f32), natural: (f32, f32)) -> (f32, f32) {
    let (area_w, area_h) = area;
    if area_w <= 0.0 || area_h <= 0.0 {
        return (0.0, 0.0);
    }
    let (natural_w, natural_h) = natural;
    if natural_w <= 0.0 || natural_h <= 0.0 {
        return (area_w, area_h);
    }
    let k = (area_w / natural_w).min(area_h / natural_h);
    (natural_w * k, natural_h * k)
}

/// 放大倍率的上限：图按**原像素**铺开那一刻（一个源像素对一个 CSS 像素）。
///
/// 再往上就是插值出来的糊，而放大查看的用途正是看清细节 —— 顶在原像素处，读者拖到顶
/// 就停住，比让他一路放到满屏马赛克再自己猜「够了没有」诚实。
/// 适配框不保比例（尺寸未知那条兜底）时按宽这一轴算，那是横向细节的瓶颈。
pub fn max_scale(fit: (f32, f32), natural: (f32, f32)) -> f32 {
    if fit.0 <= 0.0 || natural.0 <= 0.0 || natural.1 <= 0.0 {
        return UNKNOWN_MAX_SCALE;
    }
    (natural.0 / fit.0).max(MIN_SCALE)
}

/// 一轴上的平移夹取：图在这一轴铺得下视口，就不许拖出底色；铺不满，就钉死在中间。
///
/// `room` 因此是「缩放后的尺寸比视口多出来那一半」。夹在两端之间，读者拖到边就停住，
/// 而不是把图推走留下一片黑。
pub fn clamp_pan(pan: f32, scaled: f32, area: f32) -> f32 {
    let room = ((scaled - area) / 2.0).max(0.0);
    pan.clamp(-room, room)
}

/// 一张正被放大了看的图。`ImageViewer::looking` 是 `None` 就是这一层没开。
#[derive(Clone)]
struct Looking {
    /// 这张图在**整本书那一列**里的下标。像素、固有尺寸、文件名、出处都由它查
    /// （[`ImageViewer::sequence`]），所以这一层自己不留任何一份图的副本。
    index: usize,
    scale: f32,
    /// 图心相对 `area` 中心的位移（CSS 像素），已经过 [`clamp_pan`]。
    pan: (f32, f32),
    /// 铺满那一档上攒着的横向位移（CSS 像素）。过 [`SWIPE_TRIGGER`] 切一张并清零。
    ///
    /// 攒而不是「一次性位移判阈值」：手机上一次横扫的增量是逐帧来的（惯性那段还在续），
    /// 单帧那一下未必够得着阈值，而攒过头又会一次滑出两张 —— 所以过线就清。
    swipe: f32,
    /// 这一趟手势已经切过一张了。
    ///
    /// 手机上一次横扫从按下到惯性尾巴走完是一条**手势**（平台逐帧派发 `Scroll`，见
    /// [`begin_swipe`]），而一次横扫该只换一张 —— 不拦的话一条 200 像素的横扫加上
    /// 惯性尾巴会在半秒内滑过四五张，读者只能看到最后一张。
    ///
    /// 只在 [`begin_swipe`] 里清（新一趟手指按下那一刻）。
    swiped: bool,
}

/// 没有书可列时那一列是空的：兜底那一屏、以及还没开书的会话。
pub(in crate::ui::features::reader) fn empty_image_sequence() -> Arc<[ImageSlot]> {
    Arc::from(Vec::new())
}

/// 阅读页持有的查看器状态。
#[derive(Default)]
pub struct ImageViewer {
    looking: Option<Looking>,
    /// 这一层画出来的宽高，由 [`overlay`] 里那块 `canvas` 每帧回写。
    ///
    /// 开那一层的那一次先拿窗口视口顶上（[`ImageViewer::open`] 的入参），于是第一次
    /// 落位就不差多少；真尺寸那一帧回来再校正。
    area: Rc<Cell<(f32, f32)>>,
    /// 桌面鼠标拖动的起点：按下时的指针位置 + 那一刻的 `pan`。
    drag: Option<((f32, f32), (f32, f32))>,
    /// 整本书的那一列图，开这一层时带进来，之后只读。
    ///
    /// 带进来而不是回会话去问：这一层由阅读页画，而阅读页手里那份 `Arc` 与会话的
    /// 是同一份（`Arc` 的克隆只是引用计数），于是绘制帧里一个借用都不必向会话要。
    sequence: Arc<[ImageSlot]>,
    /// 桌面鼠标这一趟已经走过的横向位移（CSS 像素）。`move_drag` 每次收到的是
    /// 「按下点到这里」的**总**位移，攒切图阈值要的是增量，所以记下上一次那一个。
    swipe_walked: Cell<f32>,
}

impl ImageViewer {
    pub fn is_open(&self) -> bool {
        self.looking.is_some()
    }

    /// 第 `index` 张的那一条；越界给 `None`（列是空的、或这本书没有图）。
    pub fn slot(&self, index: usize) -> Option<&ImageSlot> {
        self.sequence.get(index)
    }

    /// 正在看的那张图的**固有尺寸**；没开着时 `None`。
    ///
    /// 这一格值得单独问得出：排版那一路有没有把尺寸带过来，错了没有任何可见症状 ——
    /// 查看器会静默退到经验上限（[`UNKNOWN_MAX_SCALE`]），读者只看得见「放不大」，
    /// 而那条线索指向的是排版，不是查看器。
    ///
    /// 只由测试问：界面的这一格在屏幕上看不出来，能钉住它的就是这一问。
    #[cfg(test)]
    pub(in crate::ui::features::reader) fn seen_natural(&self) -> Option<(f32, f32)> {
        self.looking
            .as_ref()
            .and_then(|looking| self.slot(looking.index))
            .map(|slot| slot.natural)
    }

    /// 打开看这一列里的第 `index` 张。`viewport` 是发起那一击时的窗口视口，
    /// 只当第一帧的尺寸用；`sequence` 是整本书那一列（会话开书时折好的那一份）。
    pub fn open(&mut self, index: usize, viewport: (f32, f32), sequence: Arc<[ImageSlot]>) {
        self.area.set(viewport);
        self.sequence = sequence;
        self.looking = Some(Looking {
            index,
            scale: MIN_SCALE,
            pan: (0.0, 0.0),
            swipe: 0.0,
            swiped: false,
        });
        self.drag = None;
    }

    pub fn close(&mut self) {
        self.looking = None;
        self.drag = None;
    }

    /// 这一列里挪一张：`delta` 是 ±1（越界不动，第一张再往左、最后一张再往右都不动）。
    ///
    /// 换了张就把倍率与位移清回铺满那一档：读者的意图是「看下一张」，
    /// 上一张的放大与平移跟着跨过去只会让下一张看起来是歪的、糊的。
    pub fn step(&mut self, delta: isize) -> bool {
        let Some(looking) = self.looking.as_mut() else {
            return false;
        };
        let len = self.sequence.len();
        if len == 0 {
            return false;
        }
        let target = looking.index as isize + delta;
        if target < 0 || target as usize >= len {
            return false;
        }
        looking.index = target as usize;
        looking.scale = MIN_SCALE;
        looking.pan = (0.0, 0.0);
        looking.swipe = 0.0;
        true
    }

    /// 放大 `delta`（[`PinchEvent::delta`] 那一种比例增量：0.1 = 放大一成）。
    ///
    /// 以图的中心为锚，不做「绕手指那一点」的锚点缩放：看图要清的是局部细节，拖一下
    /// 就把中心挪过去了，绕锚点那套多出来的状态值不回这个体验。
    fn zoom_by(&mut self, delta: f32) -> bool {
        // 先把手上那两样取出来再把 `self.looking` 可变借走：`area` 是个 `Cell`、
        // 固有尺寸在整本书那一列里，两处都要借 `self`，与那一步不能同时。
        let area = self.area.get();
        let Some(index) = self.looking.as_ref().map(|looking| looking.index) else {
            return false;
        };
        let natural = self.natural_of(index);
        let fit = fit_size(area, natural);
        let Some(looking) = &mut self.looking else {
            return false;
        };
        let before = looking.scale;
        let limit = max_scale(fit, natural);
        looking.scale = (before * (1.0 + delta)).clamp(MIN_SCALE, limit);
        looking.pan = clamp_pan_to(looking.pan, fit, looking.scale, area);
        (looking.scale - before).abs() > f32::EPSILON
    }

    /// 平移一段增量（CSS 像素）。手指往下 ⇒ 图也往下 —— 与正文那条 `scroll_y -= dy`
    /// 是同一个方向的两次表达（正文动的是视口，这里动的是图）。
    fn pan_by(&mut self, delta: (f32, f32)) -> bool {
        let area = self.area.get();
        let Some(index) = self.looking.as_ref().map(|looking| looking.index) else {
            return false;
        };
        let natural = self.natural_of(index);
        let fit = fit_size(area, natural);
        let Some(looking) = &mut self.looking else {
            return false;
        };
        let before = looking.pan;
        looking.pan = clamp_pan_to(
            (looking.pan.0 + delta.0, looking.pan.1 + delta.1),
            fit,
            looking.scale,
            area,
        );
        (looking.pan.0 - before.0).abs() > 0.5 || (looking.pan.1 - before.1).abs() > 0.5
    }

    /// 放大上限要按**当前这一张**的固有尺寸算，所以每回都要从那一列里取一次。
    fn natural_of(&self, index: usize) -> (f32, f32) {
        self.slot(index)
            .map(|slot| slot.natural)
            .unwrap_or((0.0, 0.0))
    }

    /// 攒一段横向位移，够切一张就交出方向（±1），否则 `None`。
    ///
    /// **只在铺满那一档切**（与他处相册同一口径）：放大之后横向拖的意思是平移着看细节，
    /// 那时切图会把读者正盯着的那一小块换掉。而放大着的时候 `clamp_pan` 本来就有可拖的
    /// 余地，两条不会撞。
    ///
    /// **一趟手势只切一张**（见 [`Looking::swiped`]）：手机上一次横扫加上惯性尾巴会
    /// 逐帧派发很多条增量，不拦就是半秒滑过四五张。
    ///
    /// 手指往右 ⇒ 看前面那张（与正文 `scroll_y -= dy` 那一条方向感同源）。
    fn swipe_by(&mut self, dx: f32) -> Option<isize> {
        let looking = self.looking.as_mut()?;
        if looking.scale > MIN_SCALE + f32::EPSILON || looking.swiped {
            return None;
        }
        looking.swipe += dx;
        if looking.swipe.abs() < SWIPE_TRIGGER {
            return None;
        }
        let delta = if looking.swipe > 0.0 { -1 } else { 1 };
        looking.swipe = 0.0;
        looking.swiped = true;
        Some(delta)
    }

    /// 一趟手势从这里起：调用方在平台报 `TouchPhase::Started` 那一帧叫它
    /// （见 `overlay` 的 `on_scroll`）。
    ///
    /// 手机上一次横扫从按下到惯性尾巴走完是一条手势：平台逐帧派发 `Scroll`，
    /// 首帧带 `Started`，抬指那一帧带 `Ended`，之后惯性那段又是 `Moved` 直到
    /// 最后一条 `Ended`（见 `gpui-pre` 的 `gestures.rs`）。不在这里把
    /// [`Looking::swiped`] 还回来，读者下一次横扫（上一趟还停在惯性尾巴上）就切不动了。
    fn begin_swipe(&mut self) {
        if let Some(looking) = self.looking.as_mut() {
            looking.swipe = 0.0;
            looking.swiped = false;
        }
    }

    /// 桌面真鼠标滚轮的那一下：一次滚轮事件就是一格手势，上一格那面旗挡不住下一格。
    ///
    /// 只清 [`Looking::swiped`]，不清 [`Looking::swipe`]：一次滚一格往往不够阈值，
    /// 那一段要留着让连续几格攒起来。
    fn wheel_step(&mut self, dx: f32) -> Option<isize> {
        if let Some(looking) = self.looking.as_mut() {
            looking.swiped = false;
        }
        self.swipe_by(dx)
    }

    /// 按下：记下起点，先不当它是拖。
    fn begin_drag(&mut self, at: (f32, f32)) {
        self.drag = self.looking.as_ref().map(|looking| (at, looking.pan));
    }

    /// 拖到 `at`。过阈值才算「在拖」（`DRAG_SLOP` 以下当手指抖了一下，不当手势）。
    ///
    /// 铺满那一档、且横向为主的这一趟**不是平移**（那一带本来就没有可平移的余地），
    /// 而是攒进切图那一条，见 [`ImageViewer::swipe_by`]；纵向与放大时的两轴仍是平移。
    fn move_drag(&mut self, at: (f32, f32)) -> bool {
        let Some((start, start_pan)) = self.drag else {
            return false;
        };
        let delta = (at.0 - start.0, at.1 - start.1);
        if delta.0.hypot(delta.1) < DRAG_SLOP {
            return false;
        }
        let area = self.area.get();
        let Some(looking) = &self.looking else {
            return false;
        };
        let natural = self.natural_of(looking.index);
        let at_fit = looking.scale <= MIN_SCALE + f32::EPSILON;
        let horizontal = at_fit && delta.0.abs() > delta.1.abs();
        if horizontal {
            // 攒的是**增量**：拖到一半往回拖要退得回去，否则来回抖几下就切出好几张。
            let walked = delta.0 - self.swipe_walked.get();
            self.swipe_walked.set(delta.0);
            return match self.swipe_by(walked) {
                Some(step) => self.step(step),
                None => true,
            };
        }
        let Some(looking) = &mut self.looking else {
            return false;
        };
        looking.pan = clamp_pan_to(
            (start_pan.0 + delta.0, start_pan.1 + delta.1),
            fit_size(area, natural),
            looking.scale,
            area,
        );
        true
    }

    fn end_drag(&mut self) {
        self.drag = None;
        self.swipe_walked.set(0.0);
    }
}

/// 一对位移按适配框与倍率夹回可拖的范围。两轴各夹各的（判据在 [`clamp_pan`]）。
fn clamp_pan_to(pan: (f32, f32), fit: (f32, f32), scale: f32, area: (f32, f32)) -> (f32, f32) {
    (
        clamp_pan(pan.0, fit.0 * scale, area.0),
        clamp_pan(pan.1, fit.1 * scale, area.1),
    )
}

impl ReaderSession {
    /// 挂上阅读页持有的那一个查看器句柄。会话只认这个句柄：状态是同一份，
    /// 而重画的是那一屏（构造时给一次，见 `page::ReadingPage::new`）。
    pub(in crate::ui::features::reader) fn set_image_viewer(&mut self, viewer: Entity<ImageViewer>) {
        self.image_viewer = Some(viewer);
    }

    /// 整本书的那一列图。查看器开的时候把这一份 `Arc` 带走。
    pub(in crate::ui::features::reader) fn image_sequence(&self) -> &Arc<[ImageSlot]> {
        &self.image_sequence
    }

    /// 正文里画出来的那一张（按图集的键问）在这一列里的第几条。
    ///
    /// 键是 `<本书 token>/<容器路径>`，建列与建场景用的是同一份 `BookImages`，
    /// 所以两边算出来的键**必须**相等 —— 不等就是那条 `<img src>` 的解析有两本账。
    ///
    /// 同一张图在这一章里被用两次时交出**第一条**：像素是同一份，读者看到的画面对得上，
    /// 只是计数与「定位原文」的落点指到第一次那一处。认第二处要的是「点在的是哪一行」，
    /// 而那一格在点击事件里没有对应物（`VisibleImage` 带的是摆位，不是清单序号）。
    pub(in crate::ui::features::reader) fn image_index_of_key(&self, key: &str) -> Option<usize> {
        self.image_sequence
            .iter()
            .position(|slot| slot.request.key == key)
    }

    /// 重算那一列：开书（`with_document`）与换书（`open_book`）两条路都要过，
    /// 漏一条的症状是图片查看器那一列空的 —— 正文里的图照样点得开，只是没有左右张。
    ///
    /// 折一列的钱是「章数 + 图片条目数」次字符串拼接与哈希查表，**一次都不排版、
    /// 一次都不碰字节**，《未来简史》那种 55 张的书实测在毫秒以下（见会话侧调用点）。
    pub(in crate::ui::features::reader) fn rebuild_image_sequence(&mut self) {
        self.image_sequence = match &self.source {
            Source::Book(book) => book.build_image_sequence(),
            Source::Fixed(fixed) => fixed.build_image_sequence(),
            Source::Sample(_) => empty_image_sequence(),
        };
    }

    /// 关掉放大查看那一层。没挂着句柄（单独的会话测试）时什么都不做。
    pub(in crate::ui::features::reader) fn close_image_viewer(&mut self, cx: &mut Context<Self>) {
        if let Some(viewer) = &self.image_viewer {
            viewer.update(cx, |viewer, cx| {
                viewer.close();
                cx.notify();
            });
        }
    }

    /// 「分享这一张」：把原图字节递给系统的分享面板。
    pub fn share_image(&mut self, index: usize, cx: &mut Context<Self>) {
        self.export_image(index, cx, crate::ui::host::share_image);
    }

    /// 「下载这一张」：把原图字节存进系统相册（桌面落到下载目录）。
    pub fn save_image(&mut self, index: usize, cx: &mut Context<Self>) {
        self.export_image(index, cx, crate::ui::host::save_image);
    }

    /// 那两条递给系统的路共用的那一半：取字节、然后交出去。
    ///
    /// 递的是**容器里的原始字节**，不是摊开看着的那一份像素：那一份是按
    /// `images::MAX_IMAGE_EDGE` 缩过的、也可能是 EPUB3 `fallback=` 换过的位图，
    /// 拿它去分享就是把读者书里的原图降一档给他。
    ///
    /// 解压不在 UI 线程上：一页漫画是一两兆的 JPEG，inflate 那一下是看得见的卡，
    /// 而按下按钮的人期待的是「系统面板过一会儿自己出来」。
    /// 读不到字节只写一行日志 —— 一次导出没成不该把阅读页拖退（`host` 两侧同一立场）。
    fn export_image(&self, index: usize, cx: &mut Context<Self>, deliver: ExportTarget) {
        let Some(slot) = self.image_sequence.get(index) else {
            return;
        };
        let source = match &self.source {
            Source::Book(book) => book.image_source(),
            Source::Fixed(fixed) => fixed.image_source(),
            // 兜底那一行没有容器，也就没有字节可递。
            Source::Sample(_) => return,
        };
        let href = slot.href.clone();
        let mime = slot.mime.clone();
        let name = slot.name.clone();
        let logged = href.clone();
        let read = cx.background_spawn(async move { source(&href) });
        cx.spawn(async move |this, cx| {
            let Some(bytes) = read.await else {
                tracing::warn!("图片导出: {logged} 从容器里读不出字节");
                return;
            };
            // 递给系统那一步回到主执行器上做：`host` 两侧要的是一个 `&App`（它们递的是
            // 「现在就把面板开出来」这个动作，不必再往后排一趟）。会话还活着才有这一帧，
            // 读者在字节读回来之前关掉了书，就什么都不递 —— 那是他改主意了。
            let _ = this.update(cx, |_, cx| deliver(&bytes, &mime, &name, cx));
        })
        .detach();
    }
}

/// 「把一份字节交出去」那一格：分享与下载只差最后递给谁，前面取字节的活儿是同一趟。
type ExportTarget = fn(bytes: &[u8], mime: &str, name: &str, cx: &App);

/// 查看器那一层。没开着时给 `None`，阅读页因此少挂一个子节点。
///
/// `occlude` 是**必须**的，不是修饰：gpui 把一击发给每一个「自己的 hitbox 含有按下点」
/// 的 handler，而正文那层的 hitbox 是整屏的（`page.rs` 里顶栏那条注释记着同一件事）。
/// 不挡住身后，点一下这一层（哪怕是空白处）的同时底下那一屏也收到这一下 ——
/// 就变成「在图上看一眼并且把底下那页翻了」。
/// 连滚动一起收掉也是对的：放大着的时候不该还能把正文滚走。
///
/// 像素**每帧向图集要一次**（[`ImageAtlas::painted`]）并顺手预取左右那两张
/// （[`ImageAtlas::prepare`]）：这一层摊开看的与正文里的是同一份像素，切一张不多占一格，
/// 而横扫过去时那趟解码已经跑完了。
///
/// `view` 是**阅读页**的实体号 —— 解完一张图要重画的是画这一层的人，不是会话。
/// `top_inset` 是顶上那条要让开的高度（挖孔 / 刘海，桌面为 0）：那一行按钮摆在它下面。
///
/// `focus` 是这一层的焦点句柄（归阅读页所有）：开着时把它拿过来，方向键那几条
/// 因此挂得住（见 [`on_key`]）；键盘事件从焦点那一点沿祖先链向上派发，所以按了
/// 顶栏那一行的按钮之后仍然命中这一层。
#[allow(clippy::too_many_arguments)]
pub fn overlay(
    viewer: &Entity<ImageViewer>,
    atlas: &Entity<ImageAtlas>,
    reader: &Entity<ReaderSession>,
    view: EntityId,
    top_inset: f32,
    focus: &FocusHandle,
    window: &mut Window,
    cx: &mut App,
) -> Option<AnyElement> {
    // 只把要用的几样取出来，`read` 的借用在问图集之前就还掉：`atlas.update` 要 `&mut App`。
    let (looking, sequence, area, area_cell) = {
        let read = viewer.read(cx);
        let looking = read.looking.clone()?;
        (
            looking,
            Arc::clone(&read.sequence),
            read.area.get(),
            Rc::clone(&read.area),
        )
    };
    // 焦点在这儿拿（**判过 `looking` 之后**：没开着时这一格一个节点都不挂，
    // 更不能把焦点从别处抢过来）。这一层是盖满整屏的一层，开着时它或它的子孙
    // 该是焦点所在 —— 方向键、顶栏那一行按钮的键盘操作都从那儿长出来。已经在
    // 这一层里面（点了某颗按钮）就不抢回来，读者自己点的位置比我们再抢一次准。
    if !focus.contains_focused(window, cx) {
        focus.focus(window, cx);
    }
    let slot = sequence.get(looking.index).cloned()?;

    let image = atlas.update(cx, |atlas, cx| atlas.painted(&slot.request, view, cx));
    // 左右各预取一张。越界的那一头不取（第一张再往左、最后一张再往右都不动）。
    for delta in [-1isize, 1] {
        let target = looking.index as isize + delta;
        if target < 0 || target as usize >= sequence.len() {
            continue;
        }
        let next = &sequence[target as usize];
        atlas.update(cx, |atlas, cx| atlas.prepare(&next.request, view, cx));
    }

    let natural = slot.natural;
    let fit = fit_size(area, natural);
    let width = fit.0 * looking.scale;
    let height = fit.1 * looking.scale;
    // 位移在这里按**当帧量出来的**宽高再夹一遍：窗口从横变竖之后，存着的那一份可能
    // 已经越界（越界只在拖动时才被夹回来），而露出黑边是读者一眼就能看见的错。
    let (pan_x, pan_y) = clamp_pan_to(looking.pan, fit, looking.scale, area);
    let x = (area.0 - width) / 2.0 + pan_x;
    let y = (area.1 - height) / 2.0 + pan_y;
    let on_pinch = {
        let viewer = viewer.clone();
        move |event: &PinchEvent, window: &mut Window, cx: &mut App| {
            if viewer.update(cx, |viewer, _| viewer.zoom_by(event.delta)) {
                window.refresh();
            }
        }
    };
    let on_scroll = {
        let viewer = viewer.clone();
        // 这一端是不是真鼠标滚轮。每帧读一次而不是事件里现问：`has_mouse_wheel`
        // 读的是应用全局，两处都是同一条路。
        let wheel = crate::ui::host::has_mouse_wheel(cx);
        move |event: &ScrollWheelEvent, window: &mut Window, cx: &mut App| {
            let (dx, dy) = match event.delta {
                ScrollDelta::Pixels(delta) => (f32::from(delta.x), f32::from(delta.y)),
                ScrollDelta::Lines(delta) => (delta.x * WHEEL_STEP, delta.y * WHEEL_STEP),
            };
            let claimed = viewer.update(cx, |viewer, _| {
                if wheel {
                    return on_wheel(viewer, event, dx, dy);
                }
                // 触屏：一趟手势的边界由平台报的相位定（首帧 `Started`、抬指或惯性
                // 尾巴收尾那一帧 `Ended`），一趟里只切一张。
                if event.touch_phase == TouchPhase::Started {
                    viewer.begin_swipe();
                }
                if let Some(step) = viewer.swipe_by(dx) {
                    viewer.step(step)
                } else {
                    // 移动端的一次手指平移被翻成 ScrollWheel（模块头那条口径），
                    // 铺满那一档没攒够阈值就走到这里：`clamp_pan` 夹出来是零位移，
                    // 于是返回 `false`，不逼出多余的一帧。
                    viewer.pan_by((dx, dy))
                }
            });
            if claimed {
                window.refresh();
            }
        }
    };
    let on_down = {
        let viewer = viewer.clone();
        move |event: &MouseDownEvent, _window: &mut Window, cx: &mut App| {
            viewer.update(cx, |viewer, _| {
                viewer.begin_drag((f32::from(event.position.x), f32::from(event.position.y)));
            });
        }
    };
    let on_move = {
        let viewer = viewer.clone();
        move |event: &MouseMoveEvent, window: &mut Window, cx: &mut App| {
            if event.pressed_button != Some(MouseButton::Left) {
                return;
            }
            let dragged = viewer.update(cx, |viewer, _| {
                viewer.move_drag((f32::from(event.position.x), f32::from(event.position.y)))
            });
            if dragged {
                window.refresh();
            }
        }
    };
    let on_up = {
        let viewer = viewer.clone();
        move |_: &MouseUpEvent, _window: &mut Window, cx: &mut App| {
            viewer.update(cx, |viewer, _| viewer.end_drag());
        }
    };
    // 方向键（桌面）：左右切一张。
    //
    // 挂在**这一层**而不是绑一条应用级的 `KeyBinding`：方向键在别处是打字与滚动的，
    // 全局绑上就要与输入框抢（`shell` 里音量键那条注释记的是同一件事）。键盘事件
    // 从焦点沿祖先链向上派发，而这一层开着时它就是焦点所在（顶部那句 `focus`），
    // 所以这一挂只在查看器里生效。
    let on_key = {
        let viewer = viewer.clone();
        move |event: &KeyDownEvent, window: &mut Window, cx: &mut App| {
            let key = event.keystroke.key.as_str();
            // 一次不可变借阅把结果算完再还：`step` 要 `&mut self`，而 `event`
            // 那一头还握着一条 `&str`。
            let moved = viewer.update(cx, |viewer, _| match key {
                "left" => Some(viewer.step(-1)),
                "right" => Some(viewer.step(1)),
                _ => None,
            });
            let Some(moved) = moved else {
                // 不是这一层认的键：原样接着往上派发 —— `escape` 要落到外壳那条 `Back`。
                return;
            };
            cx.stop_propagation();
            if moved {
                window.refresh();
            }
        }
    };

    // ── 顶栏那一行：左边分享与下载，正中计数，右边定位原文与关闭 ──────────────
    //
    // 三组摆 `justify_between`：左右各两颗、宽度相近，中间那一格因此落在屏幕正中附近。
    // 每一颗都要让开 `top_inset` 那条线 —— 沉浸态把状态栏藏了，挖孔还在物理上占着那一条，
    // 压在摄像头上的一颗按钮既看不清也点不准。
    let index = looking.index;
    let total = sequence.len();
    let counter = div()
        .text_size(px(COUNTER_TEXT))
        .text_color(CONTROL_INK)
        .child(tf(
            cx,
            "reader.image_counter",
            &[("index", &(index + 1)), ("total", &total)],
        ));
    let left = h_flex()
        .gap_1()
        .child(control(
            "image-share",
            IconName::Share2,
            tr(cx, "reader.image_share"),
            {
                let reader = reader.clone();
                move |_, cx| {
                    reader.update(cx, |session, cx| session.share_image(index, cx));
                }
            },
        ))
        .child(control(
            "image-download",
            IconName::Download,
            tr(cx, "reader.image_download"),
            {
                let reader = reader.clone();
                move |_, cx| {
                    reader.update(cx, |session, cx| session.save_image(index, cx));
                }
            },
        ));
    // 「定位原文」在两支内容里是同一件事：跳到那张图在正文里的位置。定版式那一支的
    // 「位置」就是那一页（`Place::Fixed`），标签不必跟着换字眼。
    let locate_label = tr(cx, "reader.image_locate");
    let right = h_flex()
        .gap_1()
        .child(control("image-locate", IconName::MapPin, locate_label, {
            let reader = reader.clone();
            move |_, cx| {
                reader.update(cx, |session, cx| session.locate_image(index, cx));
            }
        }))
        .child(control(
            "image-close",
            IconName::X,
            tr(cx, "reader.image_close"),
            {
                let viewer = viewer.clone();
                move |_, cx| {
                    viewer.update(cx, |viewer, cx| {
                        viewer.close();
                        cx.notify();
                    });
                }
            },
        ));

    Some(
        div()
            .id("image-viewer")
            // 宿主的测试要按 id 把点击投到这里。非 `test-support` 构建里这一层是透明的。
            .test_support()
            .occlude()
            .absolute()
            // 顶上那条系统状态栏 / 挖孔（`top_inset`）也归这一层：阅读页那一层的盒从
            // 它下面起（外壳 `reading_overlay` 的 `pt`，见 `shell::reading_top_inset`），
            // 不向上长回去，那一条露出的就是书页的浅底色 —— 读者看到的是「最上面一条白」。
            // 与顶栏那条 `TopBar::overlay` 同一个写法（它也要长回窗口顶边）。
            .top(px(-top_inset))
            .bottom_0()
            .left_0()
            .right_0()
            // 黑底而不是主题底色：书里的插图自带白边的比例千差万别，用页面那一档米白
            // 会让「图的边缘在哪」读不出来。
            .bg(hsla(0.0, 0.0, 0.0, 1.0))
            // 不在这里接点击——只能点「关闭」那一颗或者走系统返回（外壳的
            // `dismiss_chrome` 会先收这一层）关掉这一层。点空白关掉会让读者
            // 「缩着缩着松手一下就退出」，而移动端上没有右键撤销这一档。
            .on_pinch(on_pinch)
            .on_scroll_wheel(on_scroll)
            .on_mouse_down(MouseButton::Left, on_down)
            .on_mouse_move(on_move)
            .on_mouse_up(MouseButton::Left, on_up)
            .track_focus(focus)
            .on_key_down(on_key)
            .children(image.map(|image| {
                div()
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px(width))
                    .h(px(height))
                    .child(img(image).size_full().object_fit(ObjectFit::Contain))
                    .into_any_element()
            }))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .pt(px(top_inset + CONTROL_TOP))
                    .px(px(CONTROL_SIDE))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(left)
                    .child(counter)
                    .child(right),
            )
            .child(
                // 这一格自己不画任何东西，只回写「这一层真落在哪儿」。窗口变高变矮时
                // 靠它逼出第二帧，让摆位按新尺寸重算。
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| {
                        let measured =
                            (f32::from(bounds.size.width), f32::from(bounds.size.height));
                        if area_cell.get() != measured {
                            area_cell.set(measured);
                            window.refresh();
                        }
                    },
                )
                .absolute()
                .inset_0(),
            )
            .into_any_element(),
    )
}

/// 桌面真鼠标滚轮的那一下。返回「这一下真的动了画面吗」—— 拿它决定要不要逼一帧。
///
/// 竖向滚轮是**缩放**，横向滚轮（X11 上 `shift` + 滚轮被平台翻成横轴，见
/// `gpui-pre-linux` 的 `scroll_wheel_event`）是**切图**：两者共用同一条事件通道，
/// 用哪一轴主导来分。`ctrl` / `cmd` + 滚轮也走缩放：触控板的捏合在 X11 上要
/// compositor 报手势事件，这一条总使得上。
///
/// 缩放量按像素比例给（一「格」滚轮 = [`WHEEL_STEP`] 像素 ⇒ [`ZOOM_STEP`] 成），
/// 所以触控板的连续滚动不会一下冲到底。
fn on_wheel(viewer: &mut ImageViewer, event: &ScrollWheelEvent, dx: f32, dy: f32) -> bool {
    if event.modifiers.control || event.modifiers.platform {
        return viewer.zoom_by(if dy > 0.0 { -ZOOM_STEP } else { ZOOM_STEP });
    }
    if dx.abs() > dy.abs() {
        return viewer.wheel_step(dx).is_some_and(|step| viewer.step(step));
    }
    let ratio = (dy.abs() / WHEEL_STEP) * ZOOM_STEP;
    if ratio <= 0.0 {
        return false;
    }
    viewer.zoom_by(if dy > 0.0 { -ratio } else { ratio })
}

/// 顶栏那一行的一颗：图标在上、两三字标签在下（与选中菜单那一行同一个形状）。
///
/// 为什么带字而不是只给图标再加悬停提示：这一层在**触屏**上是主角，触屏没有悬停那一档，
/// 只画图标就得让读者猜；而 gpui 的 `tooltip` 要的是一个浮层视图，移动端那条
/// 「弹层画得出、点不住」的旧账不该在这一层再赌一次。字就是提示。
///
/// `stop_propagation` 是**必须**的，不是修饰：这一层的 hitbox 是整屏的，而 gpui 把
/// 一击发给每一个 hitbox 含按下点的 handler —— 不拦住这一击，按下「分享」就同时是
/// 「分享并且把这一击漏给身后那层」（阅读页上就是跟着翻了页）。
///
/// 墨色是不透白的 [`CONTROL_INK`]：这一层是盖在书上的一层黑，跟着主题换亮换暗会把它
/// 读成「书自己的一屏」。
fn control(
    id: &'static str,
    icon: IconName,
    label: SharedString,
    action: impl Fn(&mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .test_support()
        .w(px(CONTROL_W))
        .h(px(CONTROL_H))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(2.0))
        .cursor_pointer()
        .hover(|style| style.bg(hsla(0.0, 0.0, 1.0, 0.12)))
        .rounded_md()
        .child(
            div()
                .text_size(px(CONTROL_ICON))
                .text_color(CONTROL_INK)
                .child(Icon::new(icon)),
        )
        .child(
            div()
                .text_size(px(CONTROL_LABEL))
                .text_color(CONTROL_INK)
                .child(label),
        )
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            action(window, cx);
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    //! 三条纯几何各自钉一个临界点。为什么钉这几条：它们错了都不会报错，只会让读者
    //! 看见「图跑到屏幕外」或者「放到第三下忽然黑了」—— 那种问题在真机上最难复现。

    use std::sync::Arc;

    use super::{
        ImageSlot, ImageViewer, MIN_SCALE, Place, SWIPE_TRIGGER, UNKNOWN_MAX_SCALE, clamp_pan,
        file_name, fit_size, max_scale,
    };
    use crate::ui::foundation::images::BookImages;

    /// 竖图在横窗口里按**高**顶住，横图反过来：等比，不裁。
    #[test]
    fn fitting_hugs_the_limiting_axis() {
        // 1080×2400 的一页摆在 800×600 的窗口里：高先顶住。
        assert_eq!(fit_size((800.0, 600.0), (1080.0, 2400.0)), (270.0, 600.0));
        // 同一页横过来放：宽先顶住。
        assert_eq!(fit_size((800.0, 600.0), (2400.0, 1080.0)), (800.0, 360.0));
    }

    /// 固有尺寸不知道就铺满那一格；视口还没量出来时不给尺寸，免得算出零框把图藏起来。
    #[test]
    fn unknown_sizes_fill_or_give_nothing() {
        assert_eq!(fit_size((800.0, 600.0), (0.0, 0.0)), (800.0, 600.0));
        assert_eq!(fit_size((0.0, 0.0), (1080.0, 2400.0)), (0.0, 0.0));
    }

    /// 放大上限就是「一个源像素对一个 CSS 像素」那一刻，而且不许低于铺满那一档。
    #[test]
    fn the_zoom_ceiling_is_one_source_pixel_per_css_pixel() {
        // 1080 宽的页铺在 270 宽 ⇒ 还能放 4 倍到原像素。
        assert_eq!(max_scale((270.0, 600.0), (1080.0, 2400.0)), 4.0);
        // 比视口还小的图：铺满已经是放大，不再往上给。
        assert_eq!(max_scale((800.0, 600.0), (600.0, 400.0)), 1.0);
        // 尺寸未知时给一个经验顶，别让读者顶在一个算不出来的数上。
        assert_eq!(max_scale((270.0, 600.0), (0.0, 0.0)), UNKNOWN_MAX_SCALE);
    }

    /// 平移夹取：铺得下就不许拖出底色，铺不满钉在中间（`room` 为 0，两头同值）。
    #[test]
    fn panning_stops_at_the_edges() {
        // 视口 800，图放到 1600：左右各能多拖 400。
        assert_eq!(clamp_pan(500.0, 1600.0, 800.0), 400.0);
        assert_eq!(clamp_pan(-500.0, 1600.0, 800.0), -400.0);
        assert_eq!(clamp_pan(120.0, 1600.0, 800.0), 120.0, "范围里的原样交出");
        // 没放到铺满：任何位移都夹回中间。
        assert_eq!(clamp_pan(120.0, 600.0, 800.0), 0.0);
    }

    /// 文件名：序号前缀 + mime 给的后缀，两者都有存在的理由。
    #[test]
    fn exported_names_carry_the_ordinal_and_the_real_type() {
        // 同一本书里两张都叫 `image1.png`：不带序号，第二张会把第一张盖掉，
        // 而读者以为只存了一张。
        assert_eq!(
            file_name(1, "OEBPS/Images/image1.png", "image/png"),
            "001_image1.png"
        );
        assert_eq!(
            file_name(42, "OEBPS/Images/image1.png", "image/png"),
            "042_image1.png"
        );
        // `fallback=` 那一族：键叫 .svg、字节其实是 PNG —— 后缀按 mime 给，不按路径给。
        assert_eq!(
            file_name(7, "cover.svg", "image/png"),
            "007_cover.png",
            "按容器路径给后缀会递错格式给系统"
        );
        // mime 空（表里没写）才回退到路径自己的后缀；两个都不认识就不给后缀。
        assert_eq!(file_name(3, "a/b/bike.JPEG", ""), "003_bike.jpeg");
        assert_eq!(file_name(3, "a/b/data.xbm", ""), "003_data");
        assert_eq!(file_name(3, "no-slash", "image/gif"), "003_no-slash.gif");
        // 名字里本来就带点的（`chapter1.2.png` 这类）：后缀按**最后一个**点去，
        // 按第一个点去就把「.2」当成后缀，读者的文件名被截掉一截。
        assert_eq!(
            file_name(9, "a/b/chapter1.2.png", "image/png"),
            "009_chapter1.2.png"
        );
    }

    /// 那一列里左右走：阈值之前不切、过了阈值切一张、两头不外溢；
    /// 而**一趟手势只切一张**。
    ///
    /// 钉的是「攒」而不是「单次位移判阈值」：手机上一次横扫的增量是逐帧来的，
    /// 一帧那一两条都不够 60 像素，而惯性尾巴还在续 —— 不攒就切不动，乱攒就一次过两张。
    #[test]
    fn swiping_at_the_fitted_level_walks_the_column() {
        let mut viewer = viewer_with(1, 4);
        // 没到阈值：还是这一张。
        assert_eq!(viewer.swipe_by(-(SWIPE_TRIGGER - 1.0)), None);
        assert_eq!(viewer.looking.as_ref().unwrap().index, 1);
        // 补过阈值：手指往左 ⇒ +1（下一张）。
        assert_eq!(viewer.swipe_by(-1.0), Some(1));
        assert!(viewer.step(1));
        assert_eq!(viewer.looking.as_ref().unwrap().index, 2);
        // 同一回手势里再攒多少都切不动：惯性尾巴不会一口滑过四五张。
        assert_eq!(viewer.swipe_by(-(SWIPE_TRIGGER * 5.0)), None);
        assert_eq!(viewer.looking.as_ref().unwrap().index, 2);
        // 新的一回手势（手指再次按下 ⇒ 平台报 `Started`）：旗还回来。
        viewer.begin_swipe();
        // 反向：手指往右 ⇒ -1（前一张）。
        assert_eq!(viewer.swipe_by(SWIPE_TRIGGER + 1.0), Some(-1));
        assert!(viewer.step(-1));
        assert_eq!(viewer.looking.as_ref().unwrap().index, 1);
        // 两头夹住：第一张再往左、最后一张再往右都不动，也不报错。
        let mut first = viewer_with(0, 4);
        assert!(!first.step(-1), "第一张没有前一张");
        assert_eq!(first.looking.as_ref().unwrap().index, 0);
        let mut last = viewer_with(3, 4);
        assert!(!last.step(1), "最后一张没有下一张");
        assert_eq!(last.looking.as_ref().unwrap().index, 3);
    }

    /// 放大之后横向是平移，不再切图 —— 而这一条要**真的**不切：读者放大看的正是
    /// 那一小块，切图会把它换掉。
    #[test]
    fn a_zoomed_picture_pans_instead_of_switching() {
        let mut viewer = viewer_with(1, 4);
        // 视口 400×600、固有 800×600 ⇒ 铺满那一档的上限是 2.0。
        viewer.area.set((400.0, 600.0));
        assert!(viewer.zoom_by(1.0), "放大一成");
        assert!(viewer.looking.as_ref().unwrap().scale > MIN_SCALE);
        // 同样的横扫：`swipe_by` 在放大那一档恒给 `None`。
        assert_eq!(viewer.swipe_by(-(SWIPE_TRIGGER * 2.0)), None);
        assert_eq!(viewer.looking.as_ref().unwrap().index, 1, "放大着没切图");
        // 缩小回铺满那一档，横向又归切图。
        assert!(viewer.zoom_by(-1.0));
        assert_eq!(viewer.looking.as_ref().unwrap().scale, MIN_SCALE);
        assert_eq!(viewer.swipe_by(-(SWIPE_TRIGGER + 1.0)), Some(1));
    }

    /// 切一张要把倍率与位移清回铺满：上一张的放大与平移跟着跨过去，下一张看起来
    /// 就是歪的、糊的。
    #[test]
    fn switching_resets_the_view_to_the_fitted_level() {
        let mut viewer = viewer_with(0, 3);
        viewer.area.set((400.0, 600.0));
        viewer.zoom_by(1.0);
        viewer.pan_by((40.0, 40.0));
        assert!(viewer.step(1));
        let looking = viewer.looking.as_ref().unwrap();
        assert_eq!((looking.index, looking.scale), (1, MIN_SCALE));
        assert_eq!(looking.pan, (0.0, 0.0), "平移不该跨张带过去");
        assert_eq!(looking.swipe, 0.0, "上一张攒的那一段不该跟着过来");
    }

    /// 一列 `total` 条、开在第 `index` 张的查看器（视口先用窗口值顶着，几何用例自己会改）。
    fn viewer_with(index: usize, total: usize) -> ImageViewer {
        let sequence: Vec<ImageSlot> = (0..total)
            .map(|i| slot(&format!("p{i}.png"), i / 2, i * 7))
            .collect();
        let mut viewer = ImageViewer::default();
        viewer.open(index, (400.0, 600.0), Arc::from(sequence));
        viewer
    }

    /// 那一列里的一条，走的是**生产那条建表路**（`BookImages` → `ChapterImages`），
    /// 所以 `request.key` 与真书里的同一个形状，不是手搓的字符串。
    pub(in crate::ui::features::reader) fn slot(href: &str, chapter: usize, offset: usize) -> ImageSlot {
        use crate::ui::foundation::images::{ByteSource, ChapterImages, TINY_PNG};
        use folio_core::content::format::ImageRes;

        let images = vec![ImageRes {
            href: href.into(),
            source: href.into(),
            media_type: "image/png".into(),
            width: 800,
            height: 600,
        }];
        let bytes: ByteSource = {
            let href = href.to_string();
            Arc::new(move |asked: &str| (asked == href).then(|| TINY_PNG.to_vec()))
        };
        let view = ChapterImages::new(Arc::new(BookImages::new(&images, bytes)), "Text/chap.xhtml");
        let request = view.decode_request(href).expect("表里有这条就该给得出活儿");
        ImageSlot {
            natural: (800.0, 600.0),
            mime: "image/png".into(),
            href: href.into(),
            name: file_name(chapter + 1, href, "image/png"),
            request,
            place: Place::Reflow { chapter, offset },
        }
    }
}

#[cfg(test)]
mod view_tests {
    //! 这一层最容易错在四件看不见的事上：该挂的时候没挂（层根本没出来）、该拦的时候
    //! 没拦（在图上看一眼连着把底下那页翻了）、「拖一张图」被当成点击、
    //! 以及顶栏那一颗按钮把自己的那一击漏给了身后的整屏。
    //! 四条都能在桌面测准：`try_find` 看得见元素在不在树里，鼠标、滚轮与捏合走的
    //! 是与真机同一条 `dispatch_event` 入口。
    //!
    //! **长按那一条不在这儿钉**：`LongPressEvent` 只由触摸手势合成（`gestures.rs` 的
    //! `TouchGestureState::LongPressing`），测试窗口投不出一次「被认领的长按」——
    //! 定版式那一页的入口要上 Android 真按一次才算数。**切到下一张的解码时机**同理：
    //! 这里的 `slot()` 交得出字节，但「预取那两张什么时候解好」是后台那一趟的事，
    //! 门在 `image_atlas` 那边。

    use std::cell::Cell;
    use std::rc::Rc;
    use std::sync::Arc;

    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt as _;
    // `test_support` 得在这儿再导入一次：内层 `mod` 不继承外层文件的 use。
    use gpui_kit::{
        AnyWindowHandle, AppContext as _, Context, Entity, EntityId, FocusHandle, InputEvent as _,
        InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
        MouseUpEvent, ParentElement as _, PinchEvent, Pixels, PlatformInput, Point, Render,
        ScrollDelta, ScrollWheelEvent, StatefulInteractiveElement as _, Styled as _, Subscription,
        TestAppContext, TouchPhase, Window, div, point, px, size,
    };

    use super::{ImageSlot, ImageViewer, Looking, overlay};
    use crate::ui::features::reader::Arrow;
    use crate::ui::features::reader::model::session::ReaderSession;

    /// 视口 400×600，图 800×600：适配框因此是 `400×300`，放大上限 `2.0`（原像素那一档）。
    const VIEWPORT: (f32, f32) = (400.0, 600.0);
    const CENTER: Point<Pixels> = point(px(200.0), px(300.0));
    /// 这一列几张。计数的分母、越界那一档的判据都跟着它走。
    const TOTAL: usize = 4;

    /// 挂这一层的最小宿主。生产上是 `ReadingPage` 挂它（盖住顶栏底栏与那两块弹层），
    /// 这里要钉的只是「在不在树里」与「身后的那一屏收不收得到」。
    struct ViewerHost {
        viewer: Entity<ImageViewer>,
        /// 会话带着那一列图：`overlay` 那一颗颗按钮的去向都在会话上（分享、下载、定位原文）。
        session: Entity<ReaderSession>,
        sequence: Arc<[ImageSlot]>,
        /// 这一层的焦点句柄。生产上是阅读页手里那一份（`page::ReadingPage`），测试台照抄。
        focus: FocusHandle,
        /// 顶上那条系统栏 / 挖孔让给这一层的 inset。日常是 0（桌面与测试），
        /// 一条用例把它抬起来，钉「黑底盖住那一条」这一件。
        top_inset: f32,
        /// 落在**身后那一层**的一击次数：`occlude` 与按钮的 `stop_propagation`
        /// 没拦住就会 +1。
        body_taps: Rc<Cell<usize>>,
        /// 订阅必须活着事件才来：`Subscription` 一 drop 就退订。生产上这一格在
        /// `page::ReadingPage::new` 里，测试台照抄同一个形状。
        _watch: Subscription,
    }

    impl ViewerHost {
        fn boot(cx: &mut Context<Self>) -> Self {
            let viewer = cx.new(|_| ImageViewer::default());
            // 会话用兜底那一屏的构造：本层只问它要那一列图与那颗图集，不排任何正文。
            let session = cx.new(|cx| ReaderSession::boot(cx));
            let sequence: Vec<ImageSlot> = (0..TOTAL)
                .map(|i| super::tests::slot(&format!("p{i}.png"), i / 2, i * 7))
                .collect();
            let sequence: Arc<[ImageSlot]> = sequence.into();
            session.update(cx, |session, _| {
                session.image_sequence = Arc::clone(&sequence);
                session.set_image_viewer(viewer.clone());
            });
            let _watch = cx.observe(&viewer, |_, _, cx| cx.notify());
            Self {
                viewer,
                session,
                sequence,
                focus: cx.focus_handle(),
                top_inset: 0.0,
                body_taps: Rc::default(),
                _watch,
            }
        }
    }

    impl Render for ViewerHost {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let taps = self.body_taps.clone();
            // 借用先还掉：`overlay` 要 `&mut App`，而这里正借着 `self.session` 读图集。
            let atlas = self
                .session
                .read_with(cx, |session, _| session.atlas.clone());
            let session = self.session.clone();
            let viewer = self.viewer.clone();
            let focus = self.focus.clone();
            let top_inset = self.top_inset;
            let view: EntityId = cx.entity().entity_id();
            div()
                .size_full()
                .relative()
                .child(
                    div()
                        .id("body-underneath")
                        .size_full()
                        .on_click(move |_, _, _| taps.set(taps.get() + 1)),
                )
                .children(overlay(
                    &viewer, &atlas, &session, view, top_inset, &focus, window, cx,
                ))
        }
    }

    fn boot(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<ViewerHost>) {
        cx.update(gpui_kit::init);
        // 那一行的文案走 `tr`/`tf`，而它们读的是**每个应用上下文自己的** `ConfigHolder`
        // （不是进程级那个全局，见 `shared::l10n` 模块头）。不装就是把界面语言
        // 交给上一个测试留下的那一份 —— 并行跑批时会互相改语言。
        cx.update(|app| {
            crate::ui::services::config::ConfigHolder::install(
                folio_core::Config::default(),
                app,
            );
        });
        let mut host = None;
        let handle: AnyWindowHandle = cx
            .open_window(size(px(400.0), px(600.0)), |window, cx| {
                let view = cx.new(ViewerHost::boot);
                host = Some(view.clone());
                Root::new(view, window, cx)
            })
            .into();
        (handle, host.expect("测试台应已建起"))
    }

    /// 画一帧并跑空事件。摆位与 `try_find` 看的都是**已经画出来**的那一帧。
    fn tick(cx: &mut TestAppContext, handle: AnyWindowHandle) {
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("渲染应成功");
    }

    /// 开在第 `index` 张。生产上是正文那一次点击开的（`view.rs::open_picture`），
    /// 这里直接开：本层要钉的是开着之后的行为，不是那一次点击。
    fn open(cx: &mut TestAppContext, host: &Entity<ViewerHost>, index: usize) {
        let sequence = host.read_with(cx, |host, _| host.sequence.clone());
        cx.update(|app| {
            host.update(app, |this, app| {
                this.viewer.update(app, |viewer, cx| {
                    viewer.open(index, VIEWPORT, sequence);
                    cx.notify();
                });
            });
        });
    }

    fn is_open(cx: &TestAppContext, host: &Entity<ViewerHost>) -> bool {
        host.read_with(cx, |this, cx| this.viewer.read(cx).is_open())
    }

    /// 这一层此刻看的是第几张。
    fn index(cx: &TestAppContext, host: &Entity<ViewerHost>) -> usize {
        host.read_with(cx, |this, cx| {
            this.viewer
                .read(cx)
                .looking
                .as_ref()
                .expect("开着的时候该有一张被看的图")
                .index
        })
    }

    /// 这一层此刻的倍率与位移。直接读状态：这两条式的错都在「数」上，不在像素上。
    fn scale_and_pan(cx: &TestAppContext, host: &Entity<ViewerHost>) -> (f32, (f32, f32)) {
        host.read_with(cx, |this, cx| {
            this.viewer
                .read(cx)
                .looking
                .as_ref()
                .map(|looking: &Looking| (looking.scale, looking.pan))
                .expect("开着的时候该有一张被看的图")
        })
    }

    fn mounted(cx: &mut TestAppContext, handle: AnyWindowHandle) -> bool {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.try_find("image-viewer").is_some()
        })
        .expect("渲染应成功")
    }

    /// 顶栏那一行的某一颗此刻在树里吗。
    fn has_button(cx: &mut TestAppContext, handle: AnyWindowHandle, id: &'static str) -> bool {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.try_find(id).is_some()
        })
        .expect("渲染应成功")
    }

    fn body_taps(cx: &TestAppContext, host: &Entity<ViewerHost>) -> usize {
        host.read_with(cx, |this, _| this.body_taps.get())
    }

    fn dispatch(cx: &mut TestAppContext, handle: AnyWindowHandle, event: PlatformInput) {
        cx.update_window(handle, |_, window, cx| window.dispatch_event(event, cx))
            .expect("事件应送达窗口");
        cx.run_until_parked();
    }

    fn click(cx: &mut TestAppContext, handle: AnyWindowHandle, at: Point<Pixels>) {
        dispatch(
            cx,
            handle,
            MouseDownEvent {
                button: MouseButton::Left,
                position: at,
                ..Default::default()
            }
            .to_platform_input(),
        );
        dispatch(
            cx,
            handle,
            MouseUpEvent {
                button: MouseButton::Left,
                position: at,
                ..Default::default()
            }
            .to_platform_input(),
        );
    }

    /// 鼠标拖一段：按下、挪出去、抬起。桌面上的「拖着看」就是这三下。
    fn drag(
        cx: &mut TestAppContext,
        handle: AnyWindowHandle,
        from: Point<Pixels>,
        to: Point<Pixels>,
    ) {
        dispatch(
            cx,
            handle,
            MouseDownEvent {
                button: MouseButton::Left,
                position: from,
                ..Default::default()
            }
            .to_platform_input(),
        );
        dispatch(
            cx,
            handle,
            MouseMoveEvent {
                position: to,
                pressed_button: Some(MouseButton::Left),
                ..Default::default()
            }
            .to_platform_input(),
        );
        dispatch(
            cx,
            handle,
            MouseUpEvent {
                button: MouseButton::Left,
                position: to,
                ..Default::default()
            }
            .to_platform_input(),
        );
    }

    fn pinch(cx: &mut TestAppContext, handle: AnyWindowHandle, delta: f32) {
        dispatch(
            cx,
            handle,
            PinchEvent {
                position: CENTER,
                delta,
                phase: TouchPhase::Moved,
                ..Default::default()
            }
            .to_platform_input(),
        );
    }

    /// 滚轮 / 手指平移的一趟：`dx` 与 `dy` 各给一条，判据在 `overlay` 那三分叉上。
    /// 相位固定 `Moved`（触屏上手指平移中间的那些帧），一趟手势的首帧另用
    /// [`scroll_start`]。
    fn scroll(cx: &mut TestAppContext, handle: AnyWindowHandle, dx: f32, dy: f32) {
        scroll_at(cx, handle, dx, dy, TouchPhase::Moved);
    }

    /// 手机上一次手指按下的那一帧（`TouchPhase::Started`）：一趟手势从这里起，
    /// 上一趟那面「已经切过一张」的旗在这里还回来。
    fn scroll_start(cx: &mut TestAppContext, handle: AnyWindowHandle, dx: f32, dy: f32) {
        scroll_at(cx, handle, dx, dy, TouchPhase::Started);
    }

    fn scroll_at(
        cx: &mut TestAppContext,
        handle: AnyWindowHandle,
        dx: f32,
        dy: f32,
        touch_phase: TouchPhase,
    ) {
        dispatch(
            cx,
            handle,
            ScrollWheelEvent {
                position: CENTER,
                delta: ScrollDelta::Pixels(point(px(dx), px(dy))),
                touch_phase,
                ..Default::default()
            }
            .to_platform_input(),
        );
    }

    /// 桌面真滚轮的一格（`ScrollDelta::Lines`）：X11 平台报的就是这一形状，
    /// 而触屏那端永远是 `Pixels`（见 `has_mouse_wheel`）。
    fn wheel_lines(cx: &mut TestAppContext, handle: AnyWindowHandle, dx: f32, dy: f32) {
        dispatch(
            cx,
            handle,
            ScrollWheelEvent {
                position: CENTER,
                delta: ScrollDelta::Lines(point(dx, dy)),
                touch_phase: TouchPhase::Moved,
                ..Default::default()
            }
            .to_platform_input(),
        );
    }

    /// 装上桌面平台服务。没装时 [`crate::ui::host::has_mouse_wheel`] 给 `false`
    /// （走触屏那条路），而滚轮缩放与方向键那两条要的正是桌面那一档。
    fn desktop_host(cx: &mut TestAppContext) {
        cx.update(crate::ui::host::install);
    }

    /// 黑底要盖上顶上那条系统栏 / 挖孔（`top_inset`）。
    ///
    /// 阅读页那一层的盒从系统栏下面起（外壳 `reading_overlay` 的 `pt`，见
    /// `shell::reading_top_inset`），所以这一层得用负 `top` **向上长回去** ——
    /// 写成 `inset_0` 就只盖到下面一段，露出的那一条是书页的浅底色，
    /// 真机上读作「最上面一条白」。
    #[gpui_kit::test]
    fn the_layer_grows_up_over_the_system_bar_inset(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        cx.update(|app| {
            host.update(app, |this, cx| {
                this.top_inset = 30.0;
                cx.notify();
            });
        });
        open(cx, &host, 0);
        tick(cx, handle);

        let bounds = cx
            .update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window
                    .try_find("image-viewer")
                    .expect("开着就该在树里")
                    .bounds()
            })
            .expect("渲染应成功");
        assert_eq!(
            f32::from(bounds.origin.y),
            -30.0,
            "顶上那一条系统栏该归这一层的黑底"
        );
        assert_eq!(
            f32::from(bounds.size.height),
            630.0,
            "向上长 30，高度就多那 30（600 + 30）"
        );
    }

    /// 查看器开着时方向键**不归正文**：外壳那四条方向键绑定先跑，正文这一层必须让开，
    /// 这一键才能落到查看器自己的 `on_key_down`（它在键绑定之后）。不让开的表现是
    /// 「看图时按右箭头，图的下一张没来，背后的书翻了一页」。
    #[gpui_kit::test]
    fn the_reader_lets_go_of_the_arrow_keys_while_the_viewer_is_open(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        open(cx, &host, 1);
        tick(cx, handle);

        let handled = cx.update(|app| {
            host.update(app, |this, app| {
                this.session
                    .update(app, |session, cx| session.arrow_turn(Arrow::Right, cx))
            })
        });
        assert!(!handled, "查看器开着时正文该让开方向键");
        assert_eq!(index(cx, &host), 1, "那一键不该同时把正文翻一页");
    }

    /// 回归门：没图就不挂层，开了才挂，关掉又收干净 —— 一层都不留在树里。
    #[gpui_kit::test]
    fn the_layer_mounts_only_while_a_picture_is_being_seen(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        tick(cx, handle);
        assert!(!mounted(cx, handle), "没在看图的时候一层都不该挂");

        open(cx, &host, 0);
        tick(cx, handle);
        assert!(mounted(cx, handle), "开着就该在树里");

        cx.update(|app| {
            host.update(app, |this, app| {
                this.viewer.update(app, |viewer, cx| {
                    viewer.close();
                    cx.notify();
                });
            });
        });
        tick(cx, handle);
        assert!(!mounted(cx, handle), "关掉之后不该留一层黑的在树上");
    }

    /// 顶栏那一行五格都该在树里 —— 少挂一颗在屏幕上只是「少一个图标」，
    /// 而 `try_find` 看得见它到底有没有被摆出来。
    #[gpui_kit::test]
    fn the_top_row_carries_all_four_buttons_and_the_counter(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        open(cx, &host, 1);
        tick(cx, handle);
        for id in [
            "image-share",
            "image-download",
            "image-locate",
            "image-close",
        ] {
            assert!(
                has_button(cx, handle, id),
                "顶栏那一行少了 {id} 那一颗：{id} 不在树里"
            );
        }
    }

    /// 点一下空白**不关**这一层——只能点「关闭」那一颗或者走系统返回关掉。
    ///
    /// 这一层仍然是 `occlude` 的：身后那一层一个字都不许收到（gpui 把一击发给
    /// 每一个 hitbox 含按下点的 handler，正文那层是整屏的）。点空白不关是为了
    /// 避免「缩着缩着松手一下就退出」，移动端上没有右键撤销这一档。
    #[gpui_kit::test]
    fn a_blank_tap_keeps_the_layer_open_and_reaches_nothing_below(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        open(cx, &host, 0);
        tick(cx, handle);

        click(cx, handle, CENTER);
        tick(cx, handle);
        assert!(is_open(cx, &host), "点空白不该关掉这一层");
        assert_eq!(
            body_taps(cx, &host),
            0,
            "那一击属于查看器，不该同时落到正文的整屏 hitbox 上"
        );
    }

    /// 右上角那颗关闭：关掉这一层，**并且不把身后那层点着**。
    ///
    /// 这一条钉的不是 `occlude`（上一条已经钉过）而是按钮自己的 `stop_propagation`：
    /// 关闭那颗的 hitbox 整个落在这一层的 hitbox 里面，gpui 因此会把这一击**同时**发给
    /// 这一层 —— 少了那一句，按下关闭就是「关掉并且把这一击漏给身后的正文」（翻页）。
    #[gpui_kit::test]
    fn the_close_button_closes_without_reaching_anything_below(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        open(cx, &host, 0);
        tick(cx, handle);

        cx.update_window(handle, |_, window, cx| {
            window.click("image-close", cx);
        })
        .expect("点击应送达那颗按钮");
        cx.run_until_parked();
        tick(cx, handle);

        assert!(!is_open(cx, &host), "按下关闭该关掉这一层");
        assert_eq!(
            body_taps(cx, &host),
            0,
            "那一击归按钮，不该同时算到身后那一层头上"
        );
    }

    /// 横扫切一张，越界不外溢。这条走的是**滚轮那条入口**（移动端的手指平移与桌面的
    /// 滚轮同一条），攒与阈值都归 `swipe_by`。
    #[gpui_kit::test]
    fn a_horizontal_swipe_walks_the_column_and_stops_at_both_ends(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        open(cx, &host, 1);
        tick(cx, handle);

        // 没攒够：不动。
        scroll(cx, handle, -30.0, 0.0);
        assert_eq!(index(cx, &host), 1, "过一半的横扫不该切");
        // 攒过 60：切下一张。
        scroll(cx, handle, -40.0, 0.0);
        assert_eq!(index(cx, &host), 2, "横扫过阈值该切下一张");

        // 最后一张再往左：钉在最后一张，不把这一层甩出界。
        open(cx, &host, TOTAL - 1);
        tick(cx, handle);
        scroll(cx, handle, -200.0, 0.0);
        assert_eq!(index(cx, &host), TOTAL - 1, "最后一张没有下一张");
        // 第一张再往右：同理。
        open(cx, &host, 0);
        tick(cx, handle);
        scroll(cx, handle, 200.0, 0.0);
        assert_eq!(index(cx, &host), 0, "第一张没有前一张");
    }

    /// 一回手势只切一张：手机上一次横扫从按下到惯性尾巴走完会逐帧派发十几条增量
    /// （`gestures.rs` 的 `Panning` 每帧一条、抬指后还有 `Momentum`），不拦的话
    /// 半秒滑过四五张，而读者只看到最后那一张。
    #[gpui_kit::test]
    fn one_swipe_switches_only_one_picture(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        open(cx, &host, 0);
        tick(cx, handle);

        // 首帧（手指刚过 slop）就过了阈值：切一张。
        scroll_start(cx, handle, -70.0, 0.0);
        assert_eq!(index(cx, &host), 1, "首帧过了阈值该切一张");
        // 同一回手势里接着攒（手指还在滑 + 惯性尾巴）：一张都不再切。
        scroll(cx, handle, -300.0, 0.0);
        scroll(cx, handle, -300.0, 0.0);
        assert_eq!(index(cx, &host), 1, "同一回手势只切一张");
        // 新的一回（手指再次按下 `Started`）：旗还回来，又能切。
        scroll_start(cx, handle, -70.0, 0.0);
        assert_eq!(index(cx, &host), 2, "新的一回手势该能再切一张");
    }

    /// 桌面：滚轮缩放、横向滚轮切图。触屏那条路不受影响 —— 上面几条钉着。
    #[gpui_kit::test]
    fn the_mouse_wheel_zooms_the_picture_on_desktop(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        desktop_host(cx);
        open(cx, &host, 0);
        tick(cx, handle);

        // 往上一格 = 放大一成（一格 24 像素 / `ZOOM_STEP` 0.15）。
        wheel_lines(cx, handle, 0.0, -1.0);
        assert_eq!(scale_and_pan(cx, &host).0, 1.15, "滚轮往上该放大");
        // 往下一格：缩回去（夹在铺满那一档）。
        wheel_lines(cx, handle, 0.0, 1.0);
        assert_eq!(scale_and_pan(cx, &host).0, 1.0, "滚轮往下该缩小");
        // 一路往上：顶在「一个源像素对一个 CSS 像素」那一档（800 宽铺在 400 ⇒ 2.0）。
        for _ in 0..40 {
            wheel_lines(cx, handle, 0.0, -1.0);
        }
        assert_eq!(scale_and_pan(cx, &host).0, 2.0, "放大上限仍是原像素那一档");

        // 横向滚轮（X11 下 shift+滚轮被平台翻成横轴）：切图。
        open(cx, &host, 0);
        tick(cx, handle);
        wheel_lines(cx, handle, -4.0, 0.0);
        assert_eq!(index(cx, &host), 1, "横向滚轮该切下一张");
    }

    /// 桌面：方向键左右切图。
    #[gpui_kit::test]
    fn the_arrow_keys_walk_the_column_on_desktop(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        desktop_host(cx);
        open(cx, &host, 1);
        tick(cx, handle);

        cx.simulate_keystrokes(handle, "right");
        assert_eq!(index(cx, &host), 2, "右方向键该看下一张");
        cx.simulate_keystrokes(handle, "left");
        assert_eq!(index(cx, &host), 1, "左方向键该看前一张");
        // 两头夹住：第一张再往左不动。
        open(cx, &host, 0);
        tick(cx, handle);
        cx.simulate_keystrokes(handle, "left");
        assert_eq!(index(cx, &host), 0, "第一张没有前一张");
    }

    /// 纵向的一趟**不该**切图：读者在铺满那一档往下滑，是想看下一屏还是想切一张？
    /// 答案是都不 —— 铺满那一档纵向没有可挪的余地，横扫只认横向为主的那一趟。
    #[gpui_kit::test]
    fn a_vertical_scroll_does_not_switch(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        open(cx, &host, 1);
        tick(cx, handle);
        scroll(cx, handle, 0.0, -400.0);
        assert_eq!(index(cx, &host), 1, "纯纵向的一趟不切图");
        assert_eq!(scale_and_pan(cx, &host).1, (0.0, 0.0), "也不该挪动图");
    }

    /// 放大之后横向那一趟归平移，不归切图（口径：铺满那一档才切）。
    #[gpui_kit::test]
    fn horizontal_scroll_pans_once_zoomed_in(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        open(cx, &host, 1);
        tick(cx, handle);
        pinch(cx, handle, 1.0);
        assert_eq!(scale_and_pan(cx, &host).0, 2.0, "前置：放到原像素那一档");

        scroll(cx, handle, -200.0, 0.0);
        assert_eq!(index(cx, &host), 1, "放大着横扫不该切图");
        assert!(
            scale_and_pan(cx, &host).1.0 < 0.0,
            "同一趟该把图横向挪走： {:?}",
            scale_and_pan(cx, &host).1
        );
    }

    /// 拖一张放大了的图：图跟着手指走，而这一路不漏给身后那层。
    ///
    /// （前两版这一条钉的是「松手那一下不该当成关闭」—— 点空白现在不关了，
    /// 但「拖 = 平移」与「拖不漏给正文」两样仍要钉住。）
    #[gpui_kit::test]
    fn dragging_the_picture_pans_without_reaching_the_body_below(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        open(cx, &host, 0);
        tick(cx, handle);
        pinch(cx, handle, 1.0);

        drag(
            cx,
            handle,
            point(px(150.0), px(300.0)),
            point(px(260.0), px(300.0)),
        );
        tick(cx, handle);
        assert!(is_open(cx, &host), "拖过一张图不该把这一层关掉");
        assert_eq!(body_taps(cx, &host), 0);
        let (scale, pan) = scale_and_pan(cx, &host);
        assert_eq!(
            (scale, pan.0.round() as i32),
            (2.0, 110),
            "拖动要把图挪走那一段"
        );
    }

    /// 铺满那一档的横向拖动是**切图**而不是关闭：桌面上「拖一下」与「点一下」
    /// 都从同一次按下开始，靠阈值分开这两件事。
    ///
    /// 拖完之后单独再点一下空白也**不关**：移动端上没有右键撤销这一档，
    /// 「缩着缩着松手一下就退出」是更差的体验。关闭只走「关闭」按钮与系统返回。
    #[gpui_kit::test]
    fn dragging_sideways_at_the_fitted_level_switches_picture(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        open(cx, &host, 1);
        tick(cx, handle);

        drag(
            cx,
            handle,
            point(px(300.0), px(300.0)),
            point(px(180.0), px(310.0)),
        );
        tick(cx, handle);
        assert_eq!(index(cx, &host), 2, "往左拖过阈值该是下一张");
        assert!(is_open(cx, &host), "拖完不该顺手把这层关掉");

        // 拖完之后**单独再点一下**空白也不该关：只有关闭按钮和系统返回能关。
        click(cx, handle, point(px(180.0), px(480.0)));
        tick(cx, handle);
        assert!(is_open(cx, &host), "点空白不该关掉这一层");
    }

    /// 捏合的两个顶：放大顶在**原像素**那一档，缩小顶在铺满那一档。
    ///
    /// 越过原像素只是插值出来的糊，而低于铺满意味着图缩到比屏还小 —— 两头都不该给。
    #[gpui_kit::test]
    fn pinching_stops_at_the_source_pixels_on_both_ends(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        open(cx, &host, 0);
        tick(cx, handle);
        assert_eq!(scale_and_pan(cx, &host).0, 1.0, "起手就是铺满那一档");

        pinch(cx, handle, 1.0);
        assert_eq!(
            scale_and_pan(cx, &host).0,
            2.0,
            "800 宽的图铺在 400 ⇒ 顶在 2 倍"
        );
        pinch(cx, handle, 1.0);
        assert_eq!(scale_and_pan(cx, &host).0, 2.0, "顶到了就不许再往上");
        pinch(cx, handle, -1.0);
        assert_eq!(scale_and_pan(cx, &host).0, 1.0, "最小就是铺满那一档");
    }

    /// 平移是按轴夹的：这一张图放大后**横向**多出 400 像素可拖，纵向刚好铺满 ⇒ 拖不动。
    /// 而没放大之前两轴都铺不满，滚轮一个字都不该挪。
    #[gpui_kit::test]
    fn panning_is_clamped_axis_by_axis(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        open(cx, &host, 0);
        tick(cx, handle);
        // 铺满那一档：纵向给多少都不动（这条轴上没有可拖的余地，也不是横扫）。
        scroll(cx, handle, 0.0, -100.0);
        assert_eq!(
            scale_and_pan(cx, &host).1,
            (0.0, 0.0),
            "铺满那一档没有可拖的余地"
        );

        pinch(cx, handle, 1.0);
        // 纵向：放大后 600 高，视口也 600 高 ⇒ 钉在中间。
        scroll(cx, handle, 0.0, -100.0);
        assert_eq!(
            scale_and_pan(cx, &host).1,
            (0.0, 0.0),
            "纵向铺得下的轴拖不动"
        );
        // 横向：800 对 400 ⇒ 每边可拖 200。
        scroll(cx, handle, -150.0, 0.0);
        assert!(
            scale_and_pan(cx, &host).1.0 < -100.0,
            "横向该挪得动：{:?}",
            scale_and_pan(cx, &host).1
        );
        scroll(cx, handle, -300.0, 0.0);
        assert_eq!(
            scale_and_pan(cx, &host).1.0,
            -200.0,
            "拖到边就停住，别把图推出去露黑边"
        );
    }

    /// 左上那两颗现在把手上的那张递给系统。这一条钉的是**接线**：按钮的 id 挂上了、
    /// 那一击真到了会话的命令上。字节的去向（系统的分享面板 / 相册）在宿主管不到的
    /// 那边 —— 没装平台服务时 `host` 那两句安静 no-op，所以这里断不了它落了盘，
    /// 那一半要上真机看（见模块头那份未验清单）。
    #[gpui_kit::test]
    fn the_share_and_download_buttons_reach_the_session(cx: &mut TestAppContext) {
        let (handle, host) = boot(cx);
        open(cx, &host, 2);
        tick(cx, handle);

        for id in ["image-share", "image-download"] {
            assert!(has_button(cx, handle, id), "{id} 该在树里");
            cx.update_window(handle, |_, window, cx| window.click(id, cx))
                .expect("点击应送达那颗按钮");
            cx.run_until_parked();
            tick(cx, handle);
        }
        // 两颗按完这一层还开着：分享与下载不是「退出看图」。
        assert!(is_open(cx, &host), "按完分享/下载不该把这一层关掉");
        assert_eq!(index(cx, &host), 2, "也不该切到别的张");
        assert_eq!(body_taps(cx, &host), 0, "那一击归按钮");
    }
}
