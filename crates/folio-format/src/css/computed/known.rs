//! 认识但一期不实现（认得、不生效）的属性名单。
//!
//! 从 css::computed 切出来只因为 computed.rs 贴着行数上限；路径经 re-export 不变。

/// 认识但一期不实现：接受、不生效、不计入 ignored。
///
/// 这一份和下面 `inherit_from` 的区别在于：这些属性**真实出版书里满地都是**
/// （`width`、`text-decoration`、`float`），若按不支持计数，ignored 会被刷到
/// 几百条，真正需要关注的「我们没预料到的写法」就淹没了。
pub(crate) const KNOWN_UNIMPLEMENTED: &[&str] = &[
    // `background` 简写 2026-10-14 起**接住了它里面的颜色**（见 `apply` 的 `background`
    // 那一臂）：出版书里「深色底块 + 白字标题」满地都是，只认 `background-color`
    // 会让这种标题变成白纸上的白字、整条标题消失。2026-10-23 这一轮把 `url(...)`
    // 那一截也接上了，落在 [`ComputedStyle::background_layers`]。
    //
    // 这一族里**只有两条**留在「认得、不生效」这一档：
    // - `background-clip` / `background-origin` —— 一律按**边框盒**，正是出版书里
    //   「底色盖到边框下面」那一大片的写法；`padding-box` / `content-box` 那一档
    //   要给的是内缩的矩形，本书语料里一次没出现过。
    // 留在这份名单里而不是在 `apply` 里返回 `true`：单写这两条时长手确实什么都不改，
    // 报成「接住了」就是把「没做」记成「做了」，`examples/css-gap` 的表会跟着说谎。
    // 简写里的这些词仍然被**吃掉**（不作废整条），判据写在 `apply` 那一臂的注释上。
    //
    // `background-attachment` 2026-10-31 移了出去：`fixed` 挂在章节根上时，排版层
    // 把这一章抬到至少一屏高（浏览器里那是铺满视口画布的整页底图，卷首/扉页正是这么
    // 写的）。绘制仍跟着纸走、不钉在视口上 —— 那一半的降级写在
    // [`BackgroundAttachment`] 上。它现在真会改屏上的东西，所以不能再记成「不生效」。
    "background-clip",
    "background-origin",
    // `min-height` 在这份名单里空转过（认得、不生效）：2026-10-21 与 `min-width` 一起
    // 接进 `used_content_height` / `solve_block_width` 那两夹，所以移了出去。
    //
    // `text-shadow` 2026-10-29 移了出去：绘制层逐字形再画一遍偏移副本（模糊半径按
    // 几份偏移副本近似），`text_shadows` 字段进样式哈希，见 [`TextShadow`]。
    // `direction` 以前在这份名单里（「认得、接受、不生效」），2026-10-14 那一轮把它
    // 变成真生效的属性，所以移出去了。
    // `writing-mode` 同一条路，2026-10-19 那一轮移出去的，而且那一轮排版层真读到了它：
    // 竖排按「逻辑横排 + 一次转置」排，降级清单写在 [`WritingMode`] 的落地程度那一段。
    // 这一档里「认得且不生效」讲的是**这条声明会不会被记进 ignored**，
    // 不是屏上有没有变化；两件事的差别就以这一格为例 —— 移出去的理由是「已经生效」。
    //
    // `orphans` / `widows` / `break-inside` / `page-break-*` 以前在这份名单里
    // （「接受、不生效」），M4 的分片器把它们变成真生效的属性，所以移出去了。
    // `float` / `clear` 同一条路，2026-10-16 那一轮移出去的 —— 现在真会摆浮动盒、
    // 真会把后面的行带折窄，见 `layout::collect` 的 `PlacedFloat`。
    // `columns` 留下：分片器认的是**视口**栏数（`FragmentOptions::columns`，
    // 由界面或读者给），作者写的块级 `column-count` 要改排版期的折行宽度，还没接。
    // `column-gap` 2026-11-23 移了出去：它作为 `gap` 的横轴分量随 Flex 一起接住。
    "columns",
    // 列表标记 2026-10-15 起真的画了，所以 `list-style` 与 `list-style-type` 从这份
    // 名单里移了出去 —— 它们现在会改屏上的东西（`layout::marker`）。
    // `list-style-image` 与 `list-style-position` 2026-10-29 也移了出去：它们不是
    // 「认得不生效」，而是**按初值做就等于做对了**（没有图源、标记挂在流动之外），
    // 所以只收初值、其余值判不认识（见 `apply` 里那两条臂）。
    // `text-transform` 2026-11-24 移了出去：只做**字节长度不变**的大小写变换，
    // 改长度的那一类（`ß → SS`）保留原文，锚点坐标系不受影响。
    // 字体的 OpenType 层：小写大写（`font-variant` / `font-variant-caps`）、横向压缩
    // （`font-stretch`）、字距调整开关（`font-kerning`）、特性开关表（`font-feature-settings`）。
    // 这四条以前**不在这份名单里**，于是被算成「不认识」，`examples/css-gap` 的表就会说
    // 「这些属性我们听都没听过」—— 事实是认得，但要的是 gpui 的 `FontFeatures` 通道
    // （见开发方案 §3.2），那是接字体文件那一层的事，不是层叠期能折出来的数。
    // `font-variant` 与 `font-variant-caps` 2026-10-29 移了出去：它们的**初值** `normal`
    // 就是我们做的事，于是改成只收初值（见 `apply` 里那一条臂）。
    "font-stretch",
    "font-kerning",
    "font-feature-settings",
    // 字距 2026-10-16 起真的生效了（`letter-spacing` 已从这份名单移除），所以这里不再列。
    //
    // `cursor` 是**界面提示**，不是排版属性：它说的是「指针移到这儿该长什么样」。
    // 阅读器里能兑现它的地方只有链接（桌面端本来就对链接给手形），而书里把它写在
    // 5718 个元素上（《毛泽东选集》）只是转换器把网页样式原样带进来。所以记
    // 「认得、不生效」：落差表不再把它报成「我们听都没听过」，而它也确实不该动版面。
    "cursor",
    // 竖排里把两个数字并成一格那一档：注音排在字上方要动的行内收集与行盒高，
    // 它比那一格大得多，留在名单里（`-webkit-text-combine` 单独一条属性，没人消费）。
    "-webkit-text-combine",
    // `ruby-position` / `-epub-ruby-position` 与 `ruby-align` / `-epub-ruby-align`
    // 2026-10-31 一起移了出去：注音排在基字上方/下方（[`RubyPosition`]）与横向贴哪一段
    // （[`RubyAlign`]）都进了 `layout::wrap` 折 ruby 容器那一段，排版层真读这两格。
    // 触发的是《诡秘之主》—— 它是本机第一本带真 `<ruby>`/`<rt>` 的书（552 组、40 章），
    // 所以「没有一本书能验证」那句理由不再成立。`inter-character` 与 `justify` 那一族
    // 仍判不认识，理由写在各自枚举的文档上。
    // 定位一族 2026-10-29 接上了（`position: absolute|fixed` 与四个偏移，见
    // [`Positioning`]）—— 落点是封面/标题页那几块装饰（`left:50%; margin-left:-9.5em`
    // 那套居中写法）。`relative` 2026-11-04 起也收（流内整体位移，见 [`Positioning::Relative`]）；
    // `sticky` 判不认识，它要滚动状态。
    // `z-index` 2026-11-23 移了出去：完整层叠上下文落地（见 `layout::stacking`），
    // 定位盒与 Flex 子项按 z 值排序绘制。
    // 下面这一批是 2026-10-30 从《基督山伯爵》《诡秘之主》两本的落差表里捞出来的：
    // 属性名认得、这一版**没有对应物**，于是单拎出来记「认得、不生效」，不再算进
    // 「不认识」那一栏（两本书各几百到上千个元素，会把真正要关注的写法淹掉）。
    //
    // - `duokan-bleed` 2026-10-30 接上了（`ComputedStyle::bleed`），所以移了出去。
    // - 栅格化与合成提示：前者归渲染后端，后两者要「把底下那一块糊掉」。
    "-webkit-font-smoothing",
    "text-rendering",
    "backdrop-filter",
    "-webkit-backdrop-filter",
    // - flex 一族：2026-11-23 接上了主轴/交叉轴的 Flex 子集（`layout::flex`）：
    //   `flex-direction` / `align-items` / `justify-content` / `flex-wrap` / `gap` 都移了出去。
    //   `align-self` 2026-11-24 接上了（逐项覆盖交叉轴对齐），所以移出了这一档。
];
