//! 翻页动画的摆位层：一次翻页里那两张纸各自怎么走、谁压着谁。
//!
//! 决策层 [`turn`](crate::ui::features::reader::model::turn) 回答「这一步落在哪」（章内一页 / 跨章 / 一次滚动），
//! 这一层回答「那两张纸怎么走」。两边不碰对方的判据：换一档动画不该动章界规则，改章界
//! 规则也不该动这里的式子。分层的理由与 `PagingStrategy` 同一条 —— 这几条式子是纯的，
//! 能在宿主的 `cargo test` 里表驱动钉住；gpui 的 `TestWindow` 看不到绘制场景，位移只在
//! 这一层摸得到。
//!
//! 这一层给得出的东西有**两样**：位移，和「这一张纸被折线裁到哪一侧」。量距离按轴：横轴
//! 取视口宽，竖轴取视口高。位移是**烘进字形坐标**的（`view.rs` 把它当 `Scene::build` 的两
//! 个入参，正文没有 `div` 包着），裁剪则是逐字形绘制前的那一次剔除（`paint.rs`）—— 纸与纸
//! 之间没有可插入的绘制层，所以影子与折面只能作为盖在两者之上的一组形状来画（见 [`geometry::Shades`]）。
//!
//! **一挡一个文件**：`slide.rs` / `slide_up.rs` / `cover.rs` / `curl.rs` / `scroll.rs` /
//! `none.rs`，共用的几何与影子在 `geometry.rs`，本份文件只留 trait、映射表与「这一档在
//! 这种摆法下走不走」那一条判据。搬进搬出都不改式子：改一档只看它自己那一页。
//!
//! 档位来自全局配置 [`folio_core::PageTurn`]，[`of`] 是唯一的映射处（与
//! [`PageMode::paging`](crate::ui::features::reader::model::turn::PageMode::paging) 同一个形状）：加一档要多写一个
//! 实现并在那里接上，`match` 不带通配臂，漏接就是编译错误。

use folio_core::PageTurn;

use crate::ui::features::reader::model::chrome::PageMode;

mod cover;
mod curl;
mod geometry;
mod none;
mod scroll;
mod slide;
mod slide_up;

#[cfg(test)]
pub(in crate::ui::features::reader) mod testing;

use cover::Cover;
use curl::Curl;
use none::Immediate;
use scroll::Scroll;
use slide::Slide;
use slide_up::SlideUp;

pub(in crate::ui::features::reader) use geometry::{Area, Axis, Crease, Cut, Poly, Shade, Shades, Sheet, Tint};

/// 一档翻页动画。
pub(in crate::ui::features::reader) trait PageTurnStyle: Sync {
    /// 这一屏在这种摆法下有没有一趟要走。`false` = 当场落定：既不必多摆一张纸
    /// （省一整趟切片重摆与重排），`page_shift` 也就保持归零，命中不用减任何东西。
    ///
    /// 要带 `mode` 是因为这一条答案本身就依赖摆法：「上下滚动」那一档在连续里就是
    /// 它自己（补间画出来的位置），在分页里没有东西可滚。
    fn animates(&self, mode: PageMode) -> bool;

    /// 量距离、摆位移沿哪根轴。
    fn axis(&self) -> Axis;

    /// `t` ∈ [0,1] —— 已经过完缓动，0 是刚起步、1 是走完；`forward` = 沿读书顺序往后。
    /// 给回 `[进场, 出场]` 两张纸。
    fn sheets(&self, t: f32, forward: bool, area: Area) -> [Sheet; 2];

    /// 除了那两张纸，这一帧还要往它们**之上**盖哪几块（投影、纸边光、折起来的纸面）。
    ///
    /// 为什么不是 [`Sheet`] 的一格：两张纸的字形 runs 烘在同一个元素里（`view.rs` 里一个
    /// `GlyphsElement` 收两趟 `extend`），纸与纸之间没有可插入的绘制层，所以影子只能作为
    /// 盖在两者之上的一组形状来画。默认给空 —— 平移那一档两张纸之间根本没有压着谁，
    /// 给它加一条影子就是凭空多一条没人看得见的边。
    fn shades(&self, _: f32, _: bool, _: Area) -> Shades {
        Shades::new()
    }
}

