#!/usr/bin/env python3
"""Folio 设置界面（compact 档）SVG 设计稿生成器。

十七个屏的几何常量、主题 token、Lucide 图标表与行构造器全部来自 [`../_kit.py`](../_kit.py)
—— 那一份是各屏共用的尺，本文件只写「设置这一屏摆哪几行」。按 2026-11-05 的定稿：
**行内不摆灰色说明文字、卡片上方摆节头（同一屏只有一块时不摆）** —— 每行只有标题与控件，
块与块之间 32 的缝，节头到自己那张卡 8。几何数字来自 crates/folio-app/src/ui/features/settings/view/compact.rs
的真实实现；颜色取 gpui-kit Default Light / Default Dark 的 token 值；
文案取 locales/zh-CN.toml，取值取 folio-core 配置默认值。
**哪几格实现里还没有，逐条写在各自那一段的注释里**：日志那一整页、字体页的等宽那一格、
外观页的亮度（配置里有值、界面上进不去）。文字页的「翻页时长」与根屏列表里的「听书」那两格
从前也缺，2026-11-05 各补了一头 —— 前者接进了设置，后者补进了稿。
画布 384×853 = 魅族 20（1080×2400 @ 450dpi）的逻辑分辨率。
"""

import math
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from _kit import *  # noqa: E402,F403  一把尺、两份主题、图标表、行构造器全在那一份里

OUT = Path(__file__).parent


# ── 只有设置用得着的两块 ───────────────────────────────────────────────────────

STORAGE_SLICES = [
    ("书籍文件", 684, "55%"),
    ("缩略图缓存", 214, "17%"),
    ("数据库", 128, "10%"),
    ("封面", 118, "10%"),
    ("漫画解压缓存", 96, "8%"),
]


def donut(cx, cy, r_outer, r_inner, T):
    """环：一片一类，从 12 点顺时针，片间 0.02 弧度。"""
    r = (r_outer + r_inner) / 2
    sw = r_outer - r_inner
    total = sum(b for _, b, _ in STORAGE_SLICES)
    a = -math.pi / 2
    parts = []
    for i, (_, bytes_, _) in enumerate(STORAGE_SLICES):
        sweep = 2 * math.pi * bytes_ / total
        a0, a1 = a + 0.01, a + sweep - 0.01
        x0, y0 = cx + r * math.cos(a0), cy + r * math.sin(a0)
        x1, y1 = cx + r * math.cos(a1), cy + r * math.sin(a1)
        large = 1 if (a1 - a0) > math.pi else 0
        parts.append(f'<path d="M {x0:.2f} {y0:.2f} A {r} {r} 0 {large} 1 {x1:.2f} {y1:.2f}" '
                     f'fill="none" stroke="{T["charts"][i]}" stroke-width="{sw}"/>')
        a += sweep
    return parts


def _muted_line(s):
    """一行灰色状态字（服务状态的落点，不是字段的说明）。"""
    return [text(PAD + PAD, ROW / 2 + 4.5, s, 14, "__MUTED__")], ROW


# ── 各屏 ──────────────────────────────────────────────────────────────────────

def membership_row():
    """会员卡那一行：图标 + 两行字（页名、哪天到期）+ 箭头。

    高 60 而不是 44 —— 实现里那一格是 `min_h(44)` 撑到两行（`compact.rs` 的
    `membership_cell`），多一句字这一行长一点，44 那一档不缩。
    """
    tx = PAD + PAD
    cy = 60.0
    return [
        icon("crown", tx + 6, 14, 16, "__FG__"),
        text(tx + ICON_SLOT + 12, 27, "高级会员", 16, "__FG__"),
        text(tx + ICON_SLOT + 12, 47, "30 天后到期", 14, "__MUTED__"),
        icon("chevron-right", RIGHT - 14, cy / 2 - 7, 14, "__MUTED__"),
    ], 60.0


