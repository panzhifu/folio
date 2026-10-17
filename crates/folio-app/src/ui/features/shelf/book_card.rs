//! 一格卡片：封面那一格 + 书名 + 作者，以及它自己手上的两桩手势。
//!
//! 卡片单独成文件，是为了让「一张卡长什么样」只有一处可改：[`Card`] 是库行摊平到
//! 界面直接用的样子（封面已解码、作者已并成一行），[`ShelfView::card`] 把一条
//! [`ShelfItem`] 变成它，[`ShelfView::card_element`] 把它画成一格。排格子的事留在
//! [`super::view`] 的 `grid` 里 —— 几列、间距、滚动，与一张卡长什么样无关。
//!
//! 两桩手势各管各的：tap 派发 [`ShelfEvent::Open`] 把书交出去，长按（移动端）或
//! 右键（桌面端）开 [`super::book_menu`] 摆的那条浮层。菜单里没有「打开」，
//! 开书只有 tap 这一处入口；而长按一旦被菜单认领，这一次触摸就不会再派发成 tap。
//!
//! 一格的三种状态都要看得出来是它的：第一次上屏时淡到位（[`Enter`]），指过去描边
//! 加投影，按下去暗一截。桌面与手机各吃到其中一部分 —— 悬停只有指针那边有，
//! 按压两端都有（轻点在移动端合成成 MouseDown/MouseUp，见 `plans/facts.md`）。
//!
//! 封面走 gpui 的 `img` 元素，纹理上传由它负责（正文里的 `<img>` 用的是同一条路径，
//! 见 `reader_view`）；解码只在这一处调用 [`crate::ui::shared::images::cover_image`]。
//!
//! 没封面的书**必须**照样出现在架上并占住一格，否则「导入了一本但看不见」和
//! 「根本没导进来」在真机上没法分开 —— 所以那一格画的是书名，不是空白。

use std::sync::Arc;
use std::time::{Duration, Instant};

use folio_core::store::ShelfItem;
use gpui_kit::base::motion::{Easing, Stagger, StaggerOrigin, Timing};
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::menu::ContextMenuExt;
use gpui_kit::component::h_flex;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, BoxShadow, Context, Image, InteractiveElement, IntoElement, ObjectFit,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, StyledImage,
    TestSupportExt as _, Window, div, hsla, img, px,
};

use crate::ui::shared::images::cover_image;
use crate::ui::shared::l10n::tf;

use super::book_menu;
use super::view::{ShelfEvent, ShelfView};

/// 封面格的宽高比（宽 ÷ 高），出版物的通行比例。
///
/// 行高由它定而不是由图片自己定：图片的真实比例五花八门（方图、长条都有），
/// 让每格按图算高会得到一片参差的边。统一裁成这一档，多出来的部分由
/// `ObjectFit::Cover` 裁掉。
///
/// 架上这一格不是在这里按格宽推出高度来的，而是 [`super::view::grid_spec`] 算好
/// `cover_height` 递进 [`ShelfView::card_element`]：那一格的高要被 `uniform_list`
/// 拿去当整排的排距，而它量的是 min-content 宽度 —— 让排版自己按宽度推会得到一片
/// 被书名撑开的排距（那里有原因）。
///
/// `pub(super)` 就是为 [`super::view`] 那一个用处。
pub(super) const COVER_RATIO: f32 = 2.0 / 3.0;

/// 多个作者并成一行用的分隔（逻辑上是一个字符的空格 + `·` + 空格）。
///
/// 一张卡下面那一行、「信息」面板里那一格、以及阅读页的书头，用的都是这一条：读者在架上
/// 看见「甲 · 乙」，点开改完仍该是两个人，而不是一个名叫「甲 · 乙」的人
/// （拆回去见 [`super::book_info::split_authors`]，库里存的仍是换行拼接）。
pub(crate) const AUTHOR_SEP: &str = " · ";

