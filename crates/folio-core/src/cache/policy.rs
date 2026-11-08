//! 图集驱逐的纯策略：额度、设备分档、焦点距离、帧末挤出顺序。
//!
//! 这里没有一个像素，也没有 gpui：调用方把「每一格多大、上次在场是哪一帧」
//! 交进来，这里只回答「放掉哪几格」。所以设备分档与挤出顺序能在 core 的单元
//! 测试里离线跑，不必起窗口，也不必真 GPU —— 这正是把它从
//! `folio-app` 的 `image_atlas` 里搬出来的理由。
//!
//! 判据与它在 app 里时逐条相同：先按「离场满 [`HELD_AFTER_LEAVE`] 帧」收，
//! 再按字节封顶；封顶时先挤离焦点页最远的，同距挤最久没在场的；高清与低清
//! 各记各的账。唯一修正的是设备分档：从前移动端无论多宽都算手机、桌面宽窗
//! 反而算平板，与两条额度注释的承诺相反 —— 现在按「移动端看宽度、桌面看
//! 平台」分，与注释一致。

use std::collections::HashSet;
use std::sync::RwLock;

/// 一张图离开元素树之后，还留着不放的那几帧。
///
/// 留这几帧不是为了省解码，是为了**不赌渲染帧的连续性**：换模式、邻章排完补进链、
/// 浮层收起那一瞬，都可能有一帧一个 `img` 都不产出。真在那一帧就把格子放掉，
/// 下一帧又传回来，读者看到的是图在闪。
///
/// 一次翻页动画约 280 ms（≈17 帧）—— 留 30 帧，翻过去再翻回来时那一页还在手里，
/// 不必为同一页重付一趟栅格化（手机上一页是好几秒）。再往前留的由软顶那一档
/// 按「离焦点页多远、其次最久没在场」挤出，所以这里放大不会让内存无界。
pub const HELD_AFTER_LEAVE: u64 = 30;

/// 宽到这个逻辑像素数以上算平板（iPad 竖屏 768、横屏 1024；Android 平板常见 800–1280）。
pub const TABLET_MIN_WIDTH: f32 = 1000.0;

/// 设备分档。判据只有「是不是移动端」与屏幕逻辑宽两条：平板与手机的差别在
/// 「一页摆出来多大」，桌面与移动平台的差别在显存预算，按目标平台分。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceClass {
    Phone,
    Tablet,
    Desktop,
}

/// 手里攒的像素分两档记：**高清**（真画到屏上那一份）与**低清**（先顶着的那一份糊的）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    Full,
    Preview,
}

/// 当前窗口的逻辑宽度，由绘制侧每帧写一次。
///
/// 内存档位是**设备**的属性，不是「这一本图集」的属性；而图集是按书建的，
/// 测试里一次能建好几个。宽度只是用来分「平板 / 手机」那一档，写晚了最多
/// 影响一轮回收，不会画错东西。
static SCREEN_WIDTH: RwLock<Option<f32>> = RwLock::new(None);

/// 绘制侧每帧把当前逻辑宽度推过来一次。
pub fn set_screen_width(width: f32) {
    if let Ok(mut slot) = SCREEN_WIDTH.write() {
        *slot = Some(width);
    }
}

fn screen_width() -> Option<f32> {
    *SCREEN_WIDTH.read().unwrap_or_else(|e| e.into_inner())
}

/// 按目标平台与逻辑宽度分档。`mobile` 由调用点按 `cfg!(target_os)` 递进来，
/// 测试不必真的跑在手机上也能钉住两个方向。
pub fn classify(mobile: bool, width: Option<f32>) -> DeviceClass {
    if !mobile {
        return DeviceClass::Desktop;
    }
    match width {
        Some(width) if width >= TABLET_MIN_WIDTH => DeviceClass::Tablet,
        _ => DeviceClass::Phone,
    }
}

/// 当前进程（当前窗口）的设备档。
pub fn device_class() -> DeviceClass {
    let mobile = cfg!(any(target_os = "android", target_os = "ios"));
    classify(mobile, screen_width())
}

/// 手里最多攒多少字节的像素（含还没上传的）—— **软顶**，按设备分档。
///
/// 一台手机上 24 MB ≈ 六页 1024² 的纹理，或两页 2048×1400 的漫画页；桌面给 128 MB，
/// 因为桌面的显存与内存都比这个数大一个量级，卡在 24 MB 会让「翻回去看上一页」总是
/// 重画。平板取中间那一档（48 MB）：屏幕比手机大一截，一页的像素就是两三倍。
///
/// 定这个数不是为了卡住「正常一屏」—— 正常一屏到不了它 —— 是为了那一类一页摆二十张
/// 源图的大画册页：那种屏按可见带算就是几十张全在场，而一张 2048×1500 就是 12 MB。
///
/// 它**只能挤已经离场的那些**：把正在屏幕上的图放掉，下一帧就得重传，再放掉……
/// 每一帧重解一张 12 MB 的图，那是比占着显存更糟的结果。
pub fn soft_limit(class: DeviceClass) -> usize {
    match class {
        DeviceClass::Phone => 24 * 1024 * 1024,
        DeviceClass::Tablet => 48 * 1024 * 1024,
        DeviceClass::Desktop => 128 * 1024 * 1024,
    }
}

