#!/usr/bin/env python3
"""Folio 平板档（Medium 竖屏 + Expanded 横屏）SVG 设计稿生成器。

十三屏。两块画布一台设备：小米平板 5（2560×1600 @ density 2.0 ⇒ 逻辑 800×1280），
竖屏落 `SizeClass::Medium`、横屏落 `Expanded`（`shell/navigation/mod.rs:98-104`）。
画布经 `k.set_canvas()` 切，**一把尺仍只有 [`../_kit.py`](../_kit.py) 那一份**。

尺全部来自实现，逐条对得上：

| 这一格 | 数 | 出处 |
| --- | --- | --- |
| 分档 | <600 Compact / <840 Medium / ≥840 Expanded | `shell/navigation/mod.rs:98-104` |
| 侧栏宽 | rail **48** / sidebar **260** | `navigation/mod.rs:22-24`（folio 覆盖框架的 255/48，可调范围在 folio 侧不存在） |
| 侧栏项 | 展开 44（p_2+h_7+p_2）/ rail 30；图标 14、label 14、缝 8、圆角 6 | `sidebar/menu.rs:305-307,329`、`:75` |
| 选中态 | 只换底色 `sidebar_accent` + 前景 + `font_medium`，**无左条无另变色** | `menu.rs:316-320` |
| 品牌头 | **两档都不给**（应用名在桌面归自绘标题栏）；首项起点 = 首组 `pt_3` 12，rail 外层另有 `p_2` 8 | `navigation/side_nav.rs`、`sidebar/mod.rs:474` |
| 内容宽 | 800−48=**752** / 1280−260=**1020** | `shell/mod.rs:1193-1212` |
| 书架列数 | Medium **4 列**（card 176 / cover 264）· Expanded **5 列**（192.8 / 289.2） | `shelf/view/grid.rs:43-50,79,56-59` |
| 平板顶行 | `justify_end`、搜索框定宽 **256**、Small 档（h24 px8 gap4）、右侧只有三点钮 | `toolbar.rs:30-41`、`view/mod.rs:296-300` |
| 统计格子 | Medium cell **10**（room 700）· Expanded cell **9** 且列夹 **720** 居中 | `stats/view/mod.rs:83-98,138,146` |
| 阅读双页 | 两档默认都是 **Spread**：栏宽 364 / 604，中缝 24 | `chrome.rs:56,58,152-159`、`body.rs:227-233` |
| 顶栏那颗 | `wide` 两档皆真 → 右侧多一枚 `ListTree` 开关（ghost small 纯图标 24×24） | `reading_page.rs:677-678`、`regions.rs:48-63` |
| 目录抽屉 | `left 12 / top 56 / bottom 12 / w 320`、圆角 16、正文一寸不让宽 | `toc_panel.rs:90-94,555-597`、`shell/mod.rs:145-150` |
| 设置两栏 | 侧栏 **170** + 分隔条 **9** + 正文 **573 / 841**；两档正文都 >480 ⇒ 都走横排 | `settings/view/desktop.rs:24-26`、`setting/settings.rs:19,424-429` |
| 字段一行 | 左标题列 `flex_1` + 最多 60% + 缝 12 + 右字段；标题 14、说明 14 次要色在下 | `setting/item.rs:276-313` |
| 控件 | switch **36×20**、number_input **128×32**、dropdown **h32** outline | `fields/bool.rs:44-51`、`fields/number.rs:147-152`、`fields/dropdown.rs:65-73` |
| Fill 卡 | 底 `#f5f5f5`、圆角 6、内缩 16、项间 16、组标题 14 次要色 | `group_box.rs:145,152,158-160,174-176` |
| 扫描浮层 | 居中，卡 **460×460**（高定死，里面是虚拟列表）；两张表单卡 420×152 | `shelf/overlay/mod.rs:38-43,103-125` |

**两栏设置在 Android 平板上不成立，这条最要紧**：那道门是
`host::supports_overlay_controls(cx)`（`shell/mod.rs:478-488`），桌面宿主 `true`
（`host/desktop.rs:22-24`）、Android 宿主 `false`（`host/mobile.rs:27-31`，理由是那端浮层点不住，
而「设置项点不动等于设置没有」）。所以**真机平板上「我的」画的仍是手机那份 `CompactSettings`**，
只是被撑到 752 / 1020 宽 —— `03-` 那张画的就是这个现状，它与 `04-` `05-` 那两张两栏形态并排摆着，
差别一眼看得见。两栏那两张因此是「桌面宿主把窗口拉到平板宽度」那一台，不是 Android 平板。

**钻进展屏时侧栏整条不画、不留空位**（`shell/mod.rs:1037-1039` 的 `nav_gone`），而阅读层是
`absolute inset_0` 盖住侧栏、`reserve.width = 0`（`:956-964`、`:145-150`）—— 所以正文按
**整窗宽**算版心，不是按内容列宽。
"""

import random
import sys
from datetime import date, timedelta
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
import _kit as k  # noqa: E402  平板一律现问 k.，不许 import * 把值复制走

OUT = Path(__file__).parent

LIGHT, DARK = k.LIGHT, k.DARK     # 两份主题与画布无关，可以直接别名

PORTRAIT = (800, 1280, 1600, 2560)     # Medium
LANDSCAPE = (1280, 800, 2560, 1600)    # Expanded

GUTTER = 12.0        # shelf/view/grid.rs:23 与统计共用同一个数
GAP = 8.0            # grid.rs:13
MIN_CARD = 168.0     # grid.rs:19
COVER_RATIO = 2 / 3
SEARCH_W = 256.0     # toolbar.rs:41 的 w_64
MORE_W = 24.0        # ghost + small 纯图标
TOOLBAR_H = 8 + 24 + 8
ART = "#d4d4d4"      # 封面位的占位灰，与手机稿同一处理