/// 相邻两格起步的间隔。主题 `motion` 里只有「一次动多久」那一档，没有「隔多久动下一格」，
/// 所以这一条定在这里：14ms 一格，推到 [`ENTER_DELAY_CAP`] 那 200ms 为止（约第 15 格），
/// 再往后推就开始看得出是在排队浮现了。
const ENTER_INTERVAL: Duration = Duration::from_millis(14);

/// 最后一格最多往后推多久。超过这一档的格子与它同时落 —— 一整面墙的书不该
/// 一列一列地依次浮现，那已经是在放开场动画而不是在交代「这些书刚摆上来」。
const ENTER_DELAY_CAP: Duration = Duration::from_millis(200);

/// 逐格推后的那条规则：从第一格起算。
const ENTER_STAGGER: Stagger = Stagger::new(ENTER_INTERVAL, StaggerOrigin::First);

/// 悬停时那一格封面的投影：一颗「抬起来 2 像素」的影子，而不是真把它挪 2 像素。
///
/// 投影是纯绘制，不动排版；`top` 那一套相对位移在 `uniform_list` 的 `grid` 排布里
/// 走不通（taffy 只在 block/flexbox 两条排布里认相对偏移），写上去只会是一条谁也
/// 看不见的死样式。
fn hover_shadow() -> Vec<BoxShadow> {
    vec![BoxShadow::new(px(0.), px(2.), hsla(0., 0., 0., 0.18)).blur_radius(px(6.))]
}

/// 一片格子第一次上屏时的「落定」。
///
/// 交代的是「这些书刚摆上来」这一件变化：整片从透明淡到位，越靠后的格子起步越晚。
/// 只放**第一次**上屏那一段预算 —— `uniform_list` 会把滚出视区的格子的一切状态丢掉，
/// 不设终点的话读者每滚一轮整片架子就重落一遍，而滚动不是变化
/// （Design Guides「Motion」：动效解释变化，不是环境装饰）。
///
/// 动的是不透明度，不是排距：格子的位置一开始就是对的，淡到位不会让任何一行推离
/// 它那条对齐脊线。
pub(super) struct Enter {
    /// 从第一次画出格子起算过去了多久。`None` = 预算已经花完，或者系统要求减少
    /// 动态效果：两种都是每一格直接画成到位的样子。
    elapsed: Option<Duration>,
    /// 一格淡到位要多久，取自主题的 `duration_normal`。
    duration: Duration,
    /// 架上此刻一共几本。
    count: usize,
}

impl Enter {
    /// 第 `index` 格此刻该画到几成：`0.` 全透明，`1.` 到位。
    fn progress(&self, index: usize) -> f32 {
        let Some(elapsed) = self.elapsed else {
            return 1.0;
        };
        let delay = ENTER_STAGGER.delay(index, self.count).min(ENTER_DELAY_CAP);
        Timing::new(self.duration)
            .delay(delay.into())
            .ease(Easing::EaseOut)
            .sample(elapsed)
            .directed_progress
    }
}

/// 一张卡片：库行摊平到界面直接用的样子 —— 封面已解码，作者已并成一行。
pub(super) struct Card {
    pub(super) id: i64,
    pub(super) title: SharedString,
    pub(super) authors: SharedString,
    pub(super) cover: Option<Arc<Image>>,
}

/// 一张组卡：一个组摊到架面那一格上的样子。
///
/// `covers` 是这个组摆在卡上的那几本封面（至多 [`TILE_SLOTS`] 张，缺封面的那本占一格但什么都不画），
/// `count` 是组里**一共**几本 —— 卡上画不下全部，那一行数字才是答案。
pub(super) struct GroupCard {
    pub(super) id: i64,
    pub(super) name: SharedString,
    pub(super) count: usize,
    pub(super) covers: Vec<Option<Arc<Image>>>,
}

/// 组卡里封面马赛克的格数：两行两列。
///
/// 写死 2×2 而不是按卡宽算能塞几格：一张组卡与一本的书卡占同一格宽，塞得下的就是这四张。
const TILE_ROWS: usize = 2;
const TILE_COLS: usize = 2;
pub(super) const TILE_SLOTS: usize = TILE_ROWS * TILE_COLS;