def root_screen(T):
    """根屏：一张会员卡 + 三块带节头的入口 + 末尾一张无节头的卡。

    块与块名 = `compact.rs` 的 `ROOT_BLOCKS`（阅读 / 翻译与 AI / 书与数据），页序仍跟
    `SettingsPage::ALL`。两格不进列表：**会员**顶在根上单独一张卡（当场念等级与到期日），
    **「关于」**落末尾那张无节头的卡 —— 一块只剩一行时，块名念的就是那一行自己的名字。

    实现里现在是 16 页，这份稿画到 15 格：`Logging` 那一页在稿上是「稿先到」（界面上还没有
    入口，只能改 config.json 或环境变量），而它从前占的那一格现在由「TXT 分章规则」与「AI」
    这些真页顶上。整屏的内容比 853 那块画布高 —— 这一屏**滚**（分了块就装不下），最后一张卡
    落在折叠底下，画布到这儿裁掉，不是版式画错了。
    """
    blocks = [
        ("阅读", [("palette", "外观"), ("type", "文字"), ("columns-3", "版面"),
                  ("a-large-small", "字体"), ("book-headphones", "听书"),
                  ("book-open", "词典")]),
        ("翻译与 AI", [("languages", "翻译"), ("sparkles", "AI")]),
        ("书与数据", [("folder-open", "书库"), ("file-text", "TXT 分章规则"),
                     ("list-tree", "目录"), ("arrow-down-up", "同步"), ("hard-drive", "存储"),
                     ("archive", "备份")]),
    ]
    e = status_bar(T)
    y = STATUS + 8
    # 搜索框固定在滚动区**外面**：它是反复用的输入口，滚一下就没得输入不叫省一格。高 32 是
    # Medium 那一档的组件数（`INPUT_SIZES`），不是挑的。
    e += search_box(T, PAD, y, W - 2 * PAD, "", "搜索设置项")
    y += INPUT_SIZES["medium"][0] + 8
    row, rh = membership_row()
    e.append(card(PAD, y, W - 2 * PAD, rh, T))
    e.append('<g transform="translate(0,{:.2f})">{}</g>'.format(y, "".join(resolve(row, T))))
    # 会员卡后面是第一块，它带节头 → 那道缝归 16 那一档（缝跟着「下一块带不带节头」走）。
    y += rh + GAP_HEAD
    for i, (head, pages) in enumerate(blocks):
        els, hh = section_header(head, T)
        e.append('<g transform="translate(0,{:.2f})">{}</g>'.format(y, "".join(resolve(els, T))))
        # 最后一块后面接的是没有节头的「关于」那张 → 留大的一档。
        nxt = GAP_HEAD if i + 1 < len(blocks) else GAP_SECT
        y = draw_card(e, y + hh, [push_row(label, icon_name=ic) for ic, label in pages], T, gap=nxt)
    y = draw_card(e, y, [push_row("关于", icon_name="info")], T)
    e += bottom_bar(T)
    return svg("01-root", e, T["label"],
               "根屏：会员卡 + 三块带节头的入口（阅读 / 翻译与 AI / 书与数据）+ 无节头的关于；整屏滚动", T)


def search_screen(T):
    """搜出来那一屏：框里有词时整屏换成结果，块结构让位。

    两类命中摆成两种东西，因为读者要的两件事不同：页名对上 → 一行带箭头的入口（进去那一屏）；
    某一格的标题对上 → **那一格自己**（这里摆的是「翻页动画」的弹层行与「翻页时长」的步进行），
    当场能改，不必再钻一页去找回刚才那句话。卡上方的节头是命中的那一页的页名。
    """
    e = status_bar(T)
    y = STATUS + 8
    e += search_box(T, PAD, y, W - 2 * PAD, "翻页", "搜索设置项")
    y += INPUT_SIZES["medium"][0] + 8
    # 页命中：一张卡，一行一页。
    row, rh = push_row("文字", icon_name="type")
    e.append(card(PAD, y, W - 2 * PAD, rh, T))
    e.append('<g transform="translate(0,{:.2f})">{}</g>'.format(y, "".join(resolve(row, T))))
    # 页命中那张卡没有节头，但它下面那一堆带节头 → 缝按下一块走，16 那一档。
    y += rh + GAP_HEAD
    # 行命中：按页归堆，节头就是页名，卡里摆的是那一格本身。
    els, hh = section_header("文字", T)
    e.append('<g transform="translate(0,{:.2f})">{}</g>'.format(y, "".join(resolve(els, T))))
    draw_card(e, y + hh, [popup_row("翻页动画", "左右平移"),
                          stepper_row("翻页时长", "280 ms")], T)
    e += bottom_bar(T)
    return svg("14-search", e, T["label"], "搜索：页命中给入口，行命中直接摆那一格的控件", T)


