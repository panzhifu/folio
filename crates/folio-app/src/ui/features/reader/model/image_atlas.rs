//! 书里的图与 GPU 图集之间那一层**会回收**的缓存。
//!
//! ## 为什么要有这一层
//!
//! gpui 把每张画过的图放进 sprite atlas，一格的大小是**图的原始像素**（`宽 × 高 × 4`），
//! 而**没有任何一条生产可用的路把它放掉**：字形那批格子的确是永远留着（gpui 里没有删字形的口），
//! 图片这一批本来是有回收口的 —— `App::drop_image` + `RetainAllImageCache` —— 可那个口要拿到
//! 解好的 `RenderImage` 句柄，而我们从前交给 `img` 的是 `ImageSource::Image(Arc<Image>)`
//! （编码字节），解出来的那一份由窗口自己的 asset 表持有，界面碰不到。
//! `ImageSource::remove_asset` 只清那张表里的一条字节 → 像素是放了，**图集那一格照样留着**，
//! 回屏还会再传一格 —— 用它「回收」是倒贴。
//!
//! 于是《未来简史》55 张插图读完全书就是 **126.8 MB** 永久压在显存里（2026-10-15 两条独立
//! 路子数过：设备库里的 `images` 表，与 EPUB 里真实的 PNG IHDR / JPEG SOF 头，同一个总数）。
//! 一页 1024² 的纹理才 4 MB，那是三十多张纹理。
//!
//! ## 这一层做的事
//!
//! 把「谁持有像素」从窗口那张表换成**我们自己的表**：
//!
//! - 交给 `img` 的是 `ImageSource::Render(Arc<RenderImage>)`，那份像素的**唯一强引用**
//!   就在这个实体手里（`Arc::strong_count == 1` 是回收的前提，也是它成立的证据）；
//! - 解像素的那一趟跑在**后台线程**，而且用的就是 gpui 自己那个公开入口
//!   （`Image::to_image_data`）—— BGRA 换位、EXIF 方向、动图逐帧的口径与从前
//!   `ImageSource::Image` 那条路**逐字相同**，我们没有自研解码；
//! - 一张图离开可见带、又过了 [`HELD_AFTER_LEAVE`] 帧，就 `App::drop_image` 放掉那一格
//!   并把手里的像素一起扔了。回屏时自动重传（`window.rs` 那句
//!   `sprite_atlas.get_or_insert_with`），代价是重解一次 —— 后台那一趟，不在绘制帧里。
//!
//! 稳态占用的上界因此是「这一屏的图 + 离场不到 [`HELD_AFTER_LEAVE`] 帧的那几屏」，
//! 再叠一条 [`soft_limit`] 的封顶。从前那个上界是**整本书**。

use std::collections::HashMap;
use std::sync::Arc;

use folio_core::infra::metrics::Metric;
use gpui_kit::{
    App, AppContext as _, Context, Entity, EntityId, ImageFormat, RenderImage, Task, WeakEntity,
    Window,
};

use crate::ui::foundation::images::DecodeRequest;

/// 一张图离开元素树之后，还留着不放的那几帧。
///
/// 留这几帧不是为了省解码，是为了**不赌渲染帧的连续性**：换模式、邻章排完补进链、
/// 浮层收起那一瞬，都可能有一帧一个 `img` 都不产出。真在那一帧就把格子放掉，
/// 下一帧又传回来，读者看到的是图在闪。
///
/// 一次翻页动画约 280 ms（≈17 帧）—— 留 30 帧，翻过去再翻回来时那一页还在手里，
/// 不必为同一页重付一趟栅格化（手机上一页是好几秒）。再往前留的由
/// [`soft_limit`] 那一档按「离焦点页多远、其次最久没在场」挤出，所以这里放大不会让内存无界。
const HELD_AFTER_LEAVE: u64 = 30;