SET_NAV_W = 170.0    # settings/view/desktop.rs:24
SET_HANDLE_W = 9.0   # resizable/resize_handle.rs:12-13（1 + 4 + 4）
SET_ITEM_H = 44.0
SET_INSET = 12.0
SET_PAGE_H = 57.0    # 页头 p_4 + 发丝线：16 + 25 + 16

MARGIN_X = 24.0      # folio-core config.rs:623
SPREAD_GAP = 24.0    # chrome.rs:58 MIN_SPREAD_GAP
FS, LH = 17.0, 29.75
BAND = 28.0          # chrome.rs:14,16

TOC_W, TOC_INSET, TOC_TOP, TOC_ROW = 320.0, 12.0, 56.0, 46.0

SCAN_W, SCAN_H, POPOVER_PAD = 460.0, 460.0, 12.0

CELL_GAP, LABEL_LANE, MONTH_LANE, WEEKS, ROWS = 3.0, 28.0, 18.0, 53, 7
LEGEND_SQ, CELL_R, COLUMN_MAX = 14.0, 3.0, 720.0
TODAY = date(2026, 10, 24)

SHELF = [
    ("百年孤独", "加西亚·马尔克斯", True), ("瓦尔登湖", "亨利·戴维·梭罗", True),
    ("卡拉马佐夫兄弟", "陀思妥耶夫斯基", True), ("雪国", "川端康成", True),
    ("平沙落雁", None, False), ("人类简史", "尤瓦尔·赫拉利", True),
    ("庄子的蝴蝶", "佚名", False), ("长夜难明", "紫金陈", True),
    ("城南旧事", "林海音", True), ("沙乡年鉴", "奥尔多·利奥波德", True),
    ("夜航西飞", "柏瑞尔·马卡姆", True), ("两京十五日", "马伯庸", False),
    ("缓慢的河流", "伊恩·麦克尤恩", True), ("给青年诗人的信", "里尔克", False),
]
GROUPS = [("梭罗", 6), ("民国书信", 3)]
CHAPTER = [
    ("h1", "湖上"),
    ("p", "清晨我划到湖心去，雾还没散。水面没有一点风，船头推开的地方起一圈圈细纹，"
          "细纹走到雾里就没了。我停下来听，四周只有水拍船底那一下一下。"),
    ("p", "松林在雾里只剩一条深色的边。岸上的颜色比水浅，比天深，说不清是哪一种绿。"
          "我认得这一条边，从远处看它一直在，走近了才发现它是由一棵一棵树站出来的。"),
    ("p", "划桨的手不需要想。想的是另一件事：这一年读过的书，有多少是读第一遍时真在读，"
          "有多少只是眼睛从字上走了一遍。眼睛走得很快，快得什么也没留下。"),
    ("p", "雾散的时候太阳已经高了。湖面重新变成一件东西，而不是一片光。我把船靠到那块"
          "旧石头上，石头上有水痕，一道一道，是这些年水位留下的。"),
    ("p", "回去的路上经过一片浅滩，水草在脚边那么深的地方立着。我停下来看了一会儿。"
          "它们不急着往哪儿去，也不因为有人看就长得更快些。"),
    ("p", "傍晚起了一点风。我把船划回岸边，把缆绳绕在那棵歪掉的桦树上。树干上有一道旧"
          "刻痕，看不清原先写的是什么了。"),
    ("p", "我常想，一个人真正需要的东西比他以为的要少得多。要一间能挡雨的屋子，要一张"
          "能睡的床，要一点能吃的东西。剩下的都是想，而想是可以放下的。"),
    ("p", "放下之后剩下的时间做什么？我做了一整年的书。不是读，是做：把借来的句子拆开，"
          "看看它为什么在那里，看看换一种摆法它还成不成。"),
    ("p", "有些句子换了就不成了。有些换了还是成了，那说明它本来也没那么重要。这一条"
          "放在读别人身上同样好用。"),
    ("p", "湖不认识我。它在这里的时候我还不在，我走了它还在。这话听起来伤感，其实不是："
          "它说的是尺度，而尺度是能让人安静下来的东西。"),
    ("p", "风停的时候已经下午了。水面上重新铺了一层光，像有人把一整匹布抖开又按住。"
          "我把桨收进船里，让它自己漂一会儿。"),
    ("p", "漂到芦苇边停住。苇秆上有去年留下的枯穗，风一过就响。这种声音不需要谁去听，"
          "它自己就是完整的。"),
    ("p", "我记下这些的时候用的是铅笔。铅笔的好处是可以擦，擦过的地方留一道浅痕，"
          "提醒我这一句曾经不是这样。"),
]

SET_PAGES = [("palette", "外观", True), ("type", "文字", True), ("columns-3", "版面", False),
             ("a-large-small", "字体", False), ("folder-open", "书库", True),
             ("list-tree", "目录", False), ("hard-drive", "存储", True), ("info", "关于", True)]


# ── 骨架 ──────────────────────────────────────────────────────────────────────

def use(canvas):
    """切画布。返回这一档要用的三格：窗宽、窗高、侧栏宽。"""
    k.set_canvas(*canvas)
    return {"W": float(canvas[0]), "H": float(canvas[1]),
            "nav": k.RAIL_W if canvas == PORTRAIT else k.SIDEBAR_W}


def chrome(T, cv, current, collapsed):
    """状态栏 + 侧栏（侧栏从安全区下面起画）。返回 (元素表, 内容左缘, 内容宽)。"""
    e = k.status_bar(T)
    e += k.side_nav(T, current, collapsed, k.H - k.STATUS, k.STATUS)
    return e, cv["nav"], k.W - cv["nav"]