def contrast_on(hex_color):
    """压在某个底色上的字该用黑还是白：预览里的勾不能跟着主色一起消失。"""
    h = hex_color.lstrip("#")
    r, g, b = (int(h[i:i + 2], 16) / 255 for i in (0, 2, 4))
    lum = 0.2126 * r + 0.7152 * g + 0.0722 * b
    return "#0a0a0a" if lum > 0.55 else "#ffffff"


def theme_card(x, y, w, name, bg, primary, fg, selected, T):
    """一张主题卡（compact 两列网格里的一格）。

    实现里卡片内容按端分叉：手机只有「预览三条色带 + 名字」，桌面多一行浅深标签与一块
    `Aa`。这里画的是手机那一档，所以没有标签行 —— 浅深由上方「浅色主题 / 深色主题」
    两个行标题说。选中描边用**当前界面**的主色，勾用被预览主题的主色 + 反差墨色。
    """
    h = 80
    border = T["primary"] if selected else T["border"]
    e = [f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="8" fill="{T["popover"]}" '
         f'stroke="{border}" stroke-width="1"/>']
    # 预览：主题自己的底色；三条色带依次是主色与两档墨色。
    e.append(f'<rect x="{x + 8}" y="{y + 8}" width="{w - 16}" height="36" rx="4" fill="{bg}"/>')
    for bar_y, bar_w, color, opacity in [
        (y + 14, 40, primary, None),
        (y + 22, 58, fg, 0.35),
        (y + 30, 32, fg, 0.18),
    ]:
        alpha = "" if opacity is None else f' opacity="{opacity}"'
        e.append(f'<rect x="{x + 14}" y="{bar_y}" width="{bar_w}" height="4" rx="2" '
                 f'fill="{color}"{alpha}/>')
    if selected:
        e.append(f'<circle cx="{x + w - 16}" cy="{y + 16}" r="8" fill="{primary}"/>')
        e.extend(resolve([icon("check", x + w - 21, y + 11, 10, contrast_on(primary))], T))
    e.append(text(x + 8, y + 62, name, 14, T["fg"]))
    return e, h


def theme_grid_row(title, cards, T):
    """一行「标题 + 两列主题卡」。手机那一档的网格是两列，桌面才是三列。

    `cards` 每项是 `(名字, 底色, 主色, 墨色, 是否选中)`。稿子每个方向只画两格：
    真机上每侧有十来套，这一屏本来就是滚的，画满会把下面「亮度」与自定义主题挤出画布，
    反而看不出这一块的形状。
    """
    e = [text(PAD + PAD, 20, title, 16, T["fg"])]
    gap = 8
    w = (RIGHT - (PAD + PAD) - gap) / 2
    for i, (name, bg, primary, fg, selected) in enumerate(cards[:2]):
        x = PAD + PAD + i * (w + gap)
        els, _ = theme_card(x, 30, w, name, bg, primary, fg, selected, T)
        e.extend(els)
    return e, 30 + 80 + 8