/// 软顶不够装下**眼前这一页**时能涨到的那一档。
///
/// 只在「焦点页（或这一屏的几页）自己就要这么多」时才涨：手机上放大到 4x 的
/// 4096 区域图一张就是 64 MB，而那是读者**正看着**的那一张，挤掉它等于什么都没做。
/// 涨到 2 倍仍装不下就不再涨 —— 那是「这一页本来就超出预算」的问题，不该拿无界的
/// 内存去兜。
pub fn hard_of(soft: usize) -> usize {
    soft * 2
}

/// 低清那一档自己的额度。
///
/// 一张 360×270 的 RGBA 是 0.39 MB，所以手机这一档装得下**二十来页**的占位图，
/// 而同一台机器的高清那一档只装得下两页（24 MB / 12.6 MB）。两档各记各的账是这一条
/// 成立的前提：把低清跟高清塞进同一个帽子，要么低清先被挤光（白屏回来了），
/// 要么帽子整体变大（内存回不到原样）。
///
/// 配置文件里那一格（`reader.image_cache_bytes`）**只覆盖高清那一档**，低清跟着设备分档
/// —— 那一格是给「我显存大，多留几页高清」用的，不是给占位图用的。
pub fn preview_limit(class: DeviceClass) -> usize {
    match class {
        DeviceClass::Phone => 8 * 1024 * 1024,
        DeviceClass::Tablet => 12 * 1024 * 1024,
        DeviceClass::Desktop => 24 * 1024 * 1024,
    }
}

/// 低清那一档的键长什么样：`<这本书的 token>/<容器路径>@preview`
/// （拼法见 app 侧 `foundation::images::DecodeRequest::preview`）。
///
/// 定版式那一路的预览键是 `render/<page>@720` 那一族，**不**带这个后缀，所以它照旧记在
/// 高清那一档里 —— 那是它从前的口径，这一刀不动它（动了就会改变 PDF/CBZ 的挤出顺序，
/// 而那一族本来就有两档）。
pub fn tier_of(key: &str) -> Tier {
    if key.ends_with("@preview") {
        Tier::Preview
    } else {
        Tier::Full
    }
}

/// 这一格离焦点页多远（越大越先被挤出）。键里带着页号，见 app 侧
/// `foundation::images` 的 `render_job`（`<token>/render/<page>@<edge>`）；
/// 容器图没有页号，一律给 `None`。
///
/// 键是**字符串**，所以这里只能按格式拆。拆不出来（或这一格不是渲染页）给 `None`，
/// 排序时 `0` 排在「有距离」的前面 —— 容器图那一族本来就按「最久没在场」走，
/// 与从前一样。
pub fn distance_of(key: &str, focus: Option<usize>) -> usize {
    let Some(focus) = focus else {
        return 0;
    };
    let Some(rest) = key.split_once("/render/").map(|(_, rest)| rest) else {
        return 0;
    };
    let page: Option<usize> = rest
        .split_once('@')
        .and_then(|(page, _)| page.parse().ok())
        .or_else(|| rest.split_once('/').and_then(|(page, _)| page.parse().ok()));
    match page {
        Some(page) => page.abs_diff(focus),
        None => 0,
    }
}

/// 一格里与「该不该放」有关的三个数：身份、占多少字节、上一次在场是哪一帧。
///
/// 只收 `Held`（像素在手）的格子；还在跑的与解失败的不占字节，调用方不该递进来。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry<'a> {
    pub key: &'a str,
    pub bytes: usize,
    pub seen: u64,
}

/// 帧末那一趟的输入。
pub struct Frame<'a> {
    /// 手里握着的格子。
    pub entries: &'a [Entry<'a>],
    /// 这一帧在场的键。
    pub on_screen: &'a HashSet<String>,
    /// 上一帧在场的键 —— 与 `on_screen` 一起构成「最近两帧都没上场」。
    pub previous: &'a HashSet<String>,
    /// 当前帧号。
    pub frame: u64,
    /// 这一帧读者在看哪一页；容器图那一族是 `None`。
    pub focus: Option<usize>,
    /// 高清那一档此刻占了多少字节。
    pub bytes: usize,
    /// 低清那一档此刻占了多少字节。
    pub preview_bytes: usize,
    /// 高清那一档的软顶（配置覆盖后的生效值）。
    pub soft: usize,
    /// 低清那一档的额度。
    pub preview_cap: usize,
}