def canvas_bg(T):
    return [f'<rect x="0" y="0" width="{k.W}" height="{k.H}" fill="{T["bg"]}"/>']


def note(text):
    """把实算出来的数留在 SVG 里当注释 —— 看图的人能知道那一格是怎么来的。"""
    return f"<!-- {text} -->"


# ── 书架 ──────────────────────────────────────────────────────────────────────

def shelf_grid(content_w):
    """列数与格宽按 grid.rs:43-50,79,56-59 那套实算。"""
    avail = content_w - 2 * GUTTER
    cols = int((avail + GAP) // (MIN_CARD + GAP))
    card = (avail - GAP * (cols - 1)) / cols
    return cols, card, card / COVER_RATIO


def cover_art(T, x, y, w, h):
    return (f'<rect x="{x + 0.5:.1f}" y="{y + 0.5:.1f}" width="{w - 1:.1f}" '
            f'height="{h - 1:.1f}" rx="{k.RADIUS - 0.5}" fill="{ART}" fill-opacity="0.35"/>')


def book_cell(T, x, y, card, cover, title, author, has_art):
    e = k.book_cover(T, x, y, card, cover, title=None if has_art else title,
                     art=cover_art(T, x, y, card, cover) if has_art else None)
    e.append(k.text(x, y + cover + 4 + 15, k.truncate(title, 16, card), 16, T["fg"]))
    if author:
        e.append(k.text(x, y + cover + 4 + 20 + 4 + 12, k.truncate(author, 12, card),
                        12, T["muted"]))
    return e


def group_cell(T, x, y, card, cover, name, count):
    pad, tgap = 6.0, 4.0
    tw = (card - 2 * pad - tgap) / 2
    th = (cover - 2 * pad - tgap) / 2
    e = [f'<rect x="{x:.2f}" y="{y:.2f}" width="{card:.2f}" height="{cover:.2f}" '
         f'rx="{k.RADIUS}" fill="{T["popover"]}"/>']
    for i in range(4):
        tx = x + pad + (i % 2) * (tw + tgap)
        ty = y + pad + (i // 2) * (th + tgap)
        filled = i < 3
        e.append(f'<rect x="{tx:.1f}" y="{ty:.1f}" width="{tw:.1f}" height="{th:.1f}" rx="2" '
                 f'fill="{ART if filled else T["muted_bg"]}" '
                 f'fill-opacity="{0.35 if filled else 1}"/>')
    e.append(k.text(x, y + cover + 4 + 15, k.truncate(name, 16, card), 16, T["fg"]))
    e.append(k.text(x, y + cover + 4 + 20 + 4 + 12, f"{count} 本", 12, T["muted"]))
    return e


def shelf_toolbar(T, content_x, content_w, value=""):
    """平板顶行 `justify_end`：定宽搜索框贴右，右边只剩一颗三点钮。"""
    y = k.STATUS + 8.0
    right = content_x + content_w - GUTTER
    e = k.search_box(T, right - MORE_W - GAP - SEARCH_W, y, SEARCH_W, value,
                     "搜索书名或作者", size="small")
    e.append(k.icon("ellipsis-vertical", right - MORE_W + 5, y + 5, 14, T["fg"]))
    return e


def shelf_cells(T, content_x, content_w, groups, books, top):
    cols, card, cover = shelf_grid(content_w)
    cell_h = cover + 43
    cells = [group_cell(T, 0, 0, card, cover, n, c) for n, c in groups]
    cells += [book_cell(T, 0, 0, card, cover, t, a, art) for t, a, art in books]
    e = []
    for i, cell in enumerate(cells):
        row, col = divmod(i, cols)
        y = top + row * (cell_h + GAP)
        if y > k.H - cell_h:
            break
        e.append(f'<g transform="translate({content_x + GUTTER + col * (card + GAP):.2f},'
                 f'{y:.2f})">{"".join(cell)}</g>')
    return e, cols, card, cover


def shelf_screen(T, cv, collapsed):
    e, cx, cw = chrome(T, cv, "书架", collapsed)
    e += shelf_toolbar(T, cx, cw)
    cells, cols, card, cover = shelf_cells(T, cx, cw, GROUPS, SHELF,
                                           k.STATUS + TOOLBAR_H + 4)
    e += cells
    e.append(note(f"内容宽 {cw:.0f} − 24 = {cw - 24:.0f} 可用 ⇒ {cols} 列，"
                  f"一格 {card:.1f} × 封面 {cover:.1f}"))
    return e


# ── 设置：Android 平板的现状 ───────────────────────────────────────────────────

def settings_compact_screen(T, cv, collapsed):
    """真机平板上「我的」那一格：手机那份钻入式列表，宽度跟着内容列放大。"""
    e, cx, cw = chrome(T, cv, "我的", collapsed)
    y = k.STATUS + 8.0
    rows = [("palette", "外观"), ("type", "文字"), ("columns-3", "版面"),
            ("a-large-small", "字体"), ("folder-open", "书库"), ("list-tree", "目录"),
            ("hard-drive", "存储"), ("info", "关于")]
    h = len(rows) * k.ROW + (len(rows) - 1) * 0.5
    e.append(k.card(cx + k.PAD, y, cw - 2 * k.PAD, h, T))
    for i, (ic, label) in enumerate(rows):
        ry = y + i * (k.ROW + 0.5)
        row, _ = k.push_row(label, icon_name=ic)
        e.append(f'<g transform="translate({cx:.2f},{ry:.2f})">'
                 f'{"".join(k.resolve(row, T))}</g>')
        if i:
            e.append(k.hairline(ry, cx + k.PAD + 1 + 56, cx + cw - k.PAD - 1, T))
    e.append(note(f"卡片宽 {cw - 2 * k.PAD:.0f}：手机那一档它是 352，这里跟着内容列一起放大 —— "
                  f"两栏的门是 host 不是档位（shell/mod.rs:478-488）"))
    return e


# ── 设置：两栏（桌面宿主拉到平板宽度）──────────────────────────────────────────

def settings_sidebar(T, x, y, w, h, selected):
    """设置自己的侧栏：搜索框在上，八个页名在下，多组页带一枚展开箭头。"""
    e = [f'<rect x="{x}" y="{y}" width="{w}" height="{h}" fill="{T["sidebar_bg"]}"/>',
         f'<rect x="{x + w - 1}" y="{y}" width="1" height="{h}" '
         f'fill="{T["sidebar_border"]}"/>']
    yy = y + 12.0
    e += k.search_box(T, x + SET_INSET, yy, w - 2 * SET_INSET, "", "搜索设置项")
    yy += k.BUTTON_H + 12.0
    for ic, label, expandable in SET_PAGES:
        on = label == selected
        if on:
            e.append(f'<rect x="{x + SET_INSET}" y="{yy}" width="{w - 2 * SET_INSET}" '
                     f'height="{SET_ITEM_H}" rx="{k.RADIUS}" fill="{T["sidebar_accent"]}"/>')
        ink = T["sidebar_accent_fg"] if on else T["fg"]
        e.append(k.icon(ic, x + SET_INSET + 8, yy + (SET_ITEM_H - 14) / 2, 14, ink))
        e.append(k.text(x + SET_INSET + 8 + 14 + 8, yy + SET_ITEM_H / 2 + 5, label, 14, ink,
                        500 if on else 400))
        if expandable:
            e.append(k.icon("chevron-right", x + w - SET_INSET - 20,
                            yy + SET_ITEM_H / 2 - 6, 12, T["muted"]))
        yy += SET_ITEM_H + 8
    return e


def field_dropdown(T, right, cy, label):
    w = 10 + k.tw(label, 14) + 6 + 14 + 10
    x = right - w
    return ([f'<rect x="{x:.1f}" y="{cy - 16}" width="{w:.1f}" height="32" rx="{k.RADIUS}" '
             f'fill="none" stroke="{T["border"]}" stroke-width="1"/>',
             k.text(x + 10, cy + 5, label, 14, T["fg"]),
             k.icon("chevron-down", x + w - 22, cy - 6, 12, T["fg"])]), w


def field_number(T, right, cy, value):
    x, w = right - 128.0, 128.0
    return ([f'<rect x="{x}" y="{cy - 16}" width="{w}" height="32" rx="{k.RADIUS}" '
             f'fill="none" stroke="{T["border"]}" stroke-width="1"/>',
             k.text(x + 10, cy + 5, value, 14, T["fg"])]), w


def field_path_button(T, right, cy, path):
    """「主题目录」那一格：灰色路径字 + 一颗 secondary 按钮，**不是下拉**。

    实现里这是一条 `Row::custom_titled`，桌面那头渲染的是 `SettingField::render`：
    一个 h_flex，左边 `muted_label(路径)`，右边 `open_folder_button`（普通 Button）。
    画成带下箭头的那颗就是撒谎 —— 它点开的是系统文件管理器，不是一个选项表。
    """
    label = "打开所在文件夹"
    bw = 10 + k.tw(label, 14) + 10
    bx = right - bw
    return ([k.text(right - bw - 12 - k.tw(path, 14), cy + 5, path, 14, T["muted"]),
             f'<rect x="{bx}" y="{cy - 16}" width="{bw}" height="32" rx="{k.RADIUS}" '
             f'fill="{T["btn_bg"]}"/>',
             k.text(bx + 10, cy + 5, label, 14, T["fg"])]), bw


def field_switch(T, right, cy, on):
    return [k.switch_medium(T, right - 36, cy - 10, on)], 36.0


def setting_item(T, x, y, w, title, desc, field):
    """横排一项：左标题列（`flex_1` + 最多 60%）+ 缝 12 + 右字段。"""
    e = [k.text(x, y + 15, title, 14, T["fg"])]
    if desc:
        e.append(k.text(x, y + 33, k.truncate(desc, 14, w * 0.6), 14, T["muted"]))
    part, _ = field(T, x + w, y + 22)
    return e + part


def group_box(T, x, y, w, title, items):
    """`GroupBoxVariant::Fill` 那一档：灰底卡、圆角 6、内缩 16、项间 16。"""
    n = len(items)
    h = 16 + (18 + 12 if title else 0) + n * 44 + (n - 1) * 16 + 16
    e = [f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{k.RADIUS}" '
         f'fill="{T["group_box_bg"]}"/>']
    yy = y + 16
    if title:
        e.append(k.text(x + 16, yy + 13, title, 14, T["muted"]))
        yy += 18 + 12
    for title_, desc, field in items:
        e += setting_item(T, x + 16, yy, w - 32, title_, desc, field)
        yy += 44 + 16
    return e, y + h + 16


def settings_two_pane_screen(T, cv, collapsed, selected, page_title, groups):
    """应用侧栏 + 设置侧栏 170 + 分隔条 9 + 正文。"""
    e, cx, cw = chrome(T, cv, "我的", collapsed)
    body_x = cx + SET_NAV_W + SET_HANDLE_W
    body_w = cx + cw - body_x
    e += settings_sidebar(T, cx, k.STATUS, SET_NAV_W, k.H - k.STATUS, selected)
    e.append(f'<rect x="{cx + SET_NAV_W}" y="{k.STATUS}" width="{SET_HANDLE_W}" '
             f'height="{k.H - k.STATUS}" fill="{T["bg"]}"/>')
    y = k.STATUS
    e.append(k.text(body_x + 16, y + 16 + 21, page_title, 16, T["fg"]))
    e.append(k.hairline(y + SET_PAGE_H - 1, body_x, body_x + body_w, T))
    y += SET_PAGE_H + 16
    for title, items in groups:
        part, y = group_box(T, body_x + 16, y, body_w - 32, title, items)
        e += part
    e.append(note(f"正文宽 {body_w:.0f} > STACKED_LAYOUT_MAX_WIDTH 480 ⇒ 走一行一项的横排；"
                  f"标题列最多 60%（setting/item.rs:276-313）"))
    return e


# ── 统计 ──────────────────────────────────────────────────────────────────────

def stats_geometry(content_w):
    """cell_size 那套实算（stats/view/mod.rs:83-98,138）。"""
    inset = 2 * GUTTER + LABEL_LANE
    wide = content_w >= 840.0          # 只有 Expanded 夹 COLUMN_MAX
    lane = min(content_w, COLUMN_MAX) if wide else content_w
    room = lane - inset
    pitch = int((room + CELL_GAP) // WEEKS)
    cell = max(8.0, min(float(pitch - CELL_GAP), 14.0))
    return cell, cell + CELL_GAP, wide, lane


def ramp(T):
    solid = "#2563eb"
    return [T["muted_bg"], f"{solid}|0.30", f"{solid}|0.55", f"{solid}|0.80", solid]


def square(x, y, size, color, T):
    hexv, _, alpha = color.partition("|")
    a = f' fill-opacity="{alpha}"' if alpha else ""
    return (f'<rect x="{x:.1f}" y="{y:.1f}" width="{size}" height="{size}" rx="{CELL_R}" '
            f'fill="{hexv}"{a}/>')


def start_monday():
    return (TODAY - timedelta(days=TODAY.weekday())) - timedelta(weeks=WEEKS - 1)


def stats_screen(T, cv, collapsed):
    e, cx, cw = chrome(T, cv, "统计", collapsed)
    y = k.STATUS + 8.0
    e.append(k.text(cx + GUTTER, y + 11, "近 12 个月", 14, T["muted"]))
    y += 28
    cell, pitch, wide, lane = stats_geometry(cw)
    gx = cx + (cw - lane) / 2 + GUTTER + LABEL_LANE
    top = y + MONTH_LANE
    last = -99
    for col in range(WEEKS):
        d = start_monday() + timedelta(weeks=col)
        if col - last < 4:
            continue
        e.append(k.text(gx + col * pitch, top - 5,
                        str(d.year) if d.month == 1 else f"{d.month} 月", 12, T["muted"]))
        last = col
    for row, label in ((0, "周一"), (2, "周三"), (4, "周五")):
        e.append(k.text(gx - LABEL_LANE + 4, top + row * pitch + cell - 1, label, 12, T["muted"]))
    colors, rng = ramp(T), random.Random(20261024)
    read = 0
    for i in range(WEEKS * ROWS):
        d = start_monday() + timedelta(days=i)
        if d > TODAY:
            continue
        age = (TODAY - d).days
        chance = (0.34 if age > 300 else 0.52) + (0.24 if d.weekday() >= 5 else 0.0)
        if 150 <= age <= 185 or rng.random() > chance:
            continue
        minutes = rng.choice([8, 20, 40, 75])
        level = 1 + sum(1 for t in (15, 30, 60) if minutes >= t)
        read += 1
        e.append(square(gx + (i // ROWS) * pitch, top + (i % ROWS) * pitch,
                        cell, colors[level], T))
    y = top + ROWS * pitch - CELL_GAP + 8
    w = 14 + 4 + 5 * LEGEND_SQ + 4 * 4 + 14
    x = cx + (cw + lane) / 2 - GUTTER - w
    e.append(k.text(x, y + LEGEND_SQ - 2, "少", 14, T["muted"]))
    x += 14 + 4
    for i in range(5):
        e.append(square(x, y, LEGEND_SQ, colors[i], T))
        x += LEGEND_SQ + 4
    e.append(k.text(x, y + LEGEND_SQ - 2, "多", 14, T["muted"]))
    y += LEGEND_SQ + 8
    e.append(k.text(cx + GUTTER, y + 11,
                    f"读了 {read} 天，一共 118 小时 25 分钟，最长连续 11 天", 14, T["muted"]))
    e.append(note(f"cell {cell:.0f} / pitch {pitch:.0f}，列{'夹在 720 居中' if wide else '不夹'}；"
                  f"网格内容宽 {WEEKS * pitch - CELL_GAP:.0f} < 可用 {cw - 2 * GUTTER - LABEL_LANE:.0f}"
                  f" ⇒ 这一档不横向滚（手机那一档要滚）"))
    return e


# ── 阅读页 ────────────────────────────────────────────────────────────────────

def para_lines(prose, col_w):
    out, cur, first = [], "", True
    for ch in prose:
        limit = col_w - (FS * 2.0 if first else 0)
        if cur and k.tw(cur + ch, FS) > limit:
            out.append((cur, first))
            cur, first = ch, False
        else:
            cur += ch
    if cur:
        out.append((cur, first))
    return out


def column(T, e, x, col_w, top, bottom, start=0):
    """一栏：h1 + 若干段，画满为止。返回下一个没排下的块序号。"""
    y = top
    for n in range(start, len(CHAPTER)):
        kind, prose = CHAPTER[n]
        if kind == "h1":
            if y + 34 * 1.75 > bottom:
                return n
            y += 34 * 1.75
            e.append(k.text(x, y, prose, 34, T["fg"], 700))
            y += 0.67 * 34
            continue
        lines = para_lines(prose, col_w)
        for i, (line, first) in enumerate(lines):
            if y + LH > bottom:
                return n
            lw = col_w - (FS * 2.0 if first else 0)
            lx = x + (FS * 2.0 if first else 0)
            y += LH
            if i < len(lines) - 1:
                e.append(f'<text x="{lx:.1f}" y="{y:.1f}" font-family="{k.FONT}" '
                         f'font-size="{FS}" fill="{T["fg"]}" textLength="{lw:.1f}" '
                         f'lengthAdjust="spacing">{line}</text>')
            else:
                e.append(k.text(lx, y, line, FS, T["fg"]))
        y += FS * 0.75
    return len(CHAPTER)


def reader_spread(T, e, top, bottom):
    """双页：按整窗宽算版心，中缝 24，两栏各排一半。"""
    content = k.W - 2 * MARGIN_X
    col_w = (content - SPREAD_GAP) / 2
    n = column(T, e, MARGIN_X, col_w, top, bottom, 0)
    if n < len(CHAPTER):
        column(T, e, MARGIN_X + col_w + SPREAD_GAP, col_w, top, bottom, n + 1)
    e.append(note(f"版心 {content:.0f} ⇒ 双页，栏宽 {col_w:.0f}（一行 {int(col_w / FS)} 字 @17）；"
                  f"判据 spread_fits：栏宽 ≥ MIN_COLUMN_WIDTH 200"))


def reading_clean_screen(T, cv):
    """干净态：上下各一条 28 的带，中间双页。侧栏被阅读层整条盖住。"""
    e = canvas_bg(T)
    reader_spread(T, e, k.STATUS + BAND, k.H - BAND)
    e.append(f'<rect x="0" y="{k.STATUS}" width="{k.W}" height="{BAND}" fill="{T["bg"]}"/>')
    e.append(k.text(16, k.STATUS + 2 + 17, "湖上", 12, T["muted"]))
    fy = k.H - BAND
    e.append(f'<rect x="0" y="{fy}" width="{k.W}" height="{BAND}" fill="{T["bg"]}"/>')
    e.append(k.text(16, fy + 18, "第 47 / 218 页", 12, T["muted"]))
    e.append(k.text(k.W - 16, fy + 18, "22%", 12, T["muted"], anchor="end"))
    e = k.status_bar(T) + e
    return e


def reading_chrome_screen(T, cv):
    """浮层态：顶栏 44（右侧多出目录开关），底栏一张满幅卡、四格入口。"""
    e = canvas_bg(T)
    reader_spread(T, e, k.STATUS + k.TOPBAR + 8, k.H - 152)
    e.append(f'<rect x="0" y="{k.STATUS}" width="{k.W}" height="{k.TOPBAR}" fill="{T["bg"]}"/>')
    cy = k.STATUS + k.TOPBAR / 2
    e.append(k.icon("arrow-left", 20, cy - 10, 20, T["fg"]))
    e.append(k.text(50, cy - 1, "瓦尔登湖", 14, T["fg"], 600))
    e.append(k.text(50, cy + 13, "湖上", 14, T["muted"]))
    e.append(k.icon("list-tree", k.W - 16 - 24 + 5, cy - 7, 14, T["fg"]))
    card_h = 12 + 17 + 12 + 24 + 12 + 40 + 8
    top = k.H - card_h
    e.append(f'<rect x="0" y="{top}" width="{k.W}" height="{card_h}" rx="{k.CARD_R}" '
             f'fill="{T["popover"]}"/>')
    y = top + 12
    e.append(k.text(16, y + 13, "进度 22%", 14, T["muted"]))
    y += 29
    e.append(k.icon("chevron-left", 16, y + 2, 20, T["fg"]))
    e.append(k.icon("chevron-right", k.W - 36, y + 2, 20, T["fg"]))
    track = k.W - 2 * 16 - 56
    e.append(f'<rect x="44" y="{y + 10}" width="{track:.0f}" height="4" rx="2" '
             f'fill="{T["muted_bg"]}"/>')
    e.append(f'<circle cx="{44 + track * 0.22:.1f}" cy="{y + 12}" r="9" '
             f'fill="{T["primary"]}"/>')
    y += 36
    for i, (ic, label) in enumerate((("list-tree", "目录"), ("settings-2", "设置"),
                                     ("columns-3", "双页"), ("moon", "夜间"))):
        cx = k.W * (i + 0.5) / 4
        e.append(k.icon(ic, cx - 9, y + 2, 18, T["fg"]))
        e.append(k.text(cx, y + 32, label, 12, T["fg"], anchor="middle"))
    e = k.status_bar(T) + e
    e.append(note(f"底栏卡片满幅不收窄（regions.rs:89-93 是 absolute left_0 right_0）；"
                  f"每格实宽 {(k.W - 32) / 4:.0f}；顶栏右侧那颗是 wide 才长出来的目录开关"))
    return e


def toc_drawer_screen(T, cv):
    """目录：平板上是左抽屉 320，不是手机那个底部弹层。"""
    e = reading_clean_screen(T, cv)
    e.append(k.scrim(T))
    h = k.H - TOC_TOP - TOC_INSET
    e.append(f'<rect x="{TOC_INSET}" y="{TOC_TOP}" width="{TOC_W}" height="{h}" rx="16" '
             f'fill="{T["popover"]}" stroke="{T["border"]}" stroke-width="1"/>')
    e.append(f'<rect x="{TOC_INSET + 1}" y="{TOC_TOP + 1}" width="{TOC_W - 2}" '
             f'height="11" fill="{T["popover"]}"/>')
    e.append(f'<rect x="{TOC_INSET}" y="{TOC_TOP + 12}" width="48" height="72" '
             f'rx="{k.RADIUS}" fill="{T["muted_bg"]}"/>')
    tx = TOC_INSET + 60
    e.append(k.text(tx, TOC_TOP + 28, "瓦尔登湖", 16, T["fg"]))
    e.append(k.text(tx, TOC_TOP + 50, "亨利·戴维·梭罗", 14, T["muted"]))
    e.append(k.text(tx, TOC_TOP + 72, "读到 22% · EPUB · 1.8 MB", 14, T["muted"]))
    y = TOC_TOP + 92
    e.append(k.hairline(y, TOC_INSET, TOC_INSET + TOC_W, T))
    cy = y + 22
    e.append(k.text(TOC_INSET + 15, cy + 5, "已读 6 / 18 章", 14, T["muted"]))
    bx = TOC_INSET + TOC_W - 15
    for label, on in (("全文", False), ("目录", True)):
        bw = 8 + k.tw(label, 14) + 8
        bx -= bw
        if on:
            e.append(f'<rect x="{bx:.1f}" y="{cy - 12}" width="{bw:.1f}" height="24" '
                     f'rx="{k.RADIUS}" fill="{T["muted_bg"]}"/>')
        e.append(k.text(bx + 8, cy + 5, label, 14, T["fg"]))
        bx -= 4
    y += 44
    ITEMS = [(0, "经济状", False, False), (0, "我生活的地方；我为何生活", True, True),
             (1, "读", False, False), (1, "声音", False, False), (0, "湖", False, True),
             (1, "湖的岸线", False, False), (1, "瓦尔登湖的宝藏", False, False),
             (0, "贝克斯菲尔德的来访者", False, False), (0, "种豆", False, False),
             (0, "镇", False, False)]
    for level, title, current, read in ITEMS:
        if y + TOC_ROW > TOC_TOP + h - 8:
            break
        if current:
            e.append(f'<rect x="{TOC_INSET + 1}" y="{y}" width="{TOC_W - 2}" '
                     f'height="{TOC_ROW}" fill="{T["muted_bg"]}"/>')
            e.append(f'<rect x="{TOC_INSET + 2.1}" y="{y + 10}" width="2.9" '
                     f'height="{TOC_ROW - 20}" rx="1.5" fill="{T["primary"]}"/>')
        e.append(k.text(TOC_INSET + 31 + level * 11.4, y + TOC_ROW / 2 + 5,
                        k.truncate(title, 14, TOC_W - 60), 14,
                        T["muted"] if read and not current else T["fg"],
                        600 if current else 400))
        y += TOC_ROW
    e.append(note(f"抽屉 left {TOC_INSET} / top {TOC_TOP} / bottom {TOC_INSET} / w {TOC_W:.0f}，"
                  f"圆角 16；行高 {TOC_ROW:.0f} 与手机同一档（装配函数同一份）；正文一寸不让宽"))
    return e


# ── 扫描：平板上是居中浮层 ─────────────────────────────────────────────────────

def scan_popover_screen(T, cv, collapsed):
    e, cx, cw = chrome(T, cv, "书架", collapsed)
    e += shelf_toolbar(T, cx, cw)
    cells, _, _, _ = shelf_cells(T, cx, cw, [], SHELF, k.STATUS + TOOLBAR_H + 4)
    e += cells
    e.append(k.scrim(T))
    x = (k.W - SCAN_W) / 2
    y = (k.H - (SCAN_H + 2 * POPOVER_PAD)) / 2
    e.append(f'<rect x="{x:.1f}" y="{y:.1f}" width="{SCAN_W}" '
             f'height="{SCAN_H + 2 * POPOVER_PAD}" rx="{k.RADIUS}" fill="{T["popover"]}" '
             f'stroke="{T["border"]}" stroke-width="1"/>')
    yy = y + POPOVER_PAD + 16
    e.append(k.text(x + 16, yy + 16, "在设备里找书", 16, T["fg"], 600))
    e.append(k.text(x + 16, yy + 38, "找到 12 本，勾选后导入", 14, T["muted"]))
    yy += 52
    e.append(k.hairline(yy, x, x + SCAN_W, T))
    yy += 1
    for name, size, checked in [
            ("沙乡年鉴.epub", "1.2 MB", True), ("夜航西飞.epub", "860 KB", True),
            ("缓慢的河流.pdf", "4.4 MB", False), ("给青年诗人的信.txt", "22 KB", True),
            ("湖岸船坞.md", "9 KB", False), ("月光下的裁缝.epub", "2.7 MB", False),
            ("两京十五日.epub", "5.1 MB", False), ("瓦尔登湖.epub", "1.8 MB", True)]:
        e.append(k.text(x + 44, yy + 23,
                        k.truncate(name, 16, SCAN_W - 44 - 16 - 70), 16, T["fg"]))
        e.append(k.text(x + SCAN_W - 16, yy + 23, size, 12, T["muted"], anchor="end"))
        box = f'<rect x="{x + 16}" y="{yy + 9}" width="16" height="16" rx="3"'
        e.append(box + f' fill="{T["primary"]}"/>' if checked else
                 box + f' fill="none" stroke="{T["border"]}" stroke-width="1"/>')
        yy += 36
    e.append(k.hairline(yy, x, x + SCAN_W, T))
    bx = x + 16
    for label, kind in (("全选", "outline"), ("清除", "outline"), ("导入所选 4 本", "primary")):
        part, bw = k.button(T, bx, yy + 12, label, kind)
        e += part
        bx += bw + 8
    e.append(note(f"平板上这三块是居中浮层（shelf/view/mod.rs:1374-1398 走 popover 那一支），"
                  f"卡 {SCAN_W:.0f}×{SCAN_H:.0f} 定高、整张往上抬半张（overlay/mod.rs:103-125）"))
    return e


# ── 欢迎页 ────────────────────────────────────────────────────────────────────

def welcome_screen(T, cv):
    """首次启动：没有侧栏，内容列夹在 420、整列居中。"""
    e = canvas_bg(T)
    col_w = min(k.W - 48.0, 420.0)
    x = (k.W - col_w) / 2
    stack = 64 + 16 + 28 + 16 + (k.BUTTON_H * 2 + 8)
    y = (k.H + k.STATUS - stack) / 2
    e.append(k.app_icon(k.W / 2 - 32, y, 64))
    y += 64 + 16
    e.append(k.text(k.W / 2, y + 20, "欢迎使用 Folio", 20, T["fg"], 600, anchor="middle"))
    y += 28 + 16
    part, _ = k.button(T, x, y, "选择书库文件夹…", "primary", "folder-open", w=col_w)
    e += part
    part, _ = k.button(T, x, y + k.BUTTON_H + 8, "使用默认位置", "ghost", w=col_w)
    e += part
    e = k.status_bar(T) + e
    e.append(note(f"内容列 {col_w:.0f}（max_w 420，welcome/mod.rs:85）：两档都夹死，左右各 "
                  f"{(k.W - col_w) / 2:.0f}；GUTTER 24 在这两档不构成约束"))
    return e


# ── 出图 ──────────────────────────────────────────────────────────────────────

SCREENS = [
    ("01-shelf-portrait", PORTRAIT, LIGHT,
     lambda T, cv: shelf_screen(T, cv, True),
     "书架·竖屏（Medium）：rail 48、四列一排，顶行是定宽 256 的搜索框加一颗三点钮"),
    ("02-shelf-landscape", LANDSCAPE, LIGHT,
     lambda T, cv: shelf_screen(T, cv, False),
     "书架·横屏（Expanded）：sidebar 260、五列一排"),
    ("03-settings-compact-in-wide", PORTRAIT, LIGHT,
     lambda T, cv: settings_compact_screen(T, cv, True),
     "设置·Android 平板的现状：两栏的门是 host 不是档位，所以这里仍是手机那份钻入式列表"),
    ("04-settings-two-pane-portrait", PORTRAIT, LIGHT,
     lambda T, cv: settings_two_pane_screen(T, cv, True, "外观", "外观", [
         ("浅深", [("浅深", "跟随系统 / 钉死浅 / 钉死深",
                    lambda T, r, c: field_dropdown(T, r, c, "跟随系统"))]),
         ("主题", [("浅色主题", None, lambda T, r, c: field_dropdown(T, r, c, "Default Light")),
                  ("深色主题", None, lambda T, r, c: field_dropdown(T, r, c, "Default Dark"))]),
         ("自定义主题", [("主题目录", "下次启动时读这个目录里的 JSON",
                        lambda T, r, c: field_path_button(T, r, c, "~/.config/folio/themes"))]),
     ]),
     "设置·两栏（桌面宿主拉到 800 宽）：侧栏 170 + 分隔条 9 + 正文 573，正文 >480 走横排"),
    ("05-settings-two-pane-landscape", LANDSCAPE, LIGHT,
     lambda T, cv: settings_two_pane_screen(T, cv, False, "版面", "版面", [
         ("版心", [("最大行长", "0 表示随窗口铺满",
                    lambda T, r, c: field_number(T, r, c, "0")),
                  ("左右页边距", None, lambda T, r, c: field_number(T, r, c, "24")),
                  ("两端对齐", None, lambda T, r, c: field_switch(T, r, c, True))]),
     ]),
     "设置·两栏横屏：选中「版面」这一页 —— 它只有一组，侧栏因此不列子项"),
    ("06-stats-portrait", PORTRAIT, LIGHT,
     lambda T, cv: stats_screen(T, cv, True),
     "统计·竖屏：cell 10、列不夹，53 列铺满可用宽，不横向滚"),
    ("07-stats-landscape", LANDSCAPE, LIGHT,
     lambda T, cv: stats_screen(T, cv, False),
     "统计·横屏：内容列夹在 720 居中，cell 缩到 9"),
    ("08-reading-clean", PORTRAIT, LIGHT,
     lambda T, cv: reading_clean_screen(T, cv),
     "阅读·竖屏干净态：阅读层盖住侧栏、按整窗宽算版心，默认就长出双页（栏 364）"),
    ("09-reading-chrome", LANDSCAPE, LIGHT,
     lambda T, cv: reading_chrome_screen(T, cv),
     "阅读·横屏浮层态：顶栏右侧多出目录开关，底栏满幅卡片四格入口，栏宽 604"),
    ("10-toc-drawer", PORTRAIT, LIGHT,
     lambda T, cv: toc_drawer_screen(T, cv),
     "目录·平板：左抽屉 320，正文一寸不让宽"),
    ("11-scan-popover", PORTRAIT, LIGHT,
     lambda T, cv: scan_popover_screen(T, cv, True),
     "扫描·平板：居中浮层 460×460 定高，替掉手机那一档的底部弹层"),
    ("12-welcome-landscape", LANDSCAPE, LIGHT,
     lambda T, cv: welcome_screen(T, cv),
     "首次启动·横屏：没有侧栏，内容列夹在 420 居中 —— 两档只差居中偏移"),
    ("13-shelf-landscape-dark", LANDSCAPE, DARK,
     lambda T, cv: shelf_screen(T, cv, False),
     "书架·横屏的深色对照"),
]

if __name__ == "__main__":
    OUT.mkdir(parents=True, exist_ok=True)
    for name, canvas, T, builder, desc in SCREENS:
        cv = use(canvas)
        (OUT / f"{name}.svg").write_text(
            k.svg(name, builder(T, cv), T["label"], desc, T, area="平板"), encoding="utf-8")
        k.render_png(OUT, f"{name}.svg", canvas[2], canvas[3])
        print(f"ok  {name}.svg  {canvas[0]}×{canvas[1]}")
    print("done:", len(SCREENS), "screens")