/// 配置里那一档对应哪一份摆位。加一档必须在这里接上（`match` 没有通配臂）。
pub(in crate::ui::features::reader) fn of(turn: PageTurn) -> &'static dyn PageTurnStyle {
    static SLIDE: Slide = Slide;
    static SLIDE_UP: SlideUp = SlideUp;
    static COVER: Cover = Cover;
    static CURL: Curl = Curl;
    static SCROLL: Scroll = Scroll;
    static IMMEDIATE: Immediate = Immediate;
    match turn {
        PageTurn::Slide => &SLIDE,
        PageTurn::SlideUp => &SLIDE_UP,
        PageTurn::Cover => &COVER,
        PageTurn::Curl => &CURL,
        PageTurn::Scroll => &SCROLL,
        PageTurn::None => &IMMEDIATE,
    }
}

/// 这一档在当前摆法下有没有一趟要走。底栏那一格按它筛选项：连续里只剩「上下滚动」与
/// 「不走动画」两档讲得通，分页里「上下滚动」讲不通。
pub(in crate::ui::features::reader) fn applies(turn: PageTurn, mode: PageMode) -> bool {
    match turn {
        // 「不走动画」在任何摆法都是一个答案，不算「不适用于此摆法」。
        PageTurn::None => true,
        other => of(other).animates(mode),
    }
}


#[cfg(test)]
mod tests {
    //! 搬自摆位层那一份：一挡的门跟着那一档走，共用的尺在 `testing.rs`。

    use crate::ui::features::reader::model::chrome::PageMode;
    use crate::ui::features::reader::motion::turn_animation::cover::{Cover};
    use crate::ui::features::reader::motion::turn_animation::curl::{Curl};
    use crate::ui::features::reader::motion::turn_animation::curl::{crease_of};
    use crate::ui::features::reader::motion::turn_animation::geometry::{Area, Axis};
    use crate::ui::features::reader::motion::turn_animation::none::{Immediate};
    use crate::ui::features::reader::motion::turn_animation::scroll::{Scroll};
    use crate::ui::features::reader::motion::turn_animation::slide::{Slide};
    use crate::ui::features::reader::motion::turn_animation::slide_up::{SlideUp};
    use crate::ui::features::reader::motion::turn_animation::testing::{page};
    use crate::ui::features::reader::motion::turn_animation::{of, applies};
    use crate::ui::features::reader::motion::turn_animation::PageTurnStyle as _;
    use folio_core::PageTurn;