impl<'a> Frame<'a> {
    /// 这一帧该放掉的键，按放的先后排列。
    ///
    /// 两段：先收「离场满 [`HELD_AFTER_LEAVE`] 帧」的；再按字节封顶，从离焦点页
    /// 最远的那一格开始挤，够到额度就停。在屏的（这一帧与上一帧）永远不碰。
    pub fn plan(&self) -> Vec<&'a str> {
        let mut stale: Vec<&str> = self
            .entries
            .iter()
            .filter(|entry| {
                !self.on_screen.contains(entry.key)
                    && !self.previous.contains(entry.key)
                    && self.frame.saturating_sub(entry.seen) > HELD_AFTER_LEAVE
            })
            .map(|entry| entry.key)
            .collect();

        let focus_bytes = self.focus_bytes();
        for tier in [Tier::Full, Tier::Preview] {
            let limit = self.cap_of(tier, focus_bytes);
            let mut spare = match tier {
                Tier::Full => self.bytes,
                Tier::Preview => self.preview_bytes,
            };
            if spare <= limit {
                continue;
            }
            let mut ordered: Vec<&Entry<'a>> = self
                .entries
                .iter()
                .filter(|entry| !self.on_screen.contains(entry.key) && tier_of(entry.key) == tier)
                .collect();
            ordered.sort_unstable_by(|left, right| {
                // 远的先挤；同距先挤最久没在场的（`seen` 小的）；最后按键定序，
                // 让同一帧的答案与 `HashMap` 的迭代顺序无关。
                distance_of(right.key, self.focus)
                    .cmp(&distance_of(left.key, self.focus))
                    .then(left.seen.cmp(&right.seen))
                    .then(left.key.cmp(right.key))
            });
            for entry in ordered {
                if spare <= limit {
                    break;
                }
                spare = spare.saturating_sub(entry.bytes);
                if !stale.contains(&entry.key) {
                    stale.push(entry.key);
                }
            }
        }
        stale
    }

    /// 在屏的高清格子里，离焦点页最近（距离 0）的那些一共多少字节。
    fn focus_bytes(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| {
                self.on_screen.contains(entry.key)
                    && tier_of(entry.key) == Tier::Full
                    && distance_of(entry.key, self.focus) == 0
            })
            .map(|entry| entry.bytes)
            .sum()
    }

    /// 这一档本轮能用多少字节。
    ///
    /// 高清那一档保留「软顶 → 硬顶」那两段：软顶不够装下**焦点页自己**时涨到两倍
    /// （[`hard_of`]），涨了仍不够就不再涨（那一页本来就超出预算，不拿无界内存兜）。
    /// 低清那一档不涨 —— 它的作用就是让屏上不是白纸，那份额度本来就是按「多留几页」定的。
    fn cap_of(&self, tier: Tier, focus_bytes: usize) -> usize {
        match tier {
            Tier::Preview => self.preview_cap,
            Tier::Full if focus_bytes > self.soft => hard_of(self.soft),
            Tier::Full => self.soft,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DeviceClass, Entry, Frame, Tier, classify, hard_of, preview_limit, soft_limit, tier_of,
    };
    use std::collections::HashSet;

    fn keys(names: &[&str]) -> HashSet<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }

    fn entry<'a>(key: &'a str, bytes: usize, seen: u64) -> Entry<'a> {
        Entry { key, bytes, seen }
    }

    /// 移动端看宽度、桌面看平台：这是两条额度注释的承诺，也是从前写反了的那一格。
    #[test]
    fn device_tiers_follow_platform_and_width() {
        assert_eq!(classify(true, Some(384.0)), DeviceClass::Phone);
        assert_eq!(classify(true, Some(800.0)), DeviceClass::Phone);
        assert_eq!(classify(true, Some(1280.0)), DeviceClass::Tablet);
        assert_eq!(classify(true, None), DeviceClass::Phone);
        assert_eq!(classify(false, Some(384.0)), DeviceClass::Desktop);
        assert_eq!(classify(false, Some(1280.0)), DeviceClass::Desktop);
        assert_eq!(classify(false, None), DeviceClass::Desktop);

        assert_eq!(soft_limit(DeviceClass::Phone), 24 * 1024 * 1024);
        assert_eq!(soft_limit(DeviceClass::Tablet), 48 * 1024 * 1024);
        assert_eq!(soft_limit(DeviceClass::Desktop), 128 * 1024 * 1024);
        assert_eq!(preview_limit(DeviceClass::Phone), 8 * 1024 * 1024);
        assert_eq!(hard_of(soft_limit(DeviceClass::Phone)), 48 * 1024 * 1024);
    }

    /// 低清那一档认 `@preview` 后缀；容器图与定版式页都在高清那一档。
    #[test]
    fn preview_tier_is_the_suffix_and_nothing_else() {
        assert_eq!(tier_of("tok/pic.png@preview"), Tier::Preview);
        assert_eq!(tier_of("tok/pic.png"), Tier::Full);
        assert_eq!(tier_of("tok/render/7@720"), Tier::Full);
        assert_eq!(tier_of("tok/render/7@720@preview"), Tier::Preview);
    }

    /// 键里的页号 → 离焦点页的距离；拆不出来的一律 0（容器图按「最久没在场」走）。
    #[test]
    fn the_eviction_key_is_distance_from_the_focused_page() {
        use super::distance_of;
        let key = |page: usize, edge: u32| format!("tok/render/{page}@{edge}");
        assert_eq!(distance_of(&key(0, 2048), Some(0)), 0);
        assert_eq!(distance_of(&key(3, 2048), Some(0)), 3);
        assert_eq!(distance_of(&key(0, 2048), Some(3)), 3);
        assert_eq!(
            distance_of("tok/figure-3.png", Some(3)),
            0,
            "容器图没有页号：与从前一样按「最久没在场」排"
        );
        assert_eq!(
            distance_of(&key(3, 720), None),
            0,
            "没有焦点页（容器图那一族）时全部同距"
        );
        assert_eq!(
            distance_of("tok/render/7@4096/10,20,30,40", Some(1)),
            6,
            "区域图那种带斜杠的键也要拆得出页号"
        );
    }

    /// 离场满 grace 的才进第一刀；在屏的、上一帧在屏的都不碰。
    #[test]
    fn only_slots_absent_for_the_grace_are_reclaimed() {
        let entries = [
            entry("on", 10, 0),
            entry("previous", 10, 0),
            entry("young", 10, 80),
            entry("old", 10, 10),
        ];
        let on = keys(&["on"]);
        let previous = keys(&["previous"]);
        let frame = Frame {
            entries: &entries,
            on_screen: &on,
            previous: &previous,
            frame: 100,
            focus: None,
            bytes: 40,
            preview_bytes: 0,
            soft: 1024,
            preview_cap: 1024,
        };
        assert_eq!(frame.plan(), vec!["old"]);
    }

    /// 封顶时先挤离焦点页最远的，同距再挤最久没在场的；够到额度就停。
    #[test]
    fn the_cap_evicts_farthest_first_and_stops_at_the_limit() {
        let entries = [
            entry("tok/render/1@2048", 10, 1),
            entry("tok/render/5@2048", 10, 1),
            entry("tok/render/9@2048", 10, 1),
        ];
        let on = keys(&[]);
        let previous = keys(&[]);
        let frame = Frame {
            entries: &entries,
            on_screen: &on,
            previous: &previous,
            frame: 1,
            focus: Some(1),
            bytes: 30,
            preview_bytes: 0,
            soft: 20,
            preview_cap: 1024,
        };
        // 30 > 20，只放离得最远的第 9 页就够；第 1 页留在手里。
        assert_eq!(frame.plan(), vec!["tok/render/9@2048"]);
    }

    /// 高清与低清各记各的账：高清超顶不会挤低清的格子。
    #[test]
    fn the_full_cap_never_evicts_the_preview_tier() {
        let entries = [entry("tok/a.png", 30, 0), entry("tok/a.png@preview", 30, 0)];
        let on = keys(&[]);
        let previous = keys(&[]);
        let frame = Frame {
            entries: &entries,
            on_screen: &on,
            previous: &previous,
            frame: 100,
            focus: None,
            bytes: 30,
            preview_bytes: 30,
            soft: 20,
            preview_cap: 20,
        };
        let plan = frame.plan();
        assert!(plan.contains(&"tok/a.png"), "高清超顶该挤高清");
        assert!(
            plan.contains(&"tok/a.png@preview"),
            "低清也超自己的顶，各挤各的"
        );
    }

    /// 焦点页自己超过软顶时额度涨到硬顶；涨了仍不够也不许无界。
    #[test]
    fn a_focus_page_over_the_soft_cap_raises_to_the_hard_cap() {
        let entries = [entry("tok/render/0@4096", 30, 0)];
        let on = keys(&["tok/render/0@4096"]);
        let previous = keys(&[]);
        let frame = Frame {
            entries: &entries,
            on_screen: &on,
            previous: &previous,
            frame: 100,
            focus: Some(0),
            bytes: 30,
            preview_bytes: 0,
            soft: 20,
            preview_cap: 1024,
        };
        assert!(
            frame.plan().is_empty(),
            "焦点页 30 字节 > 软顶 20，但硬顶 40 装得下，不该挤"
        );
    }
}