def appearance_screen(T):
    # 「亮度」这一格实现里只在阅读面板那条滑条上（`regions.rs:365-393`），设置页进不去；
    # 稿子把它提到这一页，控件**照抄面板那一条滑条**而不是另配一颗步进 —— 同一个
    # `appearance.brightness` 在两处出现两种控件，读者就得学两套操作。
    # 顶格那一档（1.0）在配置里归成 `None`＝「跟随系统」（config.rs:447-449,477-483），
    # 所以值那一端写的可以是百分比，也可以是那三个字，轨道铺满。
    # 主题不再用下拉框：每一套一张卡，预览用的是那套主题自己的颜色（`pages/appearance.rs`）。
    e = page_screen(T, "外观", [
        [popup_row("界面语言", "简体中文")],
        [popup_row("浅深", "跟随系统"),
         theme_grid_row("浅色主题", [
             ("Default Light", "#ffffff", "#171717", "#0a0a0a", True),
             ("Adventure", "#fdf6e3", "#e07a5f", "#3d405b", False),
         ], T),
         theme_grid_row("深色主题", [
             ("Adventure Dark", "#0b0b0f", "#e07a5f", "#e5e5e5", True),
             ("Ascii", "#111111", "#fafafa", "#d4d4d4", False),
         ], T),
         slider_row("亮度", "80%", 0.8)],
        [titled_row("主题目录", content_path_button("~/.config/folio/themes"))],
    ], heads=["语言", "主题", "自定义主题"])
    return svg("02-appearance", e, T["label"], "外观：界面语言、浅深、主题卡网格、亮度、主题目录", T)


def typography_screen(T):
    # 「翻页时长」跟着「翻页动画」走：两者都是「这一页怎么换过去」的那件事，
    # 而翻页动画这一格本来就落在文字页（`pages/typography.rs:19-188`），不另起一页。
    # 控件是实现里那颗：`turn_duration` 120..700、默认 280（config.rs:463-467），
    # compact 那一档数值行走 `NumberSpec` ⇒ 带「− 值 +」的步进阶梯。
    e = page_screen(T, "文字", [
        [stepper_row("字号", "17"),
         stepper_row("行距", "1.75"),
         stepper_row("段距", "0.75")],
        [stepper_row("首行缩进", "2.00"),
         switch_row("两端对齐", True)],
        [push_row("翻页动画", "左右平移"),
         stepper_row("翻页时长", "280 ms")],
        [push_row("划线方式", "高亮")],
    ], heads=["正文", "段落", "翻页", "划线"])
    return svg("03-typography", e, T["label"], "文字：字号行距段距、缩进对齐、翻页与时长、划线", T)


def layout_screen(T):
    e = page_screen(T, "版面", [
        [stepper_row("最大行长", "0"),
         stepper_row("左右页边距", "24")],
    ])
    return svg("04-layout", e, T["label"], "版面：最大行长与左右页边距", T)


def fonts_screen(T):
    # 「等宽字体」这一格实现里有配置、界面上零入口（`config.rs:603`，默认 `monospace`），
    # 稿子补上：代码与诗那两类排版的字与正文不是一回事，没有这一格就只能跟着正文走。
    e = page_screen(T, "字体", [
        [text_row("正文字体", "LXGW WenKai"),
         text_row("等宽字体", "monospace")],
    ])
    return svg("05-fonts", e, T["label"], "字体：正文与等宽两格家族", T)


def speech_screen(T):
    """听书页：语速、音高与「念到哪翻到哪」。

    这一页实现里**已经有了**（`settings/view/pages/speech.rs:17-67`），缺的是稿子。
    两格数值走 `NumberSpec`，区间 0.5..2.0、步长 0.05、常速 1.0
    （`Speech::RATE_RANGE` / `PITCH_RANGE`，config.rs:537-541）—— 所以画的是步进阶梯
    而不是滑条：判据是「实现里那颗是什么」。

    没有「音色」那一格：Android 上 `voices()` 直接回 `Err`，桌面那份清单在手机上没有
    对应物，第一版不留空挂的字段（config.rs:523-525 那句原话）。
    """
    e = page_screen(T, "听书", [
        [stepper_row("语速", "1.00"),
         stepper_row("音高", "1.00")],
        [switch_row("念到哪翻到哪", False)],
    ], heads=["声音", "朗读时"])
    return svg("12-speech", e, T["label"], "听书：语速与音高两格步进、念到哪翻到哪一开关", T)