    /// 「走不走一趟」是**按摆法**答的：`None` 哪儿都不走；「上下滚动」只在连续里走
    /// （分页里没有可滚的东西，那一趟退化成当场落定）；其余几档两种摆法都要摆那两张纸。
    /// 底栏那一格的选项就是按这条筛的，所以它错一格，界面上就少一档或多一档。
    #[test]
    fn which_styles_run_a_slide_is_an_answer_per_layout() {
        for mode in [PageMode::Continuous, PageMode::Paged, PageMode::Spread] {
            assert!(
                !Immediate.animates(mode),
                "{mode:?} 里「不走动画」还走了一趟"
            );
            assert!(
                Slide.animates(mode)
                    && SlideUp.animates(mode)
                    && Cover.animates(mode)
                    && Curl.animates(mode),
                "{mode:?} 里那几张纸的档有一档说不走了"
            );
            assert_eq!(
                Scroll.animates(mode),
                mode.is_continuous(),
                "{mode:?} 里上下滚动该不该走算错了"
            );
        }
    }
    /// 每个摆法都至少留下一档可选 —— 全筛掉的话底栏那颗按钮点开是空的。
    #[test]
    fn some_style_always_applies() {
        for mode in [PageMode::Continuous, PageMode::Paged, PageMode::Spread] {
            let kept: Vec<_> = PageTurn::ALL
                .iter()
                .copied()
                .filter(|turn| applies(*turn, mode))
                .collect();
            assert!(!kept.is_empty(), "{mode:?} 里一档都不剩");
        }
    }
    /// 每一档都接得上，而且轴与「走不走」这两条对外承诺不会串档。加一档忘了在
    /// [`of`] 里接是编译错误，这条守的是接错那一半。
    #[test]
    fn every_configured_style_maps_to_its_own_axis() {
        let cases = [
            (PageTurn::Slide, Axis::Horizontal),
            (PageTurn::SlideUp, Axis::Vertical),
            (PageTurn::Cover, Axis::Horizontal),
            (PageTurn::Curl, Axis::Horizontal),
            (PageTurn::Scroll, Axis::Vertical),
            (PageTurn::None, Axis::Horizontal),
        ];
        for (turn, axis) in cases {
            assert_eq!(of(turn).axis(), axis, "{turn:?} 的轴不对");
            // 漏挂进 `ALL` 的那一档在界面上永远选不到，而配置里手写它又能读回来 ——
            // 就成了一个只有改文件才到达的影子档。
            assert!(PageTurn::ALL.contains(&turn), "{turn:?} 没挂进 PageTurn::ALL");
        }
    }
    /// 距离按轴取：横档取宽、竖档取高。取错那个数不会报错，只会让纸走半屏就落定。
    #[test]
    fn the_distance_follows_the_axis_not_the_screen() {
        let area = Area::new(0.0, 0.0, 900.0, 400.0);
        assert_eq!(area.distance(Axis::Horizontal), 900.0);
        assert_eq!(area.distance(Axis::Vertical), 400.0);
    }
    /// 每一档该掉几块影子是有数的：平移两张纸之间没有压着谁 ⇒ 一块都不该有；覆盖两块
    /// （前缘投影 + 纸边光）；折页五块。默认实现是空，
    /// 所以这条守的是「哪天顺手给平移也接上一条影子」—— 那种改法不会让任何一处现有
    /// 断言变红，只会在屏幕上多出一条没人看得见的边。
    #[test]
    fn each_style_sheds_the_shades_it_ows() {
        for t in [0.0, 0.35, 0.5, 0.9] {
            assert!(
                Slide.shades(t, true, page()).items().is_empty(),
                "平移里两张纸相距一整屏，没有谁压着谁"
            );
            assert!(SlideUp.shades(t, true, page()).items().is_empty());
            assert!(Scroll.shades(t, true, page()).items().is_empty());
            assert!(Immediate.shades(t, true, page()).items().is_empty());
            // 平移两张纸始终相距一整屏，没有重叠 ⇒ 不该裁任何东西（裁了就是白丢一列字）。
            for sheet in Slide.sheets(t, true, page()) {
                assert_eq!(sheet.cut, None, "平移不重叠，不该裁");
            }
            for sheet in SlideUp.sheets(t, true, page()) {
                assert_eq!(sheet.cut, None, "竖平移同样不重叠");
            }
            // 反过来：凡是真会重叠的那两档，两张纸都必须各有一条裁剪 —— 少一张就是字叠字。
            for mut style in [
                Cover.sheets(t, true, page()),
                Curl.sheets(t, true, page()),
            ] {
                for sheet in style.iter_mut() {
                    assert!(
                        sheet.cut.is_some(),
                        "重叠的档里有一张纸没裁：它的字会与另一张叠在一起 {sheet:?}"
                    );
                }
                assert!(
                    style[0].cut != style[1].cut,
                    "两张纸裁的是同一侧，压住那一段会两页都有字"
                );
            }
            // 覆盖的位移档不看进度有没有起步：前缘一上屏就该带影子。
            assert_eq!(Cover.shades(t, true, page()).items().len(), 2);
        }
        // 折页那一条从一块（起步）走到五块（折起来之后），落位归零，全在上面那条测试里。
        assert_eq!(Curl.shades(0.0, true, page()).items().len(), 1);
        assert_eq!(Curl.shades(0.5, true, page()).items().len(), 5);
    }
    /// 版心还没量出来（宽或高为 0）时不该起任何几何：既不能 panic，也不能画出一张
    /// 凭空出现的纸或一条没人看得见的投影。这一格在真机上是**会发生的** —— 首帧布局前
    /// 就有指针事件。两档都要过，因为覆盖那一条不走折线、有自己的一条早退路径。
    #[test]
    fn an_unmeasured_page_sheds_nothing() {
        let flat = Area::new(0.0, 0.0, 0.0, 0.0);
        assert!(crease_of(0.5, true, flat).is_none());
        for style in [
            Curl.shades(0.5, true, flat),
            Cover.shades(0.5, true, flat),
        ] {
            assert!(style.items().is_empty(), "量不出面积时一块都不该画");
        }
        for sheet in Curl.sheets(0.5, true, flat) {
            assert_eq!(sheet.cut, None, "量不出面积时两张纸都整张画");
            assert_eq!(sheet.offset, (0.0, 0.0));
        }
    }
}