/// 马赛克与卡边的留白、格与格之间的缝（逻辑像素）。
///
/// 比书卡那一格的边松一点：卡里装的是四张小图，边太紧会读成一张被切成四块的图，
/// 而不是四本书。
const TILE_PAD: f32 = 6.0;
const TILE_GAP: f32 = 4.0;

/// 一格马赛克该多高：从卡面高里扣掉上下留白与中间那道缝，再对半。
///
/// 高由 `cover_height` 算而不是让排版自己按宽度推 —— 与 [`COVER_RATIO`] 那一段同一条理由，
/// `uniform_list` 量的是 min-content 宽。
fn tile_height(cover_height: f32) -> f32 {
    ((cover_height - TILE_PAD * 2.0 - TILE_GAP) / TILE_ROWS as f32).max(0.0)
}

impl ShelfView {
    /// 取出这一帧的落定时钟，并在还没落完的那几帧里把下一帧要回来。
    ///
    /// 起算时刻在**第一次真画出格子**的那一帧才记下，不在 [`ShelfView::boot`] 记：
    /// 窗口挂好之前 `boot` 就执行完了，差出去的那几百毫秒会把这一小段吃掉半截，
    /// 读者看到的就只剩最后几格在淡。
    ///
    /// 系统那条「减少动态效果」的偏好也在这一处认：认的话直接不给 `elapsed`，
    /// 每一格从一开始就画成到位的样子，一帧都不动。
    pub(super) fn enter_clock(&mut self, count: usize, window: &Window, cx: &App) -> Enter {
        let duration = cx.theme().motion.duration_normal;
        let at = *self.enter_at.get_or_insert_with(Instant::now);
        let elapsed = at.elapsed();
        let running = elapsed < duration + ENTER_DELAY_CAP && !cx.reduce_motion();
        if running {
            window.request_animation_frame();
        }
        Enter {
            elapsed: running.then_some(elapsed),
            duration,
            count,
        }
    }

    /// 一条库行 → 一张卡。封面没解过才解，解不出的那本记住 `None`。
    ///
    /// 解码结果记在架面那份 [`ShelfView::covers`] 里：改一个搜索词会把同一本书再查
    /// 出来一遍，没缓存的话每敲一个字都要把整个架面的 JPEG 重解一遍；`None` 也要存 ——
    /// 不记住这次失败，每查一次都会再试一遍同样的失败。
    ///
    /// **架上解的是缩略图，不是原图**（`plans/导入管线.md` §八「缩略图」那一段）：
    /// 卡片档只有几百像素，而原图常见 800×1200 —— 网格里那点面积用不上原图的像素，
    /// 而 `Image` 持有的是**编码字节**，所以省下来的是整屏的常驻内存。
    ///
    /// 生成是**懒**的：第一次真要显示这一格时才做，做完落在 `<书库>/.thumbs/`，
    /// 跨会话不再重做。取不到缩略图（老库没有 `cover_ref`、原图是坏字节、
    /// 目录写不进去）就**退回原图** —— 缩略图是可丢弃的加速品，不是功能的入口。
    ///
    /// 只有「信息」那一屏用原图（`book_info` 走 `store.book_cover`）：那里封面是主视觉，
    /// 要大。
    pub(super) fn card(&mut self, item: ShelfItem, cover: Option<Arc<Image>>) -> Card {
        Card {
            id: item.id,
            title: item.title.into(),
            authors: item.authors.join(AUTHOR_SEP).into(),
            cover,
        }
    }