def logging_screen(T):
    """日志页：级别、两个出口、按模块加细、日志目录。**实现里还没有这一页**。

    `Logging` 那一节整节没有界面（config.rs:716-736，注释原话「界面上没有这一页」），
    现在只能改 config.json 或 `FOLIO_LOG`。稿子按配置里的四个字段一格一个，
    取值一律用默认值，不自造：

    - 级别六档 `off/error/warn/info/debug/trace`，release 默认 `info`（debug 构建默认
      `debug`，config.rs:749-756）。**依赖库不跟着走**，固定在 warn 以上 —— 这一句
      是行的内容而不是说明，但按「行内不摆灰色说明」那条规矩不画出来，
      代价是读者要按了才知道；
    - `file` 默认开、`stderr` 在移动平台默认关（config.rs:758-761），所以稿里画的是
      开 / 关这一对；
    - `targets` 是 `Vec<String>`，写法同 `RUST_LOG` 的逗号分隔那一段，默认空 ⇒
      框里是空的，不摆占位灰字；
    - `dir` 空串＝用默认 `<数据根>/logs`，这一格摆的是解析出来的那个路径。
    """
    e = page_screen(T, "日志", [
        [popup_row("级别", "info")],
        [switch_row("写到文件", True),
         switch_row("同时写 stderr", False)],
        [text_row("按模块加细", "")],
        [titled_row("日志目录", content_path_button("~/.local/share/folio/logs"))],
    ])
    return svg("13-logging", e, T["label"],
               "日志：级别、两个出口、按模块加细、日志目录（实现里还没有这一页）", T)


def library_screen(T):
    e = page_screen(T, "书库", [
        [text_row("书库位置", ""),
         content_button_leading("选择文件夹…"),
         caption_value_row("建议位置", "/storage/emulated/0/Documents/Folio")],
        [switch_row("监听文件夹", False),
         content_button_leading("选监听目录…"),
         caption_value_row("建议的收件箱", "/storage/emulated/0/Documents/Folio/收件箱")],
        [switch_row("Wi-Fi 传书", False),
         _muted_line("开关打开后，这里会显示能打开的地址与配对码。")],
    ], heads=["位置", "文件夹监听", "Wi-Fi 传书"])
    return svg("06-library", e, T["label"], "书库：位置、文件夹监听、Wi-Fi 传书", T)


def access_screen(T):
    """书库页按了「选择文件夹…」而那道门没拿到授权：这一问摆在卡里，选择器不打开。

    三颗钮按重量排：`取消` 与 `照样挑` 是 ghost，`去开启` 是 primary —— 这一组里默认要走的那
    一条。`取消` 只收面板；另两颗各重跑一次那道门，差别只在没拿到授权时开不开选择器。
    画的是「位置」那一组里的情形：这一问在实现里顶掉的是**按钮那一行**，卡里其余的行不动。
    """
    e = status_bar(T)
    e += top_bar("书库", T)
    y = STATUS + TOPBAR + 8
    els, hh = section_header("位置", T)
    e.append('<g transform="translate(0,{:.2f})">{}</g>'.format(y, "".join(resolve(els, T))))
    y += hh
    title = "这台设备还没有所有文件访问"
    body = ("从这里挑的目录，往里拷书多半会写失败 —— 只有 Download、Documents 和这个应用"
            "自己的目录写得进。可以先去开这一项，也可以照样挑一个。")
    lines = wrap_cjk(body, 14, W - 4 * PAD, 4)
    panel_h = 8 + 22 + 8 + 20 * len(lines) + 8 + BUTTON_H + 8
    row, rh = text_row("书库位置", "/storage/emulated/0/Documents/Folio")
    e.append(card(PAD, y, W - 2 * PAD, rh + 0.5 + panel_h, T))
    e.append('<g transform="translate(0,{:.2f})">{}</g>'.format(y, "".join(resolve(row, T))))
    ry = y + rh + 0.5
    e.append(hairline(ry, PAD + 1 + PAD, W - PAD - 1, T))
    py = ry + 8
    e.append(text(PAD + PAD, py + 16, title, 16, "__FG__", 600))
    by = py + 22 + 8
    for i, line in enumerate(lines):
        e.append(text(PAD + PAD, by + 14 + i * 20, line, 14, "__MUTED__"))
    x = PAD + PAD
    bx = by + 20 * len(lines) + 8
    for label, kind in [("取消", "ghost"), ("照样挑", "ghost"), ("去开启", "primary")]:
        els, bw = button(T, x, bx, label, kind)
        e.extend(resolve(els, T))
        x += bw + 8
    return svg("15-access-question", e, T["label"],
               "书库：没拿到所有文件访问时先问一句，选择器不抢在那前面", T)


