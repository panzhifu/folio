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

/// 不带弹回时钟的那一条入口：生产那一路走 [`cylinder_for`]，这条只剩门在读。
#[cfg(test)]
pub(in crate::ui::features::reader) use curl::cylinder_at;
pub(in crate::ui::features::reader) use curl::{cylinder_for, release_grip};
pub(in crate::ui::features::reader) use geometry::{
    Area, Axis, Crease, Cut, Grip, Poly, Pose, Shades, Sheet, Tint,
};

/// 一档翻页动画。
pub(in crate::ui::features::reader) trait PageTurnStyle:
    Sync
{
    /// 这一屏在这种摆法下有没有一趟要走。`false` = 当场落定：既不必多摆一张纸
    /// （省一整趟切片重摆与重排），`page_shift` 也就保持归零，命中不用减任何东西。
    ///
    /// 要带 `mode` 是因为这一条答案本身就依赖摆法：「上下滚动」那一档在连续里就是
    /// 它自己（补间画出来的位置），在分页里没有东西可滚。
    fn animates(&self, mode: PageMode) -> bool;

    /// 同上，但带上读者这一笔的手与那本书的阅读方向（见 [`Pose`]）。
    ///
    /// 只有折页那一档用得上：别的档纸是整张平移/覆盖的，起点在哪儿、书从哪边读都不改变几何，
    /// 所以默认实现只把 [`Pose::forward`] 转给 [`Self::sheets`]，一个字都不必改。
    fn sheets_at(&self, t: f32, area: Area, pose: Pose) -> [Sheet; 2] {
        self.sheets(t, pose.forward, area)
    }

    /// [`Self::shades`] 的带手版本，同上。
    fn shades_at(&self, t: f32, area: Area, pose: Pose) -> Shades {
        self.shades(t, pose.forward, area)
    }

    /// 量距离、摆位移沿哪根轴。
    fn axis(&self) -> Axis;

    /// 这根轴上的拖动算不算「翻页」。
    ///
    /// 默认只认 [`Self::axis`] 那一根（平移/覆盖/上下平移都是整张纸沿轴走，斜着划没有意义）。
    /// **折页两轴都认**：它的折线朝向由这一笔的划动向量给，斜着划本来就对 —— 而且手机上没人会
    /// 把手指拖成一条严格的水平线。从前它只认横轴，于是斜划被判成「竖向拖动」直接让开，读者的
    /// 读数是「不跟手 / 根本翻不动」。
    fn claims(&self, axis: Axis) -> bool {
        axis == self.axis()
    }

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

/// 松手那一腿自己走了几成：`0` = 松手那一刻那一格，`1` = 这一腿到底（走完或退回按下点）。
///
/// 两条腿的尺**不是同一个**，因为 `progress` 走的方向相反：走完那一腿从 `from` 涨到 `1`，
/// 弹回那一腿从 `from` 退到 `0`。两头都按 `1 − from` 归一时，弹回那一腿算出来是负的，下游
/// 那一格 `clamp` 就把落点冻在松手那一点上 —— 整趟纸一动不动，而 `release_duration` 那
/// 120 ms 还在走，读者读到的是「先跳一下，再停一下」。`from` 为 0（一按就松手）时这一腿本来
/// 没有路要走，直接给 0，不留一个除零。
///
/// 为什么不夹进 `0..1`：这一格的上下界由 `progress` 与 `from` 自己保证，而夹住会把「时钟跑过头
/// 那一帧」这种真错误读成正常终值。要用的那一头（[`release_grip`]）自己夹。
pub(in crate::ui::features::reader) fn release_clock(
    progress: f32,
    from: f32,
    revert: bool,
) -> f32 {
    if revert {
        if from > 0.0 {
            (from - progress) / from
        } else {
            0.0
        }
    } else {
        (progress - from) / (1.0 - from).max(1e-3)
    }
}

#[cfg(test)]
mod tests {
    //! 搬自摆位层那一份：一挡的门跟着那一档走，共用的尺在 `testing.rs`。

    use crate::ui::features::reader::model::chrome::PageMode;
    use crate::ui::features::reader::motion::turn_animation::PageTurnStyle as _;
    use crate::ui::features::reader::motion::turn_animation::cover::Cover;
    use crate::ui::features::reader::motion::turn_animation::curl::Curl;
    use crate::ui::features::reader::motion::turn_animation::curl::crease_of;
    use crate::ui::features::reader::motion::turn_animation::geometry::{Area, Axis};
    use crate::ui::features::reader::motion::turn_animation::none::Immediate;
    use crate::ui::features::reader::motion::turn_animation::scroll::Scroll;
    use crate::ui::features::reader::motion::turn_animation::slide::Slide;
    use crate::ui::features::reader::motion::turn_animation::slide_up::SlideUp;
    use crate::ui::features::reader::motion::turn_animation::testing::page;
    use crate::ui::features::reader::motion::turn_animation::{applies, of, release_clock};
    use folio_core::PageTurn;

    /// 翻页的轴**归动画档**，不归手指。这两问的答案就是那条判据的全部：
    ///
    /// - `axis` = 走了多远**沿哪根轴**归一（也是动画那两张纸走的轴），由档位自己给；
    /// - `claims` = 手指那根轴上的拖动算不算翻页。读者选了横向的平移 / 覆盖，竖着划不该翻页；
    ///   选了上下平移，横着划不该翻页。
    ///
    /// 折页是 `claims` 唯一的例外（一笔斜划也得算翻页），但它的 `axis` 仍只有一根横的 —— 翻的是
    /// 一张横排页。这两问从前由一处 `page_axis(turn, mode)` 合并答，于是「两轴都认」与「按哪根
    /// 归一」打架：一笔竖着划的按页高归一，纸几乎没卷而进度已经过半，读出来是「页翻了纸没动」。
    #[test]
    fn the_page_axis_comes_from_the_style_not_the_finger() {
        use Axis::{Horizontal, Vertical};
        for (turn, want) in [
            (PageTurn::Slide, Horizontal),
            (PageTurn::Cover, Horizontal),
            (PageTurn::Curl, Horizontal),
            (PageTurn::SlideUp, Vertical),
            // 「上下滚动」在分页里本就不成立（`applies` 会把它从选项里筛掉），真落进来按竖轴；
            // 「不走动画」没有一趟要走，那一格不被读到，给个确定答案就够了。
            (PageTurn::Scroll, Vertical),
            (PageTurn::None, Horizontal),
        ] {
            let style = of(turn);
            assert_eq!(style.axis(), want, "{turn:?} 的翻页该沿那根轴量距离");
            let claimed = [Horizontal, Vertical]
                .into_iter()
                .filter(|axis| style.claims(*axis))
                .collect::<Vec<_>>();
            if turn == PageTurn::Curl {
                assert_eq!(
                    claimed.len(),
                    2,
                    "折页只认一根轴：斜划又被判成竖向拖动直接让开"
                );
            } else {
                assert_eq!(claimed, [want], "{turn:?} 只该认它自己那根轴");
            }
        }
    }

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
            assert!(
                PageTurn::ALL.contains(&turn),
                "{turn:?} 没挂进 PageTurn::ALL"
            );
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
    /// （前缘投影 + 纸边光）；折页零块（纸面与投影都在 GPU 那张网格里）。默认实现是空，
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
            for mut style in [Cover.sheets(t, true, page()), Curl.sheets(t, true, page())] {
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
        // 折页那一档**一块 CPU 影子都不掉**：纸面、受光与投影全在 GPU 那张网格里画
        // （`curl.rs` 的 `curl_of` + `page_curl.wgsl`）。这里守的是「别又把那套平色带接回来」——
        // 接回来不会让任何画面变好，只会在网格之上多盖一层假卷筒。
        for t in [0.0, 0.35, 0.5, 0.9, 1.0] {
            assert!(
                Curl.shades(t, true, page()).items().is_empty(),
                "折页那一档又掉了 {t} 块的 CPU 影子"
            );
        }
    }
    /// 版心还没量出来（宽或高为 0）时不该起任何几何：既不能 panic，也不能画出一张
    /// 凭空出现的纸或一条没人看得见的投影。这一格在真机上是**会发生的** —— 首帧布局前
    /// 就有指针事件。两档都要过，因为覆盖那一条不走折线、有自己的一条早退路径。
    #[test]
    fn an_unmeasured_page_sheds_nothing() {
        let flat = Area::new(0.0, 0.0, 0.0, 0.0);
        assert!(crease_of(0.5, true, flat).is_none());
        for style in [Curl.shades(0.5, true, flat), Cover.shades(0.5, true, flat)] {
            assert!(style.items().is_empty(), "量不出面积时一块都不该画");
        }
        for sheet in Curl.sheets(0.5, true, flat) {
            assert_eq!(sheet.cut, None, "量不出面积时两张纸都整张画");
            assert_eq!(sheet.offset, (0.0, 0.0));
        }
    }

    /// 松手那两条腿各有一把自己的尺，而且**首帧必须是 0**：走完那一腿 `progress` 从 `from` 涨到
    /// `1`，弹回那一腿从 `from` 退到 `0`。两头共用 `1 − from` 时弹回算出来是负的，下游一夹就
    /// 变成 0 —— 于是整条腿的落点冻在松手那一刻，纸一动不动，而这一腿的 120 ms 还在走。
    #[test]
    fn each_release_leg_gets_its_own_clock() {
        for from in [0.2f32, 0.5, 0.9] {
            // 走完：起点 = 松手那一刻，终点 = 走满。
            assert_eq!(release_clock(from, from, false), 0.0, "走完的第一帧不是 0");
            assert!(
                (release_clock(1.0, from, false) - 1.0).abs() < 1e-5,
                "走完的终帧不是 1"
            );
            let mid = (from + 1.0) * 0.5;
            assert!(
                (release_clock(mid, from, false) - 0.5).abs() < 1e-5,
                "走完走了一半，时钟该给 0.5，实得 {}",
                release_clock(mid, from, false)
            );
            // 弹回：起点同样是松手那一刻，终点是退回按下点。
            assert_eq!(release_clock(from, from, true), 0.0, "弹回的第一帧不是 0");
            assert!(
                (release_clock(0.0, from, true) - 1.0).abs() < 1e-5,
                "弹回的终帧不是 1"
            );
            assert!(
                (release_clock(from * 0.5, from, true) - 0.5).abs() < 1e-5,
                "弹回退了一半，时钟该给 0.5，实得 {}",
                release_clock(from * 0.5, from, true)
            );
            // 每一格都得是有限数：`from` 贴在 1 那一侧时靠的是分母那一格的下限。
            for progress in [0.0, from * 0.5, from, (from + 1.0) * 0.5, 1.0] {
                for revert in [false, true] {
                    assert!(
                        release_clock(progress, from, revert).is_finite(),
                        "progress = {progress}、from = {from}、revert = {revert} 算出了非有限数"
                    );
                }
            }
        }
        // 一按就松手：这一腿本来没有路要走，给 0 而不是除零。
        assert_eq!(
            release_clock(0.0, 0.0, true),
            0.0,
            "from = 0 的弹回不该是 NaN"
        );
        assert!(
            release_clock(0.0, 0.0, false).is_finite(),
            "from = 0 的走完不该是 NaN"
        );
    }
}