    /// 一格封面：会话内解过就直接用，没解过才去磁盘。
    ///
    /// **字节从文件来**（v16：库里不存封面字节），走 `Store::book_cover` 那一条
    /// 「按 `cover_ref` 去 `covers/` 取」。文件不在就是 `None` —— 界面画占位，
    /// 这是有意的（封面是装饰，不是内容的入口）。
    ///
    /// 解码结果记在架面那份 [`ShelfView::covers`] 里：改一个搜索词会把同一本书再查
    /// 出来一遍，没缓存的话每敲一个字都要把整个架面的 JPEG 重解一遍；`None` 也要存 ——
    /// 不记住这次失败，每查一次都会再试一遍同样的失败。
    pub(super) fn resolve_cover(&mut self, item: &ShelfItem) -> Option<Arc<Image>> {
        if let Some(cached) = self.covers.get(&item.id) {
            return cached.clone();
        }
        let decoded = self
            .store
            .as_ref()
            .and_then(|store| store.book_cover(item.id).ok().flatten())
            .and_then(|bytes| {
                // 先试磁盘上那份缩略图（导入时已生成，见 `land_cover`）；
                // 没有或读不动就退回原图 —— 缩略图是可丢弃的加速品。
                let small = item
                    .cover_root
                    .as_deref()
                    .zip(item.cover_ref.as_ref())
                    .and_then(|(root, cover)| {
                        let path = folio_core::content::format::thumb::thumb_path(
                            root,
                            folio_core::infra::paths::THUMB_CARD_PX,
                            cover,
                        );
                        std::fs::read(path).ok()
                    });
                small
                    .as_deref()
                    .and_then(cover_image)
                    .or_else(|| cover_image(&bytes))
            });
        self.covers.insert(item.id, decoded.clone());
        decoded
    }

    /// 一张卡 → 一格元素。
    ///
    /// `labels` 是管理菜单那两条的文案：整个架面取一次再逐卡 clone 进来（浮层的
    /// builder 是 `Fn`，菜单每次打开都重建一遍），条目本身在 [`super::book_menu`]。
    ///
    /// `cover_height` 由架面按格宽算好递进来（[`super::view::grid_spec`]），而不是在这里
    /// 交给 `aspect_ratio` 让排版自己按宽度推 —— 那一格的高度要被 `uniform_list` 拿去当
    /// 整排的排距，而它量的是 min-content 宽度。比例仍是 [`COVER_RATIO`]，只是从这一格
    /// 挪到了算它的那一处。
    ///
    /// `enter` 与 `index` 一起管这一格的落定：`index` 是它在这面架上的第几格（横排
    /// 从左往右、从上往下数），落定的先后由它定，进度由那一片共同的那条时钟算。
    pub(super) fn card_element(
        &self,
        card: &Card,
        labels: &book_menu::Labels,
        cover_height: f32,
        enter: &Enter,
        index: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let book_id = card.id;
        let title = card.title.clone();
        let authors = card.authors.clone();
        let cover = card.cover.clone();
        let has_cover = cover.is_some();
        let placeholder_bg = cx.theme().muted;
        let muted_fg = cx.theme().muted_foreground;
        let ring_hover = cx.theme().border;
        let tapped = cx.entity().clone();
        let menu_entity = cx.entity().clone();
        // 菜单每次打开都重建一遍，闭包要一份自己的文案拷贝。
        let labels = labels.clone();
        // 组名与元素 id 用同一个串：指着的是整张卡，抬起来的却只有封面那一格，
        // 两者得认同一格，不然指书名那一行时封面毫无反应。
        let group: SharedString = format!("shelf-card-{book_id}").into();
        div()
            // 元素 id 用书库主键而不是标题：重新查库它不变，而标题会随导入更新，
            // 拿标题当键会让一张卡片每次改名都丢掉自己的按下态。
            .id(group.clone())
            .group(group.clone())
            .w_full()
            .flex()
            .flex_col()
            .gap_1()
            .cursor_pointer()
            // 按下去当场暗一截，不等人把书打开才回话：移动端那一下轻点在这里合成
            // 成 MouseDown/MouseUp（`plans/facts.md` Android 那一段），所以这一条
            // 两端都吃得到。抬起不做动画而是立刻给 —— 指针跟着手指走，慢半拍的
            // 高亮比没有高亮更容易指错。
            .active(|style| style.opacity(0.75))
            .opacity(enter.progress(index))
            .on_click(move |_, _, cx| tapped.update(cx, |_, cx| cx.emit(ShelfEvent::Open(book_id))))
            .child(
                div()
                    // 宿主门按封面这一格的真实坐标点下去，就能钉住「阅读层盖着时
                    // 这一击到不了书架」。生产构建里 `test_support` 原样返回元素。
                    .id(format!("shelf-cover-{book_id}"))
                    .test_support()
                    .w_full()
                    .h(px(cover_height))
                    // 描边常驻，只是平时透明：悬停时改的是颜色，不是这一格占多宽。
                    // 留着这一像素位才不会一指过去整排封面就抖一下。
                    .border_1()
                    .border_color(cx.theme().transparent)
                    .rounded_md()
                    .overflow_hidden()
                    .bg(placeholder_bg)
                    .group_hover(group, |style| {
                        style.border_color(ring_hover).shadow(hover_shadow())
                    })
                    .when_some(cover, |this, image| {
                        this.child(img(image).size_full().object_fit(ObjectFit::Cover))
                    })
                    // 没封面的那格把书名当封面画：一整格空白看起来像这本坏了，
                    // 而它只是没有图。
                    .when(!has_cover, |this| {
                        this.child(
                            div()
                                .size_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .px_2()
                                .text_center()
                                .text_color(muted_fg)
                                .child(title.clone()),
                        )
                    }),
            )
            .child(
                div()
                    .w_full()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(title),
            )
            .child(
                div()
                    .w_full()
                    .text_xs()
                    .text_color(muted_fg)
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(authors),
            )
            // 长按（移动端）或右键（桌面端）开管理菜单，条目见 [`super::book_menu`]。
            // 与右上角那颗三点按钮的导入菜单是同一套组件，只是触发从点击换成了长按，
            // 两档浮层同宽（[`book_menu::MIN_WIDTH`]）。
            .context_menu(move |menu, _, cx| {
                // 组的名分在**打开这一刻**现查：菜单里那几条打勾必须反映刚才那一刻的库，
                // 而不是上一次重查架面时的那一份。
                let groups = menu_entity.read_with(cx, |this, _| this.group_items(book_id));
                book_menu::build(menu, &labels, book_id, &menu_entity, groups)
            })
            .into_any_element()
    }