def toc_screen(T):
    e = page_screen(T, "目录", [
        [push_row("排列密度", "宽松"),
         stepper_row("标题最大行数", "2"),
         switch_row("读过的章节浅色显示", True)],
    ])
    return svg("07-toc", e, T["label"], "目录：密度、标题行数、已读浅色", T)


def storage_screen(T):
    e = status_bar(T)
    e += top_bar("存储", T)
    y = STATUS + TOPBAR + 8
    # 占用：一整格 —— 读数、环、明细。
    # 「占用」那一句从前画在卡**里面**：它就是规格里那个组标题（`group.usage`），只是手机那屏
    # 不摆节头，于是借了卡内的一行。现在它回到卡上方，与清理、位置那两块同一档；卡内的
    # 读数、环、明细各上移 36（那一句连同它的下垫），卡高跟着减 36。
    els, hh = section_header("占用", T)
    e.append('<g transform="translate(0,{:.2f})">{}</g>'.format(y, "".join(resolve(els, T))))
    y += hh
    total_y, ring_cy, legend_y = y + 40, y + 142, y + 246
    card_h = 374  # 读数那一句要留在卡内 18 的上垫之下，明细走完再留 8
    e.append(card(PAD, y, W - 2 * PAD, card_h, T))
    e.append(text(PAD + PAD, total_y, "1.2 GB", 30, T["fg"], 600))
    e += donut(W / 2, ring_cy, 82, 54, T)
    for i, (name, bytes_, pct) in enumerate(STORAGE_SLICES):
        ly = legend_y + i * 24
        e.append(f'<circle cx="{PAD + PAD + 4}" cy="{ly + 11}" r="4" fill="{T["charts"][i]}"/>')
        e.append(text(PAD + PAD + 16, ly + 16.5, name, 16, T["fg"]))
        e.append(text(280, ly + 16, pct, 14, T["muted"], anchor="end"))
        e.append(text(RIGHT, ly + 16.5, f"{bytes_} MB", 16, T["fg"], anchor="end"))
    y += card_h + GAP_HEAD
    # 清理：一档一行，右端字节 + ghost 小钮。
    els, hh = section_header("清理", T)
    e.append('<g transform="translate(0,{:.2f})">{}</g>'.format(y, "".join(resolve(els, T))))
    clean = [clean_row("缩略图缓存", "214 MB", "broom"),
             clean_row("漫画解压缓存", "96 MB", "broom"),
             clean_row("旧日志", "4.2 MB", "broom"),
             clean_row("整理数据库", "12 MB", "database")]
    y = draw_card(e, y + hh, clean, T)
    # 位置：两行「路径 + 打开所在文件夹」。
    els, hh = section_header("位置", T)
    e.append('<g transform="translate(0,{:.2f})">{}</g>'.format(y, "".join(resolve(els, T))))
    y = draw_card(e, y + hh, [titled_row("软件数据目录", content_path_button("/home/reader/.local/share/folio")),
                              titled_row("日志目录", content_path_button("/home/reader/.local/share/folio/logs"))], T)
    return svg("08-storage", e, T["label"], "存储：占用环图、清理、数据目录（三块各带节头）", T)