/// 手里最多攒多少字节的像素（含还没上传的）—— **软顶**，按设备分档。
///
/// 一台手机上 24 MB ≈ 六页 1024² 的纹理，或两页 2048×1400 的漫画页；桌面给 128 MB，
/// 因为桌面的显存与内存都比这个数大一个量级，卡在 24 MB 会让「翻回去看上一页」总是
/// 重画。平板取中间那一档（48 MB）：屏幕比手机大一截，一页的像素就是两三倍。
///
/// 定这个数不是为了卡住「正常一屏」—— 正常一屏到不了它 —— 是为了那一类一页摆二十张
/// 源图的大画册页：那种屏按可见带算就是几十张全在场，而一张 2048×1500 就是 12 MB。
///
/// 它**只能挤已经离场的那些**（见 [`ImageAtlas::end_frame`] 那句 `seen < frame`）：
/// 把正在屏幕上的图放掉，下一帧就得重传，再放掉…… 每一帧重解一张 12 MB 的图，
/// 那是比占着显存更糟的结果。
pub(in crate::ui::features::reader) fn soft_limit() -> usize {
    match device_class() {
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
fn hard_of(soft: usize) -> usize {
    soft * 2
}

/// 设备分档。判据只有屏幕宽一条：平板与手机的差别在「一页摆出来多大」，而那一格
/// 就是逻辑宽度；桌面与移动平台的差别在显存预算，按目标平台分。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeviceClass {
    Phone,
    Tablet,
    Desktop,
}

/// 宽到这个逻辑像素数以上算平板（iPad 竖屏 768、横屏 1024；Android 平板常见 800–1280）。
const TABLET_MIN_WIDTH: f32 = 1000.0;

/// 缩放系数在这之上算高 DPI 桌面。
fn device_class() -> DeviceClass {
    if cfg!(any(target_os = "android", target_os = "ios")) {
        return DeviceClass::Phone;
    }
    match *SCREEN_WIDTH.read().unwrap_or_else(|e| e.into_inner()) {
        Some(width) if width >= TABLET_MIN_WIDTH => DeviceClass::Tablet,
        _ => DeviceClass::Desktop,
    }
}

/// 当前窗口的逻辑宽度，由绘制侧每帧写一次（`view::body` 那一处 `set_screen_width`）。
///
/// 为什么用全局而不是塞进图集：内存档位是**设备**的属性，不是「这一本图集」的属性；
/// 而图集是按书建的，测试里一次能建好几个。宽度只是用来分「平板 / 桌面」那一档，
/// 写晚了最多影响一轮回收，不会画错东西。
static SCREEN_WIDTH: std::sync::RwLock<Option<f32>> = std::sync::RwLock::new(None);

/// 绘制侧每帧把当前逻辑宽度推过来一次（`view::body` 在 `end_frame` 之前）。
pub(in crate::ui::features::reader) fn set_screen_width(width: f32) {
    if let Ok(mut slot) = SCREEN_WIDTH.write() {
        *slot = Some(width);
    }
}

/// 配置文件里那一格（`reader.image_cache_bytes`）想直接定住软顶时用；`0` 与「没装配置
/// 全局」（纯视图测试）都是「没覆盖」，于是走 [`soft_limit`] 的设备分档。
///
/// 读的是 `ConfigHolder` 的**生效值**，不是用户层：环境变量那一档覆盖（`FOLIO_*`）与
/// 「这一本书单独设过」都已经在里面了，绕过去就会有两套口径。
fn configured_cache_bytes(cx: &App) -> Option<usize> {
    let bytes = cx
        .try_global::<crate::ui::services::config::ConfigHolder>()
        .map(|holder| holder.0.read(cx).config.reader.image_cache_bytes)
        .unwrap_or(0);
    (bytes > 0).then_some(bytes as usize)
}

/// 取字节那一截的结局（作业池那条线程 → 异步链）。
#[derive(Clone, Debug)]
enum Load {
    /// 这一页**没人要了**（取到活时取消旗已置位）或容器里根本没这张图。
    NoBytes,
    /// 池子跑完了，但作业报错（栅格化失败那类）。
    Missing,
    /// 字节连同**它的真实格式**一起交回来：正文里 JPEG / WebP / GIF 都有，
    /// 解码头不能替它们猜成 PNG（见 `touch` 里解码头那一支的 `Image::from_bytes`）。
    Bytes(ImageFormat, Vec<u8>),
}

/// 这一格离焦点页多远（越大越先被挤出）。键里带着页号，见 `foundation::images` 的
/// `render_job`（`<token>/render/<page>@<edge>`）；容器图没有页号，一律给 `None`。
///
/// 键是**字符串**，所以这里只能按格式拆。拆不出来（或这一格不是渲染页）给 `None`，
/// 排序时 `None` 排在「有距离」的前面 —— 容器图那一族本来就按「最久没在场」走，
/// 与从前一样。
fn distance_of(key: &str, focus: Option<usize>) -> usize {
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

/// 后台那一趟的三种结局。
///
/// 为什么不合成一个 `Result`：「容器里没有这张图」与「有字节但解不开」以后要分开记，
/// 前者是这本书本来就没这张图（与 KOReader 的 `NodeImageProxy::IsInvalid()` 同一种降级），
/// 后者是格式认错了或文件坏了 —— 只有后者值得喊一声。
enum Decoded {
    Pixels(Arc<RenderImage>),
    NoBytes,
    Broken(String),
}

/// 一格的状态。
enum Slot {
    /// 后台那一趟还在跑。键先占上，为的是同一张图在一帧里被问两次（双页对开、
    /// 跨页图）只起一趟。
    ///
    /// `cancel` 是**界面这一侧**握着的那面旗（不是池给 `TaskHandle` 的那面 —— 句柄关在
    /// 起活儿那条线程里，图集摸不到它）。换书、关阅读页、实体释放都把它置位：排队没开始
    /// 的那一趟在取活检查点直接交回，不再画一张早就不在屏上的页。
    Loading {
        seen: u64,
        _load: Task<()>,
        cancel: folio_core::Cancel,
    },
    /// 像素在手裡。`bytes` 是这一格真正占的量（解好之后按真像素数算，不再用估的）。
    ///
    /// `uploaded` = 这一格**真上传过**（画过至少一帧）。没画过的图只有内存里没有显存，
    /// 放掉它不该去碰 `drop_image`，也不该记进「放掉了多少显存」那笔账。
    Held {
        image: Arc<RenderImage>,
        bytes: usize,
        seen: u64,
        uploaded: bool,
    },
    /// 解过了，解不出来。**不再重试**（与从前那张 `OnceLock` 缓存 `None` 的行为一致），
    /// 且不占字节，所以不参与回收。
    Failed { seen: u64 },
}

/// 一本书当前攒下的像素，以及它们各自上一次在场是哪一帧。
#[derive(Default)]
pub struct ImageAtlas {
    slots: HashMap<String, Slot>,
    /// 攒下的字节总数（= 各 `Held` 那格的 `bytes` 之和）。
    bytes: usize,
    /// 帧号，每渲染一帧 +1（[`Self::end_frame`] 那一处加）。
    frame: u64,
    /// 这一帧读者在看哪一页（容器图没有页号，`None`）。挤出页缓存时排在最前的是
    /// 「离这一页最远」的那些，其次才是「最久没在场」。
    focus: Option<usize>,
    /// 上一帧真上屏的那些格（`end_frame` 每次把它换成这一帧的）。
    on_screen_prev: std::collections::HashSet<String>,
    /// 这一帧真上屏的那些格（[`Self::painted`] 记下，[`Self::end_frame`] 用完就清）。
    ///
    /// 为什么要有它：回收那一刀要的是「这一帧谁在场」，而从前只有 `Slot::Held.seen`
    /// 那一格 —— 它记的是「最后一次被问」，两者在画一帧、下一帧还没画的窗口里是两回事。
    /// 取字节那一截搬进作业池之后这个窗口变成了可观察的（真线程两边跑），旧判据
    /// 会把「最后一帧在场」的格子当成离场，于是先画的那一帧算白画。
    on_screen: std::collections::HashSet<String>,
    /// 配置文件里那一格覆盖的软顶（`reader.image_cache_bytes`）；`None` = 按设备分档。
    ///
    /// 建实体时读一次（[`configured_cache_bytes`]）：这一格不是设置屏上的旋钮，
    /// 为它在每个渲染帧里问一次配置全局不值。
    cache_bytes: Option<usize>,
}

impl ImageAtlas {
    /// 建这个实体，并装上**释放时把格子全部放掉**那一条。
    ///
    /// `observe_release` 是 gpui 自己那张 `RetainAllImageCache` 用的同一招
    /// （`elements/image_cache.rs:237`）：实体被别人扔了（关书、退出阅读页）的时候
    /// 已经没有 `&mut App` 可用，而那时若不逐张 `drop_image`，这一趟读过的图就
    /// **全都永久留在图集里** —— 那正是这次要修的漏，不能修一半。
    pub fn entity(cx: &mut App) -> Entity<Self> {
        let cache_bytes = configured_cache_bytes(cx);
        let atlas = cx.new(|_| ImageAtlas {
            cache_bytes,
            ..ImageAtlas::default()
        });
        cx.observe_release(&atlas, |atlas, cx| {
            for (_, slot) in std::mem::take(&mut atlas.slots) {
                match slot {
                    Slot::Held {
                        image,
                        uploaded: true,
                        ..
                    } => cx.drop_image(image, None),
                    // 手上还没落地的活儿一并请求取消：关阅读页时队里那几页预取
                    // 一分钟之内就会变成没用的字节，而它们占着 worker。
                    Slot::Loading { cancel, .. } => cancel.trigger(),
                    Slot::Held { .. } | Slot::Failed { .. } => {}
                }
            }
            atlas.bytes = 0;
        })
        .detach();
        atlas
    }

    /// 这一帧这张图画得出来吗？
    ///
    /// 解好了给像素（并把「这一帧在场」记上），还在解或解不出来给 `None` —— 调用方
    /// 把那块空着，正文照排。`None` 的那一趟解完之后会 `notify(view)` 逼出一帧，
    /// 图自己就补上来，不需要调用方去轮询。
    pub fn painted(
        &mut self,
        request: &DecodeRequest,
        view: EntityId,
        cx: &mut Context<Self>,
    ) -> Option<Arc<RenderImage>> {
        self.on_screen.insert(request.key.clone());
        self.touch(request, view, true, cx)
    }

    /// 这一格解过且解不出来（图集不再重试）。调用方据此退回别的档位 —— 低清预览
    /// 解不出来时，那一页不能永远空着。
    /// 这一格的高清**已经在手**（不负责去起它）。
    ///
    /// 滑动中那一帧用它决定「换不换」：在手就换上去，没在手就让预览再顶一会儿，
    /// 别在滑动里起一趟几十到几百 ms 的栅格化。
    pub fn ready(&self, key: &str) -> bool {
        matches!(self.slots.get(key), Some(Slot::Held { .. }))
    }

    pub fn failed(&self, key: &str) -> bool {
        matches!(self.slots.get(key), Some(Slot::Failed { .. }))
    }

    /// 预解一张「马上要看、但这一帧还没上屏」的图：起那趟后台活儿，把格子留在手里。
    ///
    /// 为什么不直接复用 [`Self::painted`]：那一句会把 `uploaded` 置真，而放掉一格时
    /// `uploaded` 决定要不要 `drop_image` 与记一笔「放掉了多少显存」。一张从没画过的图
    /// 被按「显存里有一格」放掉，那笔账就虚报，而 gpui 那边根本没有这么一格。
    ///
    /// 图片查看器用它预取左右那两张：横扫过去时解码已经完了，读者看到的是下一张，
    /// 不是黑一下。
    pub fn prepare(&mut self, request: &DecodeRequest, view: EntityId, cx: &mut Context<Self>) {
        self.touch(request, view, false, cx);
    }

    /// 「这一帧这张图画得出来吗」与「先在手里留一格」共用的那半：查格子、必要时起活。
    ///
    /// `on_screen` = 这一帧真有人画它（`painted`）。它只决定 `uploaded` 那一格，
    /// 两档都会把 `seen` 记成本帧，所以预取的那几张在查看器开着期间不会被回收，
    /// 而关掉那一层之后两帧就被收走 —— 与真的在场过的是同一套规矩。
    fn touch(
        &mut self,
        request: &DecodeRequest,
        view: EntityId,
        on_screen: bool,
        cx: &mut Context<Self>,
    ) -> Option<Arc<RenderImage>> {
        let frame = self.frame;
        if let Some(slot) = self.slots.get_mut(&request.key) {
            return match slot {
                Slot::Held {
                    image,
                    seen,
                    uploaded,
                    ..
                } => {
                    *seen = frame;
                    // 只往上置，不往下清：这一格可能先被预取、后被真画。
                    *uploaded |= on_screen;
                    Some(image.clone())
                }
                Slot::Loading { seen, .. } => {
                    *seen = frame;
                    None
                }
                Slot::Failed { seen } => {
                    *seen = frame;
                    None
                }
            };
        }

        // 第一趟：这一帧先空着，后台那一趟现在起。
        //
        // 两截：**拿字节**（栅格化 PDF 一页 / 读容器）走 core 的作业池，按优先级排队；
        // **解码**仍在起活的那条异步链上（gpui 的 `to_image_data` 要一个 `SvgRenderer`，
        // 那是 UI 侧句柄）。字节那一截是几十到几百 ms，解码那一截是几十 ms —— 先说清楚
        // 哪一截更贵，才不会为了搬线程把解码写成第二份实现（见计划 §零）。
        let key = request.key.clone();
        let loader = request.loader();
        let priority = request.priority();
        let renderer = cx.svg_renderer();
        let cancel = folio_core::Cancel::new();
        // 池子里那一截跑在**真线程**上（它不进 gpui 的测试执行器，也就不会在
        // `run_until_parked` 之前跑），所以要轮询它、并且把结果带回这条异步链。
        let slot: Arc<(std::sync::Mutex<Option<Load>>, std::sync::Condvar)> =
            Arc::new((std::sync::Mutex::new(None), std::sync::Condvar::new()));
        {
            let slot = Arc::clone(&slot);
            let job_cancel = cancel.clone();
            std::thread::Builder::new()
                .name("folio-page-load".into())
                .spawn(move || {
                    // 交回 `Option`：`None` 就是「这一趟没人要了 / 容器里没这张图」，
                    // 与「作业整体失败」分得开（那种落在 `Err`，走 `Load::Missing`）。
                    let job = move |_: &folio_core::StageCtx| -> std::result::Result<
                        Option<(ImageFormat, Vec<u8>)>,
                        folio_core::TaskError,
                    > {
                        // 协作式取消的检查点：取到活的那一刻。读者快滑过去的那一页此时可能
                        // 还排在队里，而它的栅格化是几十到几百 ms 的真活儿 —— 画完了也没人看。
                        // 一趟只画一页，页内没有第二个检查点（`hayro` 画到一半停不下来），
                        // 所以**已经在跑的那一趟只能让它画完**，扔掉的只是它交回来的字节。
                        if job_cancel.triggered() {
                            return Ok(None);
                        }
                        Ok(loader().map(|image| (image.format, image.bytes().to_vec())))
                    };
                    let outcome = match crate::ui::services::resident::pool_maybe() {
                        Some(pool) => {
                            let handle = pool.submit_with(priority, job);
                            let mut finished = None;
                            for event in &handle.events {
                                if let folio_core::TaskEvent::Finished(result) = event {
                                    finished = Some(result.map_err(|err| err.to_string()));
                                }
                            }
                            finished
                        }
                        None => Some(
                            job(&folio_core::StageCtx::detached_for_tests())
                                .map_err(|err| err.to_string()),
                        ),
                    };
                    let load = match outcome {
                        Some(Ok(Some((format, bytes)))) => Load::Bytes(format, bytes),
                        Some(Ok(None)) => Load::NoBytes,
                        Some(Err(_)) | None => Load::Missing,
                    };
                    let mut guard = slot.0.lock().unwrap_or_else(|e| e.into_inner());
                    *guard = Some(load);
                    slot.1.notify_all();
                })
                .expect("起取字节那条线程");
        }
        let raw = cx.background_spawn(async move {
            loop {
                {
                    let guard = slot.0.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(outcome) = guard.clone() {
                        break outcome;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        });
        // 那条线程不留句柄：它跑完由内核回收，而它最后一件事就是写下这一格并摇铃。
        let decode_cancel = cancel.clone();
        let decoded = cx.background_spawn(async move {
            // 解码头留在 UI 侧那条异步链上（gpui 的 `to_image_data` 要 `SvgRenderer`），
            // 与从前逐字相同；前面那一截取字节走的是作业池。耗时单独记一笔
            // （[`super::perf::note_image_decode`]）—— 那一截要不要也搬走，看的是这条数。
            match raw.await {
                Load::Missing | Load::NoBytes => Decoded::NoBytes,
                Load::Bytes(format, bytes) => {
                    // 字节到手到解码之间再查一次旗：栅格化那几秒里读者可能已经翻走了，
                    // 而解码是 UI 侧那条链上的几十 ms，不必为一个没人看的格子付。
                    if decode_cancel.triggered() {
                        return Decoded::NoBytes;
                    }
                    let at = std::time::Instant::now();
                    let image = gpui_kit::Image::from_bytes(format, bytes);
                    match image.to_image_data(renderer) {
                        Ok(pixels) => {
                            // 记的是**解出来之后**的真像素数（不是请求里那份估的），与图集
                            // 记账同一把尺 —— 两把尺差多少，正是那笔「虚报名额」的量。
                            super::perf::note_image_decode(
                                at.elapsed().as_micros() as u64,
                                tile_bytes(&pixels),
                            );
                            Decoded::Pixels(pixels)
                        }
                        Err(err) => Decoded::Broken(err.to_string()),
                    }
                }
            }
        });
        Metric::AtlasDecodes.count();
        let for_task = key.clone();
        let load = cx.spawn(async move |this: WeakEntity<ImageAtlas>, cx| {
            let decoded = decoded.await;
            let _ = this.update(cx, |atlas, cx| atlas.finish(&for_task, decoded, view, cx));
        });
        self.slots.insert(
            key,
            Slot::Loading {
                seen: frame,
                _load: load,
                cancel,
            },
        );
        None
    }

    /// 一趟后台活儿的落点。
    ///
    /// 那一趟跑完时这张图可能早就滚出去了：那时 `slots` 里已经没有这一格（或不再是
    /// `Loading`），交回来的像素直接扔 —— **不**替它占一个新的名额，否则一次快滑会把
    /// 沿途几十张图的像素全攒在手里，那就回到这次要修的那个形状上了。
    fn finish(&mut self, key: &str, decoded: Decoded, view: EntityId, cx: &mut Context<Self>) {
        let Some(Slot::Loading { seen, .. }) = self.slots.get(key) else {
            return;
        };
        let seen = *seen;
        match decoded {
            Decoded::Pixels(image) => {
                let bytes = tile_bytes(&image);
                self.bytes += bytes;
                self.slots.insert(
                    key.to_string(),
                    Slot::Held {
                        image,
                        bytes,
                        seen,
                        uploaded: false,
                    },
                );
            }
            Decoded::NoBytes => {
                tracing::debug!("图集: {key} 容器里没有这张图，那块空着");
                self.slots.insert(key.to_string(), Slot::Failed { seen });
            }
            Decoded::Broken(err) => {
                // 这一条要出声：解不开的图在屏幕上与「这本书没这张图」长得一模一样，
                // 而它的根因通常在格式（manifest 写错 / 字节头写错）上。
                tracing::warn!("图集: {key} 解不开 —— {err}");
                self.slots.insert(key.to_string(), Slot::Failed { seen });
            }
        }
        // 解完了才喊重画：解出来那一帧图上屏，读者看到的是「图晚一帧出现」，
        // 不是「图没画出来，下次滚动才有」。
        //
        // 喊的**不是自己**：图集不是视图、上不了屏，要重画的是那一帧调 `painted()` 的
        // 视图（`Context::notify()` 只会喊自己这一格，所以这里先把 `cx` 当 `App` 用）。
        let app: &mut App = cx;
        app.notify(view);
    }

    /// 帧末回收：这一帧没在场、且离场满 [`HELD_AFTER_LEAVE`] 帧的那些格子，把像素放掉；
    /// 放完还超 [`soft_limit`]，就从「离焦点页最远、其次最久没在场」的那些接着放。
    ///
    /// 只在**渲染那一帧的元素树全部搭完之后**调一次。放在渲染中途放的格子，随后又会被
    /// 同一帧里的 `img` 传回去 —— 白花一趟上传。
    ///
    /// 两条不动的规矩：
    /// - **只放 `Arc::strong_count == 1` 的**。还有别人握着这份像素（放大查看那一层
    ///   就是）说明它仍在被画：那时放掉，它下一帧自己传回来，而我们手里已经没有句柄了
    ///   —— 传回来的那一格从此没人能放，正是要修的漏。等那人松手再放。
    /// - **不碰这一帧在场的**（`seen == frame`）。封顶那条因此只能挤离场的那些：
    ///   一屏真放不下时宁可占着，也不每帧重解一张 12 MB 的图。
    pub fn end_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 这一帧在场的那些：`seen` 既是「刚被问」的标记，也是挤出顺序的第二键。
        let on_screen = std::mem::take(&mut self.on_screen);
        let previous = std::mem::replace(&mut self.on_screen_prev, on_screen.clone());
        let frame = self.frame;
        let mut freed = 0usize;
        let mut stale: Vec<String> = self
            .slots
            .iter()
            .filter(|(key, slot)| match slot {
                // 回收的判据是「**最近两帧都没上场**」，不是「这一帧没上场」：
                // 最后一帧画过它的下一条命令还没落下时 `seen` 不是最新（取字节那一截
                // 现在跑在真线程上，这个窗口真实存在），只按这一帧判会把读者眼前那一页
                // 当成离场的。宽一帧的代价只是多留一帧，而错放的代价是每帧重解一次。
                Slot::Held { seen, .. } => {
                    !on_screen.contains(key.as_str())
                        && !previous.contains(key.as_str())
                        && frame.saturating_sub(*seen) > HELD_AFTER_LEAVE
                }
                // 还没上传过的格子（一趟后台活儿刚落地就被挤出来）也要放：它占的是内存。
                Slot::Loading { .. } | Slot::Failed { .. } => false,
            })
            .map(|(key, _)| key.clone())
            .collect();
        // 封顶那一档：只挤已经离场的，按「离焦点页多远」排，同距再按「最久没在场」。
        //
        // 为什么先看距离：快速滑动时一屏能压住两三页，而「最久没在场」会把**刚滑过去
        // 的那一页**排在前面 —— 往回一滑就得重画。距离把「眼前这几页」护住，
        // 而它同一时刻仍然只动离场的格子（在屏上的永远不碰，见上面那两条不动的规矩）。
        //
        // 上限是**软顶 → 硬顶**两段：平时按软顶；软顶不够装下**焦点页自己**时就涨到
        // 硬顶，涨了仍不够就不再涨（那一页本来就超出预算，不拿无界内存兜）。
        let focus = self.focus;
        let mut oldest: Vec<(usize, u64, String)> = self
            .slots
            .iter()
            .filter_map(|(key, slot)| match slot {
                Slot::Held { seen, .. } if !on_screen.contains(key.as_str()) =>
                {
                    Some((distance_of(key, focus), *seen, key.clone()))
                }
                _ => None,
            })
            .collect();
        oldest.sort_unstable();
        let focus_bytes: usize = self
            .slots
            .iter()
            .filter_map(|(key, slot)| match slot {
                Slot::Held { bytes, .. } if on_screen.contains(key.as_str()) => {
                    Some((distance_of(key, focus), *bytes))
                }
                _ => None,
            })
            .filter(|(distance, _)| *distance == 0)
            .map(|(_, bytes)| bytes)
            .sum();
        let soft = self.soft_cap();
        let limit = if focus_bytes > soft { hard_of(soft) } else { soft };
        for (_, _, key) in oldest {
            if self.bytes.saturating_sub(freed) <= limit {
                break;
            }
            if !stale.contains(&key) {
                stale.push(key);
            }
        }
        for key in stale {
            // 强引用那一格：还有别人握着这份像素（放大查看那一层、测试里那一句借用）
            // 说明它仍在被画。这时**先不退场标记**（把这一格放回去，等那人松手）。
            if let Some(Slot::Held { image, .. }) = self.slots.get(&key)
                && Arc::strong_count(image) > 1
            {
                continue;
            }
            if let Some(Slot::Held {
                image,
                bytes,
                uploaded,
                ..
            }) = self.slots.remove(&key)
            {
                self.bytes -= bytes;
                if uploaded {
                    // 放掉 GPU 那一格，再把像素一起扔。回屏时 `img` 自己会重传
                    // （`a_reclaimed_image_uploads_itself_again_on_the_next_frame` 钉的就是这句）。
                    cx.drop_image(image, Some(window));
                    Metric::AtlasFreed.add(bytes as u64);
                    freed += bytes;
                }
            }
        }
        if freed > 0 {
            tracing::debug!(
                "图集: 放掉 {} 字节，还剩 {} 字节 / {} 张",
                freed,
                self.bytes,
                self.slots.len()
            );
        }
        self.frame += 1;
    }

    /// 这一帧读者在看哪一页。挤出缓存时「离它多远」是第一个排序键。
    ///
    /// 由 `view::body` 在 `end_frame` 之前推一次；容器图（EPUB / CBZ）没有页号，
    /// 那一族给 `None`，于是全部退回「最久没在场」的旧顺序 —— 那里本来也只有一个
    /// 可见带，没有「翻到第几页」这回事。
    pub(in crate::ui::features::reader) fn set_focus(&mut self, page: Option<usize>) {
        self.focus = page;
    }

    /// 这一本的软顶：配置文件覆盖优先，否则走 [`soft_limit`] 的设备分档。
    ///
    /// 覆盖只换**那个数**，不换规矩 —— 离场的那些才挤、在屏上的永不碰、焦点页自己
    /// 装不下时仍可涨到两倍（[`hard_of`]），全部按这一格重算。
    fn soft_cap(&self) -> usize {
        self.cache_bytes.unwrap_or_else(soft_limit)
    }

    /// 换一本书 / 退出阅读页：手里全部像素都放掉。
    ///
    /// 会话换书时 `source` 是**原地被替换**的（`session.rs` 那句 `self.source = ...`），
    /// 实体不会因此释放，所以这一趟必须显式做。键里带着每本书自己的 token，
    /// 不显式清也不会画出别的书的那张图，但旧书那一百多兆会一直攒到关会话为止。
    ///
    /// `drop_image(.., None)` = 在**所有**窗口上放掉这一格：换书时手上没有那个
    /// `&mut Window`（`open_book` 是命令入口不是渲染帧），而桌面多开一个窗口时
    /// 只放当前那个会留下另一份。
    pub fn clear(&mut self, cx: &mut App) {
        for (_, slot) in std::mem::take(&mut self.slots) {
            match slot {
                Slot::Held {
                    image,
                    bytes,
                    uploaded: true,
                    ..
                } => {
                    cx.drop_image(image, None);
                    Metric::AtlasFreed.add(bytes as u64);
                }
                // 换书那一趟把没跑完的活儿也退了：新书要的就是那几条 worker，
                // 让旧书队里的预取继续画完只会把它们挡在门外（见 [`Slot::Loading`]）。
                Slot::Loading { cancel, .. } => cancel.trigger(),
                Slot::Held { .. } | Slot::Failed { .. } => {}
            }
        }
        self.bytes = 0;
        self.frame = 0;
    }

    /// 手里攒了多少字节的像素。给测试问的（回收有没有在发生，看这一格的走势）。
    ///
    /// 生产里这一格不用问：每次真放掉东西，[`Self::end_frame`] 自己会把剩下的量打进
    /// `debug` 那条日志（真机上按 `folio_app::perf=debug` 那一档开得出声）。
    #[cfg(test)]
    pub fn resident_bytes(&self) -> usize {
        self.bytes
    }

    /// 这一格是不是正在重解（像素已放掉、活儿在排队）。
    #[cfg(test)]
    pub fn reloading(&self, key: &str) -> bool {
        matches!(self.slots.get(key), Some(Slot::Loading { .. }))
    }

    /// 还有没有没落地的格子（`Loading` 那些）。测试的 `settle` 靠它知道那条真线程
    /// 有没有回来 —— gpui 的 `run_until_parked` 只推进它自己的执行器。
    #[cfg(test)]
    pub fn outstanding(&self) -> bool {
        self.slots
            .values()
            .any(|slot| matches!(slot, Slot::Loading { .. }))
    }

    /// 这一张现在在手裡吗（解好了才给）。测试拿它取句柄去问图集。
    #[cfg(test)]
    pub fn peek(&self, key: &str) -> Option<Arc<RenderImage>> {
        match self.slots.get(key) {
            Some(Slot::Held { image, .. }) => Some(image.clone()),
            _ => None,
        }
    }
}

/// 一格的实际字节数：gpui 放进去的是**原始像素 × 4**，动图逐帧都算。
fn tile_bytes(image: &RenderImage) -> usize {
    (0..image.frame_count())
        .map(|frame| {
            let size = image.size(frame);
            size.width.0.max(0) as usize * size.height.0.max(0) as usize * 4
        })
        .sum()
}

#[cfg(test)]
mod tests {
    //! 回收这一层的七条门。每条都要能在「把那一步删掉」时翻红，见每条自己的说明。
    //!
    //! 全在 headless 窗口里跑：`has_image_atlas_entry` 是 gpui 的 test-only 接口
    //! （`test-support` 已在本 crate 的 dev 依赖里打开），不需要真 GPU，也不看渲染器脸色。
    //!
    //! **两类变异在这里表现为「挂住」而不是干净翻红**，2026-10-15 实测：把「同一张图
    //! 在场期间复用那一格」拿掉，或把「解不出来也记一笔、不再重试」拿掉 —— 两种都让
    //! 测试里 [`settle`] 那句 `run_until_parked` 永不返回。原因在这条链自己的形状：
    //! 解完必 `notify` 那个视图，`notify` 必重画，而重画又起一趟新的解 —— 没有静止点。
    //! 门照样是有效的（套件永远绿不了），但别把那个超时当成无关的 flake。
    //! 其余五条（回收关掉 / 缓冲帧数归零 / 去掉键的 token / 封顶不挤人 / 字节账记错）
    //! 都是当场翻红，红的那条正是该红的那条。

    use std::collections::HashMap;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use folio_core::content::format::ImageRes;
    use folio_core::infra::metrics::{Metric, take_local};
    use gpui_kit::{
        AnyWindowHandle, App, AppContext as _, Context, Entity, IntoElement, ParentElement, Render,
        RenderImage, Styled, TestAppContext, Window, div, img, px, size,
    };

    use super::ImageAtlas;
    use crate::ui::foundation::images::{
        BookImages, ByteSource, ChapterImages, DecodeRequest, TINY_PNG,
    };

    /// 走满 [`super::HELD_AFTER_LEAVE`] 那一档缓冲要画几帧。
    ///
    /// 判据是「离场超过缓冲帧数」**且**「最近两帧都没上场」，而换屏那一帧它还在上一帧的
    /// 在场集合里，所以从换屏起算要 `HELD_AFTER_LEAVE + 2` 帧。
    ///
    /// **这条跟着常数走，不写死帧数**：2026-10-28 那一轮把 `HELD_AFTER_LEAVE` 从 2 抬到
    /// 30（留够一趟翻页动画的 17 帧），写死 4 帧的三条门当场变成空跑的绿 —— 回收没生效，
    /// 而断言只看「画了 4 帧还没放」，那在缓冲期是 30 帧时本来就是对的。
    const fn frames_to_reclaim() -> usize {
        super::HELD_AFTER_LEAVE as usize + 2
    }

    /// 一屏摆几张图的视图。`showing` 就是「这一帧在场的那几张」。
    ///
    /// 必须是真的视图而不是裸元素：图集解完一张要 `notify` 一个实体才会重画，
    /// 而生产里那个实体就是 `ReaderSession`。这里照抄它的形状。
    struct Screen {
        atlas: Entity<ImageAtlas>,
        /// 测试自己的一本「书」：容器路径 → 那张图的活儿。
        table: Rc<HashMap<String, DecodeRequest>>,
        showing: Vec<String>,
    }

    impl Render for Screen {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let view = cx.entity().entity_id();
            let mut root = div().size_full();
            for href in self.showing.clone() {
                let Some(request) = self.table.get(&href).cloned() else {
                    continue;
                };
                let Some(image) = self
                    .atlas
                    .update(cx, |atlas, cx| atlas.painted(&request, view, cx))
                else {
                    continue;
                };
                root = root.child(img(image).size_full());
            }
            // 帧末回收：与生产里「元素树搭完之后调一次」同一个位置。
            self.atlas
                .update(cx, |atlas, cx| atlas.end_frame(window, cx));
            root
        }
    }

    /// 一本 `count` 张同尺寸插图的测试书，交出 `(图集, 表, 容器问数)`。
    ///
    /// `reads` 数的是**容器被问了几次** —— 那是「有没有重解」的直接证据。它必须是
    /// `Arc<AtomicUsize>`：那条字节来源的契约是 `Send + Sync`（排版与后台解码都拿着它），
    /// `Rc<Cell<_>>` 进不去。
    fn book(
        cx: &mut App,
        count: usize,
        width: u32,
        height: u32,
    ) -> (
        Entity<ImageAtlas>,
        Rc<HashMap<String, DecodeRequest>>,
        Arc<AtomicUsize>,
    ) {
        let reads = Arc::new(AtomicUsize::new(0));
        let images: Vec<ImageRes> = (0..count)
            .map(|i| ImageRes {
                href: format!("pic-{i}.png"),
                source: format!("pic-{i}.png"),
                media_type: "image/png".into(),
                width,
                height,
            })
            .collect();
        let bytes = {
            let reads = Arc::clone(&reads);
            std::sync::Arc::new(move |href: &str| {
                reads.fetch_add(1, Ordering::Relaxed);
                href.starts_with("pic-").then(|| png_of(width, height))
            })
        };
        let chapter =
            ChapterImages::new(Arc::new(BookImages::new(&images, bytes)), "Text/chap.xhtml");
        let mut table = HashMap::new();
        for i in 0..count {
            let href = format!("pic-{i}.png");
            table.insert(href.clone(), chapter.decode_request(&href).unwrap());
        }
        (ImageAtlas::entity(cx), Rc::new(table), reads)
    }

    /// 一张**真解得开**的 PNG。
    ///
    /// 2×2 那档直接用仓库里现成的 `TINY_PNG`；更大的一张用 `image` crate 现编
    /// （dev 依赖，见 `Cargo.toml` 那条注释）。理由与那条常量一样：只有文件头的字节要
    /// 等解码那一趟才出错，而「解不开」与「这本书没这张图」在屏幕上长得一模一样。
    fn png_of(width: u32, height: u32) -> Vec<u8> {
        if width == 2 && height == 2 {
            return TINY_PNG.to_vec();
        }
        let buffer = image::RgbaImage::from_pixel(width, height, image::Rgba([10, 20, 30, 255]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(buffer)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .expect("测试里的 PNG 应当编得出来");
        out
    }

    /// 画**恰好一帧**（标脏 + 走一遍绘制），不碰执行器。
    ///
    /// 帧号是回收的判据，所以一次 `paint` 必须正好对应图集前进一格。要等后台那一趟
    /// 跑完另有 [`settle`]，别把两件事混在一步里 —— 混了之后「离场几帧」这个数就
    /// 不是测试写的那个数了（第一次试的时候就是这么红的）。
    fn paint(handle: AnyWindowHandle, cx: &mut TestAppContext) {
        cx.update_window(handle, |_, window, _| window.refresh())
            .expect("窗口应当可标脏");
        cx.update_window(handle, |_, _, _| {})
            .expect("窗口应当可画");
    }

    /// 让后台那一趟跑完，并把解完之后被 `notify` 逼出来的那一帧画掉。
    ///
    /// 取字节那一截现在跑在**真线程**上（core 的作业池），而 `run_until_parked` 只
    /// 推进 gpui 自己的执行器 —— 它不知道那条线程还没回来。所以这里先等「图集的
    /// 后台活儿一个都不剩」再画那一帧；超时就直接画（失败的那条断言会告诉我们
    /// 是哪一格没落地，而不是整个套件挂在超时上）。
    ///
    /// **不在 `paint` 里循环等**：每多画一帧就多记一次「在场」，离场帧数那个计数
    /// 就不准了（第一版就是这么红的）。所以先把活儿等完，再画**一帧**。
    fn settle(handle: AnyWindowHandle, cx: &mut TestAppContext) {
        wait(handle, cx);
        paint(handle, cx);
    }

    /// 这个窗口的图集里还有没有没落地的格子（`Loading` 那些）。
    fn outstanding(handle: AnyWindowHandle, cx: &mut TestAppContext) -> bool {
        let Some(screen) = handle.downcast::<Screen>() else {
            return false;
        };
        screen
            .update(cx, |screen, _, cx| {
                screen
                    .atlas
                    .read_with(cx, |atlas, _| atlas.outstanding())
            })
            .unwrap_or(false)
    }

    /// 把后台那一趟等完（推进 gpui 的执行器 + 轮询图集），**不画帧**。
    ///
    /// 与 [`settle`] 分开的理由见 `paint` 的注释：画一帧就是回收判据前进一格，
    /// 「画几帧」必须由测试自己数。
    fn wait(handle: AnyWindowHandle, cx: &mut TestAppContext) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            cx.run_until_parked();
            if !outstanding(handle, cx) || std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    /// 从测试的 `TestAppContext` 问一句实体内部（`Entity::update` 要的是 `App`）。
    fn asked<R>(
        atlas: &Entity<ImageAtlas>,
        cx: &mut TestAppContext,
        f: impl FnOnce(&ImageAtlas) -> R,
    ) -> R {
        cx.update(|cx| atlas.update(cx, |atlas, _| f(atlas)))
    }

    /// 图集手里现在攒了多少字节的像素。
    fn resident(atlas: &Entity<ImageAtlas>, cx: &mut TestAppContext) -> usize {
        cx.update(|cx| atlas.update(cx, |atlas, _| atlas.resident_bytes()))
    }

    /// 这张图解好了的那一份像素（还没解好给 `None`）。
    fn held(
        atlas: &Entity<ImageAtlas>,
        key: &str,
        cx: &mut TestAppContext,
    ) -> Option<Arc<RenderImage>> {
        cx.update(|cx| atlas.update(cx, |atlas, _| atlas.peek(key)))
    }

    /// 这一格**又进场了**（像素已放掉、活儿正在重跑）。
    ///
    /// 回收之后回屏时它是这个状态：像素没了，`Loading` 那一格还在排队。从前那条门
    /// 在这时查到 `None` 就以为「回到手里了」—— 像素已经放掉、「回来」的其实是
    /// 重解那一趟。取字节那一截搬进作业池之后它变成可观察的了（以前像素与加载是
    /// 同一个异步链上的两段，`run_until_parked` 一把推到底，看不到中间态）。
    fn reloading(atlas: &Entity<ImageAtlas>, key: &str, cx: &mut TestAppContext) -> bool {
        cx.update(|cx| atlas.update(cx, |atlas, _| atlas.reloading(key)))
    }

    /// **回收本身**：离场满缓冲帧数之后，图集那一格必须真的没了。
    ///
    /// 关掉 `end_frame` 里那句 `drop_image`，这一条当场红（格子还在，第二句断言失败）。
    /// 先探一次「在场」再断「不在场」，两句互相排除空跑 —— 只有后一句的话，
    /// 「`has_image_atlas_entry` 根本量不到东西」也会伪装成通过。
    #[gpui_kit::test]
    fn an_image_that_left_the_band_frees_its_atlas_tile(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        take_local(Metric::AtlasFreed);
        let (atlas, table, reads) = cx.update(|cx| book(cx, 2, 2, 2));
        let key = table["pic-0.png"].key.clone();
        let window = cx.open_window(size(px(64.0), px(64.0)), {
            let atlas = atlas.clone();
            move |_, _| Screen {
                atlas,
                table,
                showing: vec!["pic-0.png".into()],
            }
        });
        let handle: AnyWindowHandle = window.into();
        paint(handle, cx);
        wait(handle, cx);
        // **这里不能 `held(...)` 把句柄拿住**：图集只放 `Arc::strong_count == 1` 的格子，
        // 测试自己再拿一份就等于告诉它「还有人在画」，回收永远等不到。所以先用一句
        // 临时借用问「在不在」，就此丢掉。
        let present = window
            .update(cx, |_, window, cx| {
                let Some(pixels) = atlas.read_with(cx, |atlas, _| atlas.peek(&key)) else {
                    return false;
                };
                window.has_image_atlas_entry(&pixels)
            })
            .expect("窗口应当可读");
        assert!(
            present,
            "前提：画过一帧之后这一格应当已上传。探不到就是那个 test-only \
             接口量不到东西，下面那句「放掉了」只是空跑的绿"
        );

        // 换屏：这张不再在场了。走满整档缓冲帧数才该放（帧数跟着 `HELD_AFTER_LEAVE` 算，
        // 见 [`frames_to_reclaim`] 那段：写死帧数的门在缓冲期改动后会静默空跑）。
        window
            .update(cx, |screen, _, _| screen.showing = vec!["pic-1.png".into()])
            .expect("窗口应当可改");
        for _ in 0..frames_to_reclaim() {
            paint(handle, cx);
            wait(handle, cx);
        }
        assert!(
            held(&atlas, &key, cx).is_none(),
            "离场满缓冲帧数却没放掉：回收这一步没生效，一百多兆仍然永久压在显存里"
        );
        assert_eq!(
            resident(&atlas, cx),
            2 * 2 * 4,
            "像素放掉了却没把账也放掉，封顶那一档从此按错的数挤人。剩下的那一格是\
             此刻**仍在场**的 pic-1（缓冲期 30 帧也不会碰在屏上的格子），pic-0 那一份必须已经归零"
        );
        assert_eq!(
            take_local(Metric::AtlasFreed),
            2 * 2 * 4,
            "放掉一格却没记这笔账：真机上就看不出回收有没有发生"
        );
        assert_eq!(
            reads.load(Ordering::Relaxed),
            2,
            "两张各取一次（pic-0 进场一次、pic-1 进场一次）。多于 2 就是被回收的 pic-0 \
             又重解了一趟，而它这一趟没有再回屏"
        );
    }

    /// 回收之后回屏必须自己补回来，否则「回收」就等于「这张图永久画丢」。
    #[gpui_kit::test]
    fn a_reclaimed_image_uploads_itself_again_on_the_next_frame(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (atlas, table, reads) = cx.update(|cx| book(cx, 2, 2, 2));
        let key = table["pic-0.png"].key.clone();
        let window = cx.open_window(size(px(64.0), px(64.0)), {
            let atlas = atlas.clone();
            move |_, _| Screen {
                atlas,
                table,
                showing: vec!["pic-0.png".into()],
            }
        });
        let handle: AnyWindowHandle = window.into();
        paint(handle, cx);
        settle(handle, cx);
        let first = held(&atlas, &key, cx).expect("第一屏应当解好了");
        assert!(
            window
                .update(cx, |_, window, _| window.has_image_atlas_entry(&first))
                .expect("窗口应当可读"),
            "前提：在场且已上传"
        );
        drop(first);

        window
            .update(cx, |screen, _, _| screen.showing = vec!["pic-1.png".into()])
            .expect("窗口应当可改");
        for _ in 0..frames_to_reclaim() {
            paint(handle, cx);
        }
        assert!(held(&atlas, &key, cx).is_none(), "前提：这一张已经放掉了");

        // 滚回来：重解一趟，格子自己回来。
        window
            .update(cx, |screen, _, _| screen.showing = vec!["pic-0.png".into()])
            .expect("窗口应当可改");
        paint(handle, cx);
        assert!(
            reloading(&atlas, &key, cx) || held(&atlas, &key, cx).is_some(),
            "回屏那一帧既没有像素也没有在排队：这一格回不来了"
        );
        settle(handle, cx);
        let again = held(&atlas, &key, cx).expect("回屏应当重新解出一份");
        assert!(
            window
                .update(cx, |_, window, _| window.has_image_atlas_entry(&again))
                .expect("窗口应当可读"),
            "回屏之后图集里没有这一格 —— 回收过的图不会再画出来，那是查不出根因的错"
        );
        assert_eq!(
            reads.load(Ordering::Relaxed),
            3,
            "整趟应当取三次字节：pic-0 进场一次、pic-1 进场一次、pic-0 被回收之后回屏再一次。\
             只取到 2 次说明那张图根本没被回收（回屏时旧格子还在），多于 3 次说明在场期间在重解"
        );
    }

    /// 在场期间**不重解**：同一张图连着画五帧，容器只该被问一次。
    ///
    /// 这一条钉的是回收的另一半 —— 别把回收做成每帧重解。把 `painted()` 里那一格的
    /// 复用删掉（每帧都起一趟），`reads` 立刻涨到 5。
    #[gpui_kit::test]
    fn an_image_on_screen_is_decoded_once_across_frames(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        take_local(Metric::AtlasDecodes);
        let (atlas, table, reads) = cx.update(|cx| book(cx, 1, 2, 2));
        let window = cx.open_window(size(px(64.0), px(64.0)), move |_, _| Screen {
            atlas,
            table,
            showing: vec!["pic-0.png".into()],
        });
        let handle: AnyWindowHandle = window.into();
        paint(handle, cx);
        settle(handle, cx);
        for _ in 0..3 {
            paint(handle, cx);
        }
        assert_eq!(
            reads.load(Ordering::Relaxed),
            1,
            "五帧取了五次字节：在场的那张每帧重解，回收就变成了负担"
        );
        assert_eq!(
            take_local(Metric::AtlasDecodes),
            1,
            "计数与容器问数对不上，说明有一条路绕过了图集"
        );
    }

    /// 空帧不误放：换模式 / 邻章补进链那一瞬可能一帧一个 `img` 都不产出。
    ///
    /// 把 [`super::HELD_AFTER_LEAVE`] 降到 0 这一条当场红（空一帧就放，读者看到图在闪）。
    #[gpui_kit::test]
    fn blank_frames_within_the_grace_do_not_reclaim_what_is_still_on_screen(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        let (atlas, table, reads) = cx.update(|cx| book(cx, 1, 2, 2));
        let key = table["pic-0.png"].key.clone();
        let window = cx.open_window(size(px(64.0), px(64.0)), {
            let atlas = atlas.clone();
            move |_, _| Screen {
                atlas,
                table,
                showing: vec!["pic-0.png".into()],
            }
        });
        let handle: AnyWindowHandle = window.into();
        paint(handle, cx);
        settle(handle, cx);
        assert!(held(&atlas, &key, cx).is_some(), "前提：这一张在场");

        for _ in 0..2 {
            window
                .update(cx, |screen, _, _| screen.showing.clear())
                .expect("窗口应当可改");
            paint(handle, cx);
        }
        assert!(
            held(&atlas, &key, cx).is_some(),
            "空两帧就把图放掉：缓冲帧数没起作用"
        );

        window
            .update(cx, |screen, _, _| screen.showing = vec!["pic-0.png".into()])
            .expect("窗口应当可改");
        paint(handle, cx);
        assert_eq!(
            reads.load(Ordering::Relaxed),
            1,
            "回到同一屏又回容器取了一次，说明那两帧空帧真的把格子放了"
        );
    }

    /// 一路往前滚过一整本书：手里攒的量只能是「在场 + 缓冲那一窗」，不是整本书。
    ///
    /// 这就是这次改动的**收益本身**。《未来简史》55 张全部进过一次图集是 126.8 MB
    /// （模块头那两条独立子数出来的）；这条门按同一个形状数。
    ///
    /// 张数与红线都跟着 [`super::HELD_AFTER_LEAVE`] 走，两头都得比它大：
    /// - 书要比缓冲期**长**（`HELD_AFTER_LEAVE + 20` 张，每张走一帧），否则「全收着」
    ///   和「收满缓冲期」是同一个数，回收关掉也不会红 —— 从前这里是 12 张对缓冲期 2 帧，
    ///   缓冲期抬到 30 之后 12 张全在窗内，那条断言就成了空跑的绿；
    /// - 红线 = 缓冲期那一窗 + 富余（在场 1 张 + 缓冲 `HELD_AFTER_LEAVE` 张 + 还在解的
    ///   一张 + 「最近两帧」那一格，再留两帧调度余量）。
    ///
    /// 每张 512² 是 1 MB：整本 50 MB 对红线 36 MB，回收整个没生效时必然翻红，
    /// 而这台测试进程不必真揣着 200 MB 像素。
    #[gpui_kit::test]
    fn scrolling_past_a_whole_book_does_not_keep_every_image(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let grace = super::HELD_AFTER_LEAVE as usize;
        let count = grace + 20;
        // 512×512×4 = 1 MB 一张。
        let (atlas, table, reads) = cx.update(|cx| book(cx, count, 512, 512));
        let each = 512 * 512 * 4;
        let bound = each * (grace + 6);
        let window = cx.open_window(size(px(128.0), px(128.0)), {
            let atlas = atlas.clone();
            move |_, _| Screen {
                atlas,
                table,
                showing: vec!["pic-0.png".into()],
            }
        });
        let handle: AnyWindowHandle = window.into();
        paint(handle, cx);
        settle(handle, cx);
        let mut peak = resident(&atlas, cx);
        for i in 1..count {
            window
                .update(cx, |view, _, _| view.showing = vec![format!("pic-{i}.png")])
                .expect("窗口应当可改");
            paint(handle, cx);
            settle(handle, cx);
            peak = peak.max(resident(&atlas, cx));
        }
        assert!(
            peak <= bound,
            "一路往前滚时手里峰值 {peak} 字节（单张 {each}，缓冲那一窗的红线是 {bound}）：\
             离场的那些没被放掉，那笔账就是整本书",
        );
        assert!(
            reads.load(Ordering::Relaxed) >= count,
            "{count} 张图一次都没被问过字节，说明这一趟根本没走进图集"
        );
    }

    /// 挤出顺序的**排序键**：离焦点页越远越大（越大越先被挤出），容器图一律 0。
    ///
    /// 这一条只钉键怎么算；「封顶那一下真按这个顺序挤」由
    /// [`a_screen_full_of_large_images_is_capped_after_it_leaves`] 与本条合起来覆盖
    /// （那条钉「挤得动」，这条钉「先挤谁」）。
    #[test]
    fn the_eviction_key_is_distance_from_the_focused_page() {
        let key = |page: usize, edge: u32| format!("tok/render/{page}@{edge}");
        assert_eq!(super::distance_of(&key(0, 2048), Some(0)), 0);
        assert_eq!(super::distance_of(&key(3, 2048), Some(0)), 3);
        assert_eq!(super::distance_of(&key(0, 2048), Some(3)), 3);
        assert_eq!(
            super::distance_of("tok/figure-3.png", Some(3)),
            0,
            "容器图没有页号：与从前一样按「最久没在场」排"
        );
        assert_eq!(
            super::distance_of(&key(3, 720), None),
            0,
            "没有焦点页（容器图那一族）时全部同距"
        );
        // 区域图那种带斜杠的键也要拆得出页号（`render/<page>@4096/x0,y0,x1,y1`）。
        assert_eq!(
            super::distance_of("tok/render/7@4096/10,20,30,40", Some(1)),
            6
        );
        // **顺序**：焦点在第 1 页时，第 9 页排在最后（最先被挤）。
        let mut keys = vec![
            (super::distance_of(&key(9, 2048), Some(1)), key(9, 2048)),
            (super::distance_of(&key(1, 2048), Some(1)), key(1, 2048)),
            (super::distance_of(&key(2, 2048), Some(1)), key(2, 2048)),
        ];
        keys.sort_unstable();
        assert_eq!(
            keys.into_iter().map(|(_, key)| key).collect::<Vec<_>>(),
            vec![key(1, 2048), key(2, 2048), key(9, 2048)],
            "焦点页最先保住，离得最远的排在最前（最先被挤）"
        );
    }

    /// 封顶那一档：一屏摆了十几张大图、随后整屏离场，那一窗 grace 之内也要按字节挤。
    ///
    /// 这条钉的是「可见带」判据管不到的那一类：一页摆二十张源图的画册页，按带算全都
    /// 在场。只按带放的话，那一屏 48 MB 要等满 3 帧才走；封顶那一档在它们离场的**第一帧**
    /// 就把它压回 [`super::soft_limit`] 之内。
    #[gpui_kit::test]
    fn a_screen_full_of_large_images_is_capped_after_it_leaves(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (atlas, table, _reads) = cx.update(|cx| book(cx, 12, 1024, 1024));
        let all: Vec<String> = (0..12).map(|i| format!("pic-{i}.png")).collect();
        let window = cx.open_window(size(px(256.0), px(256.0)), {
            let atlas = atlas.clone();
            move |_, _| Screen {
                atlas,
                table,
                showing: all.clone(),
            }
        });
        let handle: AnyWindowHandle = window.into();
        paint(handle, cx);
        settle(handle, cx);
        assert_eq!(
            resident(&atlas, cx),
            12 * 1024 * 1024 * 4,
            "前提：一屏十二张全在场、全记着账"
        );

        // 整屏离场（翻到没有图的那一页）。第一帧就该被封顶挤回来。
        window
            .update(cx, |view, _, _| view.showing.clear())
            .expect("窗口应当可改");
        paint(handle, cx);
        let after = resident(&atlas, cx);
        assert!(
            after <= super::soft_limit(),
            "整屏 48 MB 离场后仍攒着 {after} 字节：封顶那一档没挤动，\
             那一类画册页就把显存按整屏占住"
        );
    }

    /// 配置文件那一格（`reader.image_cache_bytes`）顶上设备分档的那三个数。
    ///
    /// 桌面的软顶是 128 MB，一屏 48 MB 远够不着它 —— 所以「不挤」在这里本来就是对的，
    /// 光看 [`a_screen_full_of_large_images_is_capped_after_it_leaves`] 那条钉不出覆盖。
    /// 把覆盖压到 8 MB，同一屏离场后必须被挤回 8 MB 以内。这条门同时钉两件事：覆盖值
    /// **走进了** `end_frame` 那一步（不是读出来摆着），以及覆盖只换那个数、不换规矩
    /// （在屏上的仍然不挤，见上面那条前提断言）。
    #[gpui_kit::test]
    fn the_configured_cache_bytes_overrides_the_device_tier(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let cap = 8 * 1024 * 1024;
        assert!(
            cap < super::soft_limit(),
            "覆盖值压不到设备档以下（{} 字节），这条门会是空断言",
            super::soft_limit()
        );
        let mut config = folio_core::infra::config::Config::default();
        config.reader.image_cache_bytes = cap as u64;
        // 必须**先装配置再建图集**：覆盖是在 `ImageAtlas::entity` 那一次读进去的。
        cx.update(|cx| crate::ui::services::config::ConfigHolder::install(config, cx));
        let (atlas, table, _reads) = cx.update(|cx| book(cx, 12, 1024, 1024));
        let all: Vec<String> = (0..12).map(|i| format!("pic-{i}.png")).collect();
        let window = cx.open_window(size(px(256.0), px(256.0)), {
            let atlas = atlas.clone();
            move |_, _| Screen {
                atlas,
                table,
                showing: all.clone(),
            }
        });
        let handle: AnyWindowHandle = window.into();
        paint(handle, cx);
        settle(handle, cx);
        assert_eq!(
            resident(&atlas, cx),
            12 * 1024 * 1024 * 4,
            "前提：一屏十二张全在场时就该全收着 —— 覆盖那一档也不许挤在屏上的格子"
        );
        window
            .update(cx, |view, _, _| view.showing.clear())
            .expect("窗口应当可改");
        paint(handle, cx);
        let after = resident(&atlas, cx);
        assert!(
            after <= cap,
            "配置压到 {cap} 字节，离场后仍攒着 {after}：覆盖没走进封顶那一步"
        );
    }

    /// 换一本书：手里全部像素与全部格子都要放掉。
    ///
    /// 会话换书时 `source` 是**原地被替换**的（`session.rs` 那句 `self.source = ...`），
    /// 实体不会释放，所以 `observe_release` 那一条替我们做不了这件事。少了 `clear`，
    /// 上一本的图会整本留在图集里 —— 与这次要修的漏同一个形状，只是换了个入口。
    /// 这条门就是钉这个入口的：关掉 `open_book` 里那一句，这里第一句断言就红。
    #[gpui_kit::test]
    fn switching_books_frees_every_tile_the_session_ever_uploaded(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        take_local(Metric::AtlasFreed);
        let (atlas, table, _reads) = cx.update(|cx| book(cx, 2, 2, 2));
        let keys: Vec<String> = vec![
            table["pic-0.png"].key.clone(),
            table["pic-1.png"].key.clone(),
        ];
        let screen = cx.open_window(size(px(64.0), px(64.0)), {
            let atlas = atlas.clone();
            move |_, _| Screen {
                atlas,
                table,
                showing: vec!["pic-0.png".into(), "pic-1.png".into()],
            }
        });
        let handle: AnyWindowHandle = screen.into();
        paint(handle, cx);
        settle(handle, cx);
        let pixels: Vec<Arc<RenderImage>> = keys
            .iter()
            .filter_map(|key| held(&atlas, key, cx))
            .collect();
        assert_eq!(pixels.len(), 2, "前提：两张都在图集手里");
        for pixels in &pixels {
            assert!(
                screen
                    .update(cx, |_, window, _| window.has_image_atlas_entry(pixels))
                    .expect("窗口应当可读"),
                "前提：两张都已上传"
            );
        }
        assert!(
            asked(&atlas, cx, |atlas| atlas.resident_bytes()) > 0,
            "前提：账上有余量"
        );

        // 换书那一步：会话还活着，实体不会释放，只有 `clear` 会放。
        cx.update(|cx| atlas.update(cx, |atlas, cx| atlas.clear(cx)));
        for pixels in &pixels {
            assert!(
                !screen
                    .update(cx, |_, window, _| window.has_image_atlas_entry(pixels))
                    .expect("窗口应当可读"),
                "换书之后上一本那格的像素还挂在手里：旧书的图会一路攒到关会话为止"
            );
        }
        assert_eq!(
            asked(&atlas, cx, |atlas| atlas.resident_bytes()),
            0,
            "字节账没跟着清，封顶那一档从此按错的数挤人"
        );
        assert_eq!(
            take_local(Metric::AtlasFreed),
            2 * 2 * 2 * 4,
            "放掉两格各 16 字节，累计应当是 32"
        );
    }

    /// 容器答不出来的那张：**一次都不重试**，且不占字节名额。
    ///
    /// 与从前那张 `OnceLock` 缓存 `None` 的行为对齐 —— 一本坏书不该每帧回容器赌一次
    /// （`ui::foundation::images` 的 `a_missing_container_yields_no_image_instead_of_a_panic`
    /// 钉的是同一件事的另一半：索引照旧、那一块空着）。
    #[gpui_kit::test]
    fn a_container_that_cannot_answer_is_not_asked_again_every_frame(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let reads = Arc::new(AtomicUsize::new(0));
        let images = vec![ImageRes {
            href: "gone.png".into(),
            source: "gone.png".into(),
            media_type: "image/png".into(),
            width: 2,
            height: 2,
        }];
        let bytes = {
            let reads = Arc::clone(&reads);
            std::sync::Arc::new(move |_: &str| {
                reads.fetch_add(1, Ordering::Relaxed);
                None
            })
        };
        let chapter =
            ChapterImages::new(Arc::new(BookImages::new(&images, bytes)), "Text/chap.xhtml");
        let request = chapter.decode_request("gone.png").expect("表里有这条");
        let key = request.key.clone();
        let mut table = HashMap::new();
        table.insert("gone.png".to_string(), request);
        let table = Rc::new(table);
        let atlas = cx.update(ImageAtlas::entity);
        let window = cx.open_window(size(px(64.0), px(64.0)), {
            let atlas = atlas.clone();
            move |_, _| Screen {
                atlas,
                table,
                showing: vec!["gone.png".into()],
            }
        });
        let handle: AnyWindowHandle = window.into();
        for _ in 0..4 {
            paint(handle, cx);
            settle(handle, cx);
        }
        assert_eq!(
            reads.load(Ordering::Relaxed),
            1,
            "四帧问了四次容器：失败没被记下来，一本坏书就是每帧一趟解压"
        );
        assert_eq!(resident(&atlas, cx), 0, "解不出来的那张不该占字节名额");
        assert!(held(&atlas, &key, cx).is_none(), "解不出来的那张不该有像素");
    }

    /// 键必须带上「哪一本」：两本书的同名容器路径不能共用同一格像素。
    ///
    /// 撞上的表现是「第二本画出第一本的图」—— 字节来源与固有尺寸都挂在第一次登记那一本
    /// 上，而且看不出任何错。删掉 `BookImages::job` 里的 token 前缀这一条当场红。
    #[test]
    fn two_books_with_the_same_container_path_do_not_share_a_slot() {
        use crate::ui::foundation::images::BookImages;
        use crate::ui::foundation::images::ChapterImages;

        let one = BookImages::new(
            &[ImageRes {
                href: "pic.png".into(),
                source: "pic.png".into(),
                media_type: "image/png".into(),
                width: 2,
                height: 2,
            }],
            Arc::new(|_| Some(TINY_PNG.to_vec())),
        );
        let two = BookImages::new(
            &[ImageRes {
                href: "pic.png".into(),
                source: "pic.png".into(),
                media_type: "image/png".into(),
                width: 2,
                height: 2,
            }],
            Arc::new(|_| Some(TINY_PNG.to_vec())),
        );
        let a = ChapterImages::new(Arc::new(one), "Text/chap.xhtml");
        let b = ChapterImages::new(Arc::new(two), "Text/chap.xhtml");
        let a = a.decode_request("pic.png").expect("表里有这条");
        let b = b.decode_request("pic.png").expect("表里有这条");
        assert_ne!(
            a.key, b.key,
            "两本书的同名容器路径用了同一个键：回收与字节来源都会串到别的本上去"
        );
    }

    /// 正文里的 JPEG 必须按它自己的格式交给 gpui。
    ///
    /// 取字节那一截从作业池回到异步链时只带字节；解码头若写死 `ImageFormat::Png`，
    /// EPUB / CBZ 里占多数的 JPEG 插图会静默解不出来，而测试里清一色 PNG 夹具
    /// 看不见这件事。这条用一张真 JPEG 走完整条路。
    #[gpui_kit::test]
    fn a_jpeg_image_keeps_its_own_format_into_gpui(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let jpeg = {
            let buffer = image::RgbImage::from_pixel(4, 4, image::Rgb([10, 20, 30]));
            let mut out = Vec::new();
            image::DynamicImage::ImageRgb8(buffer)
                .write_to(
                    &mut std::io::Cursor::new(&mut out),
                    image::ImageFormat::Jpeg,
                )
                .expect("测试里的 JPEG 应当编得出来");
            out
        };
        let (atlas, table, key) = cx.update(|cx| {
            let images = vec![ImageRes {
                href: "pic-0.jpg".into(),
                source: "pic-0.jpg".into(),
                media_type: "image/jpeg".into(),
                width: 4,
                height: 4,
            }];
            let bytes: ByteSource =
                Arc::new(move |href: &str| (href == "pic-0.jpg").then(|| jpeg.clone()));
            let chapter =
                ChapterImages::new(Arc::new(BookImages::new(&images, bytes)), "Text/chap.xhtml");
            let request = chapter.decode_request("pic-0.jpg").expect("表里有这张图");
            let key = request.key.clone();
            let table = Rc::new(HashMap::from([("pic-0.jpg".to_string(), request)]));
            (ImageAtlas::entity(cx), table, key)
        });
        let window = cx.open_window(size(px(64.0), px(64.0)), {
            let atlas = atlas.clone();
            move |_, _| Screen {
                atlas,
                table,
                showing: vec!["pic-0.jpg".into()],
            }
        });
        let handle: AnyWindowHandle = window.into();
        settle(handle, cx);
        assert!(
            held(&atlas, &key, cx).is_some(),
            "JPEG 插图必须解得出来；解码头写死 PNG 时这一句会红"
        );
    }
}