    /// 一张组卡 → 一格元素。
    ///
    /// 占的正是书卡那一格：封面槽换成 2×2 的马赛克，书名槽写组名，作者槽写一共几本 ——
    /// 于是「一组」与「一本」在同一排里对齐得起来，不必为它另起一段排版。
    ///
    /// 点下去是**筛到那一组**（[`ShelfView::set_group`]），不是开书：这一格没有「这一本」
    /// 可开。长按菜单不挂在这里 —— 组本身能改的事（改名 / 删除）此刻还没有入口，
    /// 摆一条空的或半条能点的菜单比不摆更糟。
    pub(super) fn group_cell_element(
        &self,
        tile: &GroupCard,
        cover_height: f32,
        enter: &Enter,
        index: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = tile.id;
        let name = tile.name.clone();
        let count = tile.count;
        let count_label = tf(cx, "shelf.group_count", &[("n", &count)]);
        let card_bg = cx.theme().popover;
        let empty_bg = cx.theme().muted;
        let muted_fg = cx.theme().muted_foreground;
        let ring = cx.theme().border;
        let tapped = cx.entity().clone();
        let group: SharedString = format!("shelf-tile-{id}").into();
        let cell = tile_height(cover_height);
        div()
            .id(group.clone())
            .group(group.clone())
            .w_full()
            .flex()
            .flex_col()
            .gap_1()
            .cursor_pointer()
            .active(|style| style.opacity(0.75))
            .opacity(enter.progress(index))
            .on_click(move |_, _, cx| {
                tapped.update(cx, |this, cx| {
                    this.set_group(super::view::GroupFilter::Collection(id), cx)
                })
            })
            .child(
                div()
                    .id(format!("shelf-tile-face-{id}"))
                    .test_support()
                    .w_full()
                    .h(px(cover_height))
                    .p(px(TILE_PAD))
                    .border_1()
                    .border_color(cx.theme().transparent)
                    .rounded_md()
                    .overflow_hidden()
                    .bg(card_bg)
                    .group_hover(group, |style| {
                        style.border_color(ring).shadow(hover_shadow())
                    })
                    // 四格一律摆满：只有一本时那一本仍占左上那一格，而不是被
                    // `flex_1` 拉成半张卡宽 —— 卡上那几格的尺寸在组与组之间得是同一个数，
                    // 读者才比得出「这两组看着差不多满」。
                    .children((0..TILE_ROWS).map(|row| {
                        h_flex()
                            .w_full()
                            .h(px(cell))
                            .gap(px(TILE_GAP))
                            .when(row + 1 < TILE_ROWS, |this| this.pb(px(TILE_GAP)))
                            .children((0..TILE_COLS).map(|col| {
                                let slot = row * TILE_COLS + col;
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .size_full()
                                    .rounded(px(2.0))
                                    .overflow_hidden()
                                    .when_some(
                                        tile.covers.get(slot).cloned().flatten(),
                                        |this, image| {
                                            this.child(
                                                img(image).size_full().object_fit(ObjectFit::Cover),
                                            )
                                        },
                                    )
                                    // 画不出图的那一格退成一块比卡面更深的底：留白看着像卡还没画完，
                                    // 一块底才像「这里有一本，只是它没有封面」。
                                    .when(
                                        tile.covers.get(slot).is_none_or(|c| c.is_none()),
                                        |this| this.bg(empty_bg),
                                    )
                            }))
                    })),
            )
            .child(
                div()
                    .w_full()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(name),
            )
            .child(
                div()
                    .w_full()
                    .text_xs()
                    .text_color(muted_fg)
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(count_label),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    //! 落定那一段只钉两条：它要走完，走完以后不许再放第二遍。
    //! 数学是纯函数，不必开窗也能验（与 `view` 里那两扇按列数的门同一条路）。

    use std::time::Duration;

    use super::{ENTER_DELAY_CAP, Enter};

    /// 一格从透明到到位要多久。测试里自己定一个数：进度只认「过去多久 vs 这一档」，
    /// 真值由主题的 `motion.duration_normal` 给（见 [`super::ShelfView::enter_clock`]）。
    const DURATION: Duration = Duration::from_millis(180);

    fn enter(elapsed: Option<Duration>, index: usize, count: usize) -> f32 {
        Enter {
            elapsed,
            duration: DURATION,
            count,
        }
        .progress(index)
    }

    /// 越靠后的格子起步越晚，但最晚的一格只比最早的那格晚 [`ENTER_DELAY_CAP`]：
    /// 一整面墙的书要一起落定，不是排成一列依次浮现。
    #[test]
    fn the_cards_settle_in_order_and_all_the_way() {
        assert_eq!(enter(Some(Duration::ZERO), 0, 30), 0.0);

        // 同一帧上，第 1 格比第 8 格已经淡得更开。
        let at = Some(Duration::from_millis(120));
        assert!(
            enter(at, 1, 30) > enter(at, 8, 30),
            "起步的早晚没体现在进度上：{} vs {}",
            enter(at, 1, 30),
            enter(at, 8, 30)
        );

        // 预算走完：第一格、中间一格、以及比架子还后面的一格都到位。
        let budget = DURATION + ENTER_DELAY_CAP;
        for index in [0, 7, 30, 400] {
            assert_eq!(enter(Some(budget), index, 400), 1.0, "第 {index} 格没落完");
        }
    }

    /// 预算花完（或系统要求减少动态效果）之后，一帧都不许再动。
    ///
    /// `uniform_list` 会把滚出视区的格子的一切状态丢掉，这条判据就是那一处唯一的
    /// 拦网：拦不住的话读者每滚一轮整片架子重落一遍（`grid_spec` 那边同样按
    /// 「测量给的是 min-content 宽」拦过一排问题）。
    #[test]
    fn a_settled_shelf_does_not_replay() {
        assert_eq!(enter(None, 0, 30), 1.0);
        assert_eq!(enter(None, 29, 30), 1.0);
    }
}