def about_screen(T):
    e = page_screen(T, "关于", [
        [push_row("版本", "0.1.1", chevron=False)],
        [titled_row("更新", content_status_ghost("还没查过", "检查更新", None)),
         titled_row("更新日志", content_status_ghost("看这一版改了什么", "查看", None)),
         titled_row("反馈日志", content_status_ghost("还没发过", "发送日志", None))],
    ])
    return svg("09-about", e, T["label"], "关于：版本、更新与反馈", T)


def backup_screen(T):
    """备份页：打一份、列已有的、以及包落在哪儿。

    开关那一格是这一页唯一记住的选择（带不带书文件）；删除在实现里是整行换成确认面板，
    稿子只画常态那一行 —— 确认态与存储页的五档同一形状，不重复画。
    """
    e = page_screen(T, "备份", [
        [switch_row("包含书文件", True),
         content_button_leading("立即备份"),
         _muted_line("包里有 config.json，带着同步密码与"),
         _muted_line("API 密钥，请放在稳妥的地方。")],
        [clean_row("2026-11-05 20:30", "1.2 GB", "trash", "删除"),
         clean_row("2026-11-01 09:12", "48 MB", "trash", "删除")],
        [titled_row("备份文件夹", content_path_button("~/.local/share/folio/backups"))],
    ], heads=["创建备份", "已有备份", "位置"])
    return svg("17-backup", e, T["label"], "备份：包含书文件、立即备份、已有备份与目录", T)


def dictionary_screen(T):
    """词典页：已导入空状态 + 导入入口。

    空状态不是「还没有内容」的占位，而是第一次进来唯一能做的事：这一页不内置任何词典，
    读者手里那个文件夹才是数据源。行内那句只说「要有什么文件」，不写「本产品不提供词典」
    这种解释。
    """
    e = page_screen(T, "词典", [
        [_muted_line("还没有词典。选一个放着 StarDict 词典的文件夹导入。")],
        [content_button_leading("选择词典文件夹…"),
         _muted_line("文件夹里要有 .ifo，连同 .idx / .dict（或 .idx.gz / .dict.dz）。")],
    ], heads=["已导入", "导入"])
    return svg("16-dictionary", e, T["label"], "词典：空状态与导入入口", T)


def pick_screen(T):
    e = status_bar(T)
    e += top_bar("翻页动画", T)
    y = STATUS + TOPBAR + 8
    options = ["左右平移", "上下平移", "左右覆盖", "仿真翻页", "上下滚动", "不走动画"]
    rows = [choice_row(label, i == 0) for i, label in enumerate(options)]
    draw_card(e, y, rows, T)
    return svg("10-pick", e, T["label"], "选项屏：推一屏勾选，选中即写回", T)


SCREENS = [
    ("01-root-light.svg", root_screen, LIGHT),
    ("02-appearance.svg", appearance_screen, LIGHT),
    ("03-typography.svg", typography_screen, LIGHT),
    ("04-layout.svg", layout_screen, LIGHT),
    ("05-fonts.svg", fonts_screen, LIGHT),
    ("06-library.svg", library_screen, LIGHT),
    ("07-toc.svg", toc_screen, LIGHT),
    ("08-storage.svg", storage_screen, LIGHT),
    ("09-about.svg", about_screen, LIGHT),
    ("10-pick.svg", pick_screen, LIGHT),
    ("11-typography-dark.svg", typography_screen, DARK),
    ("12-speech.svg", speech_screen, LIGHT),
    ("13-logging.svg", logging_screen, LIGHT),
    ("14-search.svg", search_screen, LIGHT),
    ("15-access-question.svg", access_screen, LIGHT),
    ("16-dictionary.svg", dictionary_screen, LIGHT),
    ("17-backup.svg", backup_screen, LIGHT),
]

if __name__ == "__main__":
    OUT.mkdir(parents=True, exist_ok=True)
    for name, fn, theme in SCREENS:
        (OUT / name).write_text(fn(theme), encoding="utf-8")
    print("done:", len(SCREENS), "screens")
    render_pngs(OUT)
