#!/usr/bin/env python3
"""Folio「阅读页」（compact 档）SVG 设计稿生成器。

九屏：干净态正文、浮层态、阅读设置面板、目录弹层、划选浮条、笔记便签、图片查看器、
分享卡、深色对照。尺全部来自实现：

| 这一格 | 数 | 出处 |
| --- | --- | --- |
| 干净态顶带 / 底带 | 各 28 | `reader/model/chrome.rs:14,16` `HEADER_HEIGHT` / 底带同高 |
| 带内文字 | 12 次要前景色，带顶再加 2 | `reader/view/bands.rs:29,72-104` `HEADER_TOP_PAD` |
| 带内左右内缩 | 16 | 同上 `px_4` |
| 版心宽 | 336 | 384 − 2×`margin_x`（`folio-core/src/infra/config.rs:550-566` 默认 24），`max_measure=0` 即铺满 |
| 字号 / 行盒 | 17 / 29.75 | 同上默认 `font_size` × `line_height` 1.75；`layout/font.rs:178` |
| 一行几个字 | 19 | 336 / 17，两端对齐 |
| 首行缩进 | 34 | `indent_em` 2.0 × 17（`chapter_layout.rs:1072`），只作用首行 |
| 段距 | 12.75 | `paragraph_gap_em` 0.75 × 17，相邻段取 max 合并（`chapter_layout.rs:728-731`） |
| 浮层顶栏 | 44、`PAD_X` 12 | `chrome.rs:12` `TOP_BAR_HEIGHT` + `components/top_bar.rs:36,43` |
| 浮层底栏卡片 | `px_4` `pt_3`=12 `pb`=8、缝 12、只圆上两角 8 | `reader/view/regions.rs:83-105` |
| 目录弹层 | 0.85 档高、头部行 44、行高 46（默认宽松）、当前章左标记条 2.9 距左 2.1 | `toc_panel.rs:83,742-795`、`config.rs:258-262`、`toc_panel.rs:62-64,68` |
| 目录文字 | 左界 31、每级缩进 11.4、标题 2 行、14 号 | `toc_panel.rs:70,75,1243,1318` |
| 划选卡 | 宽 = 顶端那一行（96 + 12 + 152 = 260）与动作条（40×6 + 8 = 248）的**较宽者**；高 94 = 32 + 6 + 48 + 8（尾巴） | `reader/components/selection.rs:981-1052` 那组常量、`card_width()` / `card_height()` |
| 划选胶囊 | 画法三格与颜色五枚**同宽一格 28**、同高 32；画法那三条没有底，颜色那五枚同住一条圆角胶囊 | 同上：`STYLE_CELL == SWATCH_CELL` 那句注释 |
| 痕迹三档 | 高亮＝整行盒压 0.35 / 实线＝盒底 2 / 波浪＝高 3.2 波长 7 振幅 1.6 描边 1.6 | `view/body.rs:2476-2530`、`view/paint.rs:242-250` |
| 笔记便签 | 顶 56、`max_w` 420、内缩 16、圆角 16、缝 12、输入区 128、引文区 ≤58 两行左边框 2 | `note_editor.rs:74-78,88-160` |
| 图片查看器 | 纯黑全屏、控制格 52×44、图标 20、标签 10、计数 14 | `image_viewer.rs:204-219,811-887` |
| 分享卡 | 720×1080 @2.0、内缩 56、引文 34/50、分隔线 1、书名 22/30、位置 18/26、上限 220 字 | `share_card.rs:31-40,119-156` |

**两处稿子与真机必然不同，先说清**：

1. 正文真机排的是配置里那个字体家族（默认 `LXGW WenKai`，一款楷体），稿子统一用
   `Noto Sans CJK SC`。字形不同，**度量按 17 / 29.75 走**，所以行位置是对的。
2. 五档标注色取主题的 `red / magenta / cyan / green / yellow`（`annotations.rs:652-658`），
   顺序对上 `ANNOTATION_COLORS` 的 `ruby / rose / sky / lime / amber`；稿子里写的是那五档在
   shadcn 调色板上的 500 阶。换主题时这一排跟着动，默认那一种（`amber`）不变。

**分享卡不进屏幕**：它是离屏渲成 PNG 再交给系统分享面板的（`platform/mod.rs:370` 那格
`RENDERS_SHARE_CARDS`），所以第八屏画的是那张产物本身，按 336×504 等比缩下来看版式。
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from _kit import *  # noqa: E402,F403

OUT = Path(__file__).parent

BAND = 28.0
BAND_PAD_TOP = 2.0
BAND_X = 16.0
BODY_X = 24.0                    # margin_x
BODY_W = W - 2 * BODY_X          # 336
FS = 17.0
LH = FS * 1.75                   # 29.75
INDENT = FS * 2.0                # 34
PGAP = FS * 0.75                 # 12.75
PER_LINE = int(BODY_W / FS)      # 19
BODY_TOP = STATUS + BAND
BODY_BOTTOM = H - BAND

# 五档标注色与默认档在 [`../_kit.py`](../_kit.py)（笔记那一屏的左缘色条问的是同一件事）。


def body_line(T, x, y, s, justify):
    """一行正文。`justify` 那几行用 `textLength` 硬拉到版心宽 —— 两端对齐就是这么个东西。"""
    if justify and len(s) > 1:
        return (f'<text x="{x}" y="{y}" font-family="{FONT}" font-size="{FS}" '
                f'fill="{T["fg"]}" textLength="{BODY_W}" lengthAdjust="spacing">{s}</text>')
    return text(x, y, s, FS, T["fg"])


def para_lines(prose):
    """一段折成 `(行, 是不是首行)`：首行少排一个缩进，其余按整幅版心排。"""
    out, cur, first = [], "", True
    for ch in prose:
        limit = BODY_W - (INDENT if first else 0)
        if cur and tw(cur + ch, FS) > limit:
            out.append((cur, first))
            cur, first = ch, False
        else:
            cur += ch
    if cur:
        out.append((cur, first))
    return out


def draw_body(T, e, y, blocks, stop):
    """正文：h1 + 若干段，画到 `stop` 为止。"""
    for kind, prose in blocks:
        if y > stop:
            break
        if kind == "h1":
            y += 34 * 1.75
            e.append(text(BODY_X, y, prose, 34, T["fg"], 700))
            y += 0.67 * 34
            continue
        lines = para_lines(prose)
        for i, (line, first) in enumerate(lines):
            if y > stop:
                break
            x = BODY_X + (INDENT if first else 0)
            y += LH
            e.append(body_line(T, x, y, line, i < len(lines) - 1))
        y += PGAP
    return y


CHAPTER = [
    ("h1", "湖上"),
    ("p", "清晨我划到湖心去，雾还没散。水面没有一点风，船头推开的地方起一圈圈细纹，"
          "细纹走到雾里就没了。我停下来听，四周只有水拍船底那一下一下。"),
    ("p", "松林在雾里只剩一条深色的边。岸上的颜色比水浅，比天深，说不清是哪一种绿。"
          "我认得这一条边——从远处看它一直在，走近了才发现它是由一棵一棵树站出来的。"),
    ("p", "划桨的手不需要想。想的是另一件事：这一年读过的书，有多少是读第一遍时"
          "真在读，有多少只是眼睛从字上走了一遍。眼睛走得很快，快得什么也没留下。"),
    ("p", "雾散的时候太阳已经高了。湖面重新变成一件东西，而不是一片光。我把船靠到"
          "那块旧石头上，石头上有水痕，一道一道，是这些年水位留下的。"),
    ("p", "回去的路上经过一片浅滩，水草在脚边那么深的地方立着。我停下来看了一会儿。"
          "它们不急着往哪儿去，也不因为有人看就长得更快些。"),
]


def bands(T, e, position="第 47 / 218 页", progress="22%"):
    """干净态那两条带：上面念章名，下面左边位置、右边进度。"""
    e.append(f'<rect x="0" y="{STATUS}" width="{W}" height="{BAND}" fill="{T["bg"]}"/>')
    e.append(text(BAND_X, STATUS + BAND_PAD_TOP + 17, "湖上", 12, T["muted"]))
    e.append(f'<rect x="0" y="{BODY_BOTTOM}" width="{W}" height="{BAND}" fill="{T["bg"]}"/>')
    e.append(text(BAND_X, BODY_BOTTOM + 18, position, 12, T["muted"]))
    e.append(text(W - BAND_X, BODY_BOTTOM + 18, progress, 12, T["muted"], anchor="end"))
    return e


def clean_screen(T, name, note):
    e = status_bar(T)
    y = draw_body(T, e, BODY_TOP, CHAPTER, BODY_BOTTOM - LH)
    bands(T, e)
    return svg(name, e, T["label"], note, T, area="阅读")


def chrome_screen(T):
    """浮层态：顶栏 44 压在带上头，底栏一张只圆上两角的卡。"""
    e = status_bar(T)
    draw_body(T, e, BODY_TOP, CHAPTER, BODY_BOTTOM - LH)
    # 顶栏：返回 + 书名，章名做副标题（compact 档右侧不摆目录钮，那是 wide 才长出来的）。
    e.append(f'<rect x="0" y="{STATUS}" width="{W}" height="{TOPBAR}" fill="{T["bg"]}"/>')
    cy = STATUS + TOPBAR / 2
    e.append(icon("arrow-left", 12 + 4, cy - 10, 20, T["fg"]))
    e.append(text(12 + 34, cy - 1, "瓦尔登湖", 14, T["fg"], 600))
    e.append(text(12 + 34, cy + 13, "湖上", 14, T["muted"]))
    # 底栏卡片：状态行 + 进度行 + 入口行。
    card_h = 12 + 17 + 12 + 24 + 12 + 40 + 8
    top = H - card_h
    e.append(f'<rect x="0" y="{top}" width="{W}" height="{card_h}" rx="{CARD_R}" '
             f'fill="{T["popover"]}" stroke="{T["border"]}" stroke-width="1"/>')
    e.append(f'<rect x="0" y="{top + CARD_R}" width="{W}" height="{card_h - CARD_R}" '
             f'fill="{T["popover"]}"/>')
    y = top + 12
    e.append(text(BAND_X, y + 13, "进度 22%", 14, T["muted"]))
    y += 17 + 12
    e.append(icon("chevron-left", BAND_X, y + 2, 20, T["fg"]))
    e.append(icon("chevron-right", W - BAND_X - 20, y + 2, 20, T["fg"]))
    e.append(f'<rect x="{BAND_X + 28}" y="{y + 10}" width="{W - 2 * BAND_X - 56}" height="4" '
             f'rx="2" fill="{T["muted_bg"]}"/>')
    knob = BAND_X + 28 + (W - 2 * BAND_X - 56) * 0.22
    e.append(f'<circle cx="{knob:.1f}" cy="{y + 12}" r="9" fill="' + T["primary"] + '"/>')
    y += 24 + 12
    entries = [("list-tree", "目录"), ("settings-2", "设置"), ("columns-3", "版式"), ("moon", "夜间")]
    cell = W / len(entries)
    for i, (ic, label) in enumerate(entries):
        cx = cell * (i + 0.5)
        e.append(icon(ic, cx - 9, y + 2, 18, T["fg"]))
        e.append(text(cx, y + 32, label, 12, T["fg"], anchor="middle"))
    return svg("02-reading-chrome", e, T["label"],
               "浮层态：顶栏 44（返回 + 书名 + 章名），底栏一张只圆上两角的卡：状态行 + 进度行 + 四格入口", T, area="阅读")


def panel_screen(T):
    """浮层 + 阅读设置面板：字号滑条、亮度、翻页动画向上开、更多设置。"""
    e = status_bar(T)
    draw_body(T, e, BODY_TOP, CHAPTER, BODY_BOTTOM - LH)
    card_h = 12 + 17 + 12 + 32 + 12 + 32 + 12 + 40 + 8
    top = H - card_h
    e.append(f'<rect x="0" y="{top}" width="{W}" height="{card_h}" rx="{CARD_R}" '
             f'fill="{T["popover"]}" stroke="{T["border"]}" stroke-width="1"/>')
    e.append(f'<rect x="0" y="{top + CARD_R}" width="{W}" height="{card_h - CARD_R}" '
             f'fill="{T["popover"]}"/>')
    y = top + 12
    # 字号滑条：12..40，当前 17。
    e.append(text(BAND_X, y + 13, "字号", 14, T["muted"]))
    e.append(text(W - BAND_X, y + 13, "17", 14, T["fg"], anchor="end"))
    track_y = y + 26
    e.append(f'<rect x="{BAND_X}" y="{track_y}" width="{W - 2 * BAND_X}" height="4" rx="2" '
             f'fill="{T["muted_bg"]}"/>')
    frac = (17 - 12) / (40 - 12)
    e.append(f'<rect x="{BAND_X}" y="{track_y}" width="{(W - 2 * BAND_X) * frac:.1f}" '
             f'height="4" rx="2" fill="{T["primary"]}"/>')
    e.append(f'<circle cx="{BAND_X + (W - 2 * BAND_X) * frac:.1f}" cy="{track_y + 2}" r="9" '
             f'fill="' + T["primary"] + '"/>')
    y = track_y + 6 + 12
    e.append(text(BAND_X, y + 13, "亮度", 14, T["muted"]))
    e.append(text(W - BAND_X, y + 13, "80%", 14, T["fg"], anchor="end"))
    e.append(f'<rect x="{BAND_X}" y="{y + 26}" width="{W - 2 * BAND_X}" height="4" rx="2" '
             f'fill="{T["muted_bg"]}"/>')
    e.append(f'<rect x="{BAND_X}" y="{y + 26}" width="{(W - 2 * BAND_X) * 0.8:.1f}" height="4" '
             f'rx="2" fill="{T["primary"]}"/>')
    e.append(f'<circle cx="{BAND_X + (W - 2 * BAND_X) * 0.8:.1f}" cy="{y + 28}" r="9" '
             f'fill="' + T["primary"] + '"/>')
    y += 26 + 6 + 12
    entries = [("list-tree", "目录"), ("settings-2", "设置"), ("columns-3", "版式"), ("moon", "夜间")]
    cell = W / len(entries)
    for i, (ic, label) in enumerate(entries):
        cx = cell * (i + 0.5)
        e.append(icon(ic, cx - 9, y + 2, 18, T["fg"]))
        e.append(text(cx, y + 32, label, 12, T["fg"], anchor="middle"))
    # 翻页动画那格向上开的浮层。
    menu_h = 6 + 6 * 30 + 6
    my = y - menu_h - 6
    e.append(f'<rect x="{cell * 1.5 - 60}" y="{my}" width="120" height="{menu_h}" '
             f'rx="{RADIUS}" fill="{T["popover"]}" stroke="{T["border"]}" stroke-width="1"/>')
    for i, label in enumerate(["左右平移", "上下平移", "左右覆盖", "仿真翻页", "上下滚动", "不走动画"]):
        e.append(text(cell * 1.5 - 60 + 12, my + 6 + i * 30 + 20, label, 14,
                      T["fg"] if label == "不走动画" else T["muted"]))
        if label == "不走动画":
            e.append(icon("check", cell * 1.5 + 34, my + 6 + i * 30 + 9, 14, T["primary"]))
    return svg("03-reading-panel", e, T["label"],
               "阅读设置面板：字号 12..40 与亮度两条滑条、四格入口、翻页动画那格向上开的浮层", T, area="阅读")


def toc_screen(T):
    """目录：底部弹层 0.85 档。书头 + 头部行 + 一级/二级条目，当前章整行底色加左标记条。"""
    e = status_bar(T)
    draw_body(T, e, BODY_TOP, CHAPTER, BODY_BOTTOM - LH)
    bands(T, e)
    e.append(scrim(T))
    part, top = sheet(T, 0.85)
    e += part
    y = top + HANDLE_BAND
    # 书头：封面 48×72 + 三行。
    e.append(f'<rect x="15" y="{y + 12}" width="48" height="72" rx="{RADIUS}" '
             f'fill="{T["muted_bg"]}"/>')
    e.append(text(15 + 48 + 12, y + 12 + 16, "瓦尔登湖", 16, T["fg"]))
    e.append(text(15 + 48 + 12, y + 12 + 38, "亨利·戴维·梭罗", 14, T["muted"]))
    e.append(text(15 + 48 + 12, y + 12 + 60, "读到 22% · EPUB · 1.8 MB", 14, T["muted"]))
    y += 12 + 72 + 8
    # 头部行 44：已读 N / M 章 + 目录·全文两格 + 放大镜 + 三点。
    e.append(f'<rect x="0" y="{y}" width="{W}" height="0.5" fill="{T["border"]}"/>')
    cy = y + 22
    e.append(text(15, cy + 5, "已读 6 / 18 章", 14, T["muted"]))
    # 右端一排：放大镜、三点，再往左是 ghost + small 那两格（高 24、内缩 8），
    # 选中那一格吃 `accent` 底色。
    e.append(icon("search", W - 15 - 36, cy - 8, 16, T["fg"]))
    e.append(icon("ellipsis", W - 15 - 16, cy - 8, 16, T["fg"]))
    bx = W - 15 - 36 - 16 - 8
    for label, on in (("全文", False), ("目录", True)):
        bw = 8 + tw(label, 14) + 8
        bx -= bw
        if on:
            e.append(f'<rect x="{bx:.1f}" y="{cy - 12}" width="{bw:.1f}" height="24" '
                     f'rx="{RADIUS}" fill="{T["muted_bg"]}"/>')
        e.append(text(bx + 8, cy + 5, label, 14, T["fg"]))
        bx -= 4
    y += 44
    ROW_H = 46.0        # toc_density.row_height()，默认宽松
    ITEMS = [
        (0, "经济状", False, False),
        (0, "我生活的地方；我为何生活", True, True),
        (1, "读", False, False),
        (1, "声音", False, False),
        (0, "湖", False, True),
        (1, "湖的岸线", False, False),
        (1, "瓦尔登湖的宝藏", False, False),
        (0, "贝克斯菲尔德的来访者", False, False),
        (0, "种豆", False, False),
        (0, "镇", False, False),
        (0, "湖的访客", False, False),
        (0, "更高的法则", False, False),
    ]
    for level, title, current, read in ITEMS:
        if y + ROW_H > H - 24:
            break
        x = 31 + level * 11.4
        if current:
            e.append(f'<rect x="0" y="{y}" width="{W}" height="{ROW_H}" fill="{T["muted_bg"]}"/>')
            e.append(f'<rect x="2.1" y="{y + 10}" width="2.9" height="{ROW_H - 20}" '
                     f'rx="1.5" fill="{T["primary"]}"/>')
        e.append(text(x, y + ROW_H / 2 + 5, title, 14,
                      T["muted"] if read and not current else T["fg"], 600 if current else 400))
        if level == 0 and not current:
            e.append(icon("chevron-right", W - 15.3 - 14, y + ROW_H / 2 - 7, 14, T["muted"]))
        y += ROW_H
    return svg("04-toc-sheet", e, T["label"],
               "目录弹层：0.85 档高，书头 + 头部行 + 条目（当前章整行底色加左标记条，已读降成次要色）", T, area="阅读")


def selection_screen(T):
    """划选浮条：卡 260×94 压在选区上方。

    这一层**不是模态**：没有遮罩，正文照常在那儿，浮条只是锚在选区上（`selection.rs:1069-1085`
    那条「选区上方优先、放不下翻面」）。所以稿子里也不给底栏 —— 阅读层是整屏的。
    """
    e = status_bar(T)
    bands(T, e)
    y = BODY_TOP
    sel_y, sel_lines = None, []
    for n, (kind, prose) in enumerate(CHAPTER):
        if kind == "h1":
            y += 34 * 1.75
            e.append(text(BODY_X, y, prose, 34, T["fg"], 700))
            y += 0.67 * 34
            continue
        lines = para_lines(prose)
        if n == 3:                       # 选这一段的前三行
            sel_y = y + LH
        for i, (line, first) in enumerate(lines):
            if y > BODY_BOTTOM - LH:
                break
            x = BODY_X + (INDENT if first else 0)
            y += LH
            if sel_y and n == 3 and i < 3:
                sel_lines.append((x, y, line, first, i == 2))
            else:
                e.append(body_line(T, x, y, line, i < len(lines) - 1))
        y += PGAP
    # 选区：高亮压在字**下面**（body.rs:2530 那一条 —— 可重排正文里它是底色带，不是覆盖层）。
    for x, yy, line, first, _last in sel_lines:
        w = BODY_W - (INDENT if first else 0)
        e.append(f'<rect x="{x:.1f}" y="{yy - 18}" width="{w:.1f}" height="{FS + 6}" '
                 f'fill="{DEFAULT_INK}" fill-opacity="0.35"/>')
    for x, yy, line, first, last in sel_lines:
        e.append(body_line(T, x, yy, line, not last))
    # 浮条：压在选区上方，离 6。数全部对齐 `selection.rs:981-1052` 那一组常量，
    # 不挑顺眼的那一档 —— 整块的宽 = 顶端那一行与动作条的**较宽者**（`card_width` 的原话）。
    #
    # **这一层没有一张白卡包着**：实现里那个容器只有 `absolute` 与 `items_center`
    # （`selection.rs:1226-1240`），三条各自带底、各自投影，底色同为一枚 `theme().muted`
    # （`schema.rs:458-460` 写明 `muted` 就是 `muted.background`，即这把尺里的 `muted_bg`）。
    # 稿子从前画的那张白卡加一圈描边是画出来的东西，屏上没有。
    ACTION_CELL, ACTION_ICON = 40.0, 21.0        # 一格的可点边长 / 图标本身
    BAR_PAD, BAR_GAP, MENU_GAP = 4.0, 6.0, 6.0   # 动作条内缩 / 两行之间的缝 / 菜单与选区的缝
    SWATCH_PAD, SWATCH_CELL, SWATCH_DOT = 6.0, 28.0, 20.0
    STYLE_CELL, STYLE_ICON, MARK_GAP = 28.0, 17.0, 12.0
    SWATCH_CHECK, MENU_TAIL, MENU_TAIL_WIDTH = 13.0, 8.0, 16.0
    N_STYLE, N_COLORS = 3, 5                      # AnnotationStyle::ALL / ANNOTATION_COLORS
    # 这一屏讲的故事是「长按落在一条已有的琥珀划线上面」：选区下面那条痕迹在库里，
    # 于是第二格换做「取消划线」（`Trash`，`selection.rs:1123-1136`），画法那一档与颜色
    # 那一枚都跟着打标记。普通选区（没划过）第二格是 `Highlighter`，两条胶囊不打标记。
    actions = ["copy", "trash", "sticky-note", "book-headphones", "share-2", "x"]
    style_pill = SWATCH_PAD * 2 + STYLE_CELL * N_STYLE            # 96
    swatch_pill = SWATCH_PAD * 2 + SWATCH_CELL * N_COLORS         # 152
    mark_row = style_pill + MARK_GAP + swatch_pill                # 260 —— 今天是最宽那一行
    bar_h = ACTION_CELL + BAR_PAD * 2                             # 48
    bar_w = ACTION_CELL * len(actions) + BAR_PAD * 2              # 248
    swatch_bar = SWATCH_DOT + SWATCH_PAD * 2                      # 32
    card_w = max(bar_w, mark_row)
    card_h = swatch_bar + BAR_GAP + bar_h + MENU_TAIL             # 32+6+48+8 = 94
    cx = (W - card_w) / 2
    cy = sel_lines[0][1] - 18 - MENU_GAP - card_h
    # `floating_shadow()` = 偏移 (0, 2)、模糊 8、黑色 18%（`selection.rs:1466-1469`）。
    # SVG 的 `stdDeviation` 约等于模糊半径的一半，所以是 4。
    e.append('<defs><filter id="float" x="-50%" y="-50%" width="200%" height="200%">'
             '<feDropShadow dx="0" dy="2" stdDeviation="4" flood-color="#000000" '
             'flood-opacity="0.18"/></filter></defs>')
    # 顶端那一行：两条胶囊 `justify_between` 顶到整块的左右两端，同高 32、圆角 16。
    e.append(f'<rect x="{cx:.1f}" y="{cy:.1f}" width="{style_pill:.1f}" height="{swatch_bar:.1f}" '
             f'rx="{swatch_bar / 2:.1f}" fill="{T["muted_bg"]}" filter="url(#float)"/>')
    # 当下那一档画法打亮：`.when(active, |cell| cell.bg(hover))`，`hover` 就是 `border`。
    e.append(f'<rect x="{cx + SWATCH_PAD:.1f}" y="{cy:.1f}" width="{STYLE_CELL:.1f}" '
             f'height="{swatch_bar:.1f}" rx="{swatch_bar / 2:.1f}" fill="{T["border"]}"/>')
    for i, ic in enumerate(("highlighter", "underline", "waves-horizontal")):
        e.append(icon(ic, cx + SWATCH_PAD + i * STYLE_CELL + (STYLE_CELL - STYLE_ICON) / 2,
                      cy + (swatch_bar - STYLE_ICON) / 2, STYLE_ICON, T["fg"]))
    sx = cx + style_pill + MARK_GAP
    e.append(f'<rect x="{sx:.1f}" y="{cy:.1f}" width="{swatch_pill:.1f}" '
             f'height="{swatch_bar:.1f}" rx="{swatch_bar / 2:.1f}" fill="{T["muted_bg"]}" '
             f'filter="url(#float)"/>')
    for i, color in enumerate(INK_COLORS):
        ccx = sx + SWATCH_PAD + i * SWATCH_CELL + SWATCH_CELL / 2
        ccy = cy + swatch_bar / 2
        e.append(f'<circle cx="{ccx:.1f}" cy="{ccy:.1f}" r="{SWATCH_DOT / 2}" fill="{color}"/>')
        if i == 4:    # 那条划线的颜色：勾压在圆点上，取页面底色（`check` 那一句的理由）
            e.append(icon("check", ccx - SWATCH_CHECK / 2, ccy - SWATCH_CHECK / 2,
                          SWATCH_CHECK, T["bg"]))
    # 动作条：居中在那一行下面（`bars` 那一条 `items_center`）。
    ay = cy + swatch_bar + BAR_GAP
    bx0 = cx + (card_w - bar_w) / 2
    e.append(f'<rect x="{bx0:.1f}" y="{ay:.1f}" width="{bar_w:.1f}" height="{bar_h:.1f}" '
             f'rx="14" fill="{T["muted_bg"]}" filter="url(#float)"/>')
    for i, ic in enumerate(actions):
        bx = bx0 + BAR_PAD + i * ACTION_CELL
        e.append(icon(ic, bx + (ACTION_CELL - ACTION_ICON) / 2,
                      ay + BAR_PAD + (ACTION_CELL - ACTION_ICON) / 2, ACTION_ICON, T["fg"]))
    # 尾巴：菜单在选区上方，所以尖朝下，贴在动作条底下、压在整块高度的最后 8 像素里。
    e.append(f'<path d="M {cx + card_w / 2 - MENU_TAIL_WIDTH / 2:.1f} {ay + bar_h:.1f} '
             f'h{MENU_TAIL_WIDTH:.0f} l-{MENU_TAIL_WIDTH / 2:.0f} {MENU_TAIL:.0f} z" '
             f'fill="{T["muted_bg"]}"/>')
    return svg("05-selection", e, T["label"],
               "划选浮条：三条各自带底各自投影的胶囊（没有白卡），上排画法三格与颜色五枚、"
               "下排六格动作条（新加「从这句念」）；这一屏画的是落在已有划线上的那一档，"
               "所以第二格是取消划线、画法与颜色各打一个标记。不是模态，没有遮罩也没有底栏",
               T, area="阅读")


def note_screen(T):
    """笔记便签：整屏遮罩 + 居中偏上那张卡，不是底部弹起。"""
    e = status_bar(T)
    draw_body(T, e, BODY_TOP, CHAPTER, BODY_BOTTOM - LH)
    bands(T, e)
    e.append(scrim(T))
    card_w = 336.0
    x = (W - card_w) / 2
    y = 56.0
    card_h = 16 + 24 + 12 + 58 + 12 + 128 + 12 + 32 + 16
    e.append(f'<rect x="{x}" y="{y}" width="{card_w}" height="{card_h}" rx="16" '
             f'fill="{T["popover"]}" stroke="{T["border"]}" stroke-width="1"/>')
    yy = y + 16
    e.append(icon("sticky-note", x + 16, yy + 3, 18, T["fg"]))
    e.append(text(x + 16 + 26, yy + 17, "笔记", 16, T["fg"], 600))
    e.append(icon("x", x + card_w - 16 - 18, yy + 3, 18, T["fg"]))
    yy += 24 + 12
    e.append(f'<rect x="{x + 16}" y="{yy}" width="2" height="58" fill="{T["border"]}"/>')
    e.append(text(x + 16 + 12, yy + 17, "我停下来听，四周只有水拍船底那一下", 14, T["muted"]))
    e.append(text(x + 16 + 12, yy + 39, "一下。", 14, T["muted"]))
    e.append(f'<rect x="{x + 16}" y="{yy + 50}" width="{card_w - 32}" height="128" '
             f'rx="{RADIUS}" fill="none" stroke="{T["border"]}" stroke-width="1"/>')
    e.append(text(x + 16 + 10, yy + 50 + 24, "写点什么…", 14, T["muted"]))
    by = yy + 50 + 128 + 12
    part, _ = button(T, x + card_w - 16 - 68, by, "保存", "primary", w=68)
    e += part
    part, _ = button(T, x + card_w - 16 - 68 - 8 - 68, by, "取消", "ghost", w=68)
    e += part
    return svg("06-note-editor", e, T["label"],
               "笔记便签：整屏遮罩加一张居中偏上的卡；引文最多两行，下面是 128 高的输入区", T, area="阅读")


def image_screen(T):
    """图片查看器：纯黑全屏，顶行三组控制。"""
    e = [f'<rect width="{W}" height="{H}" fill="#000000"/>']
    y = STATUS + 6
    for i, (ic, label) in enumerate((("share-2", "分享"), ("download", "存图"))):
        bx = 12 + i * 52
        e.append(icon(ic, bx + 16, y + 6, 20, "#fafafa"))
        e.append(text(bx + 26, y + 38, label, 10, "#a3a3a3", anchor="middle"))
    e.append(text(W / 2, y + 27, "3 / 12", 14, "#fafafa", anchor="middle"))
    for i, (ic, label) in enumerate((("map-pin", "定位"), ("x", "关闭"))):
        bx = W - 12 - (2 - i) * 52
        e.append(icon(ic, bx + 16, y + 6, 20, "#fafafa"))
        e.append(text(bx + 26, y + 38, label, 10, "#a3a3a3", anchor="middle"))
    # 图本身：等比铺满可用宽。
    iw = W
    ih = iw * 0.68
    iy = (H - ih) / 2
    e.append(f'<rect x="0" y="{iy:.1f}" width="{iw}" height="{ih:.1f}" fill="#1c1c1c"/>')
    e.append(icon("image", W / 2 - 32, iy + ih / 2 - 32, 64, "#3f3f3f"))
    return svg("07-image-viewer", e, T["label"],
               "图片查看器：纯黑全屏，顶行左两颗、中间计数、右两颗，图按手势缩放", T, area="阅读")


def share_card_screen(T):
    """分享卡：离屏渲成 PNG 交给系统面板的那张产物，按 336×504 等比看版式。"""
    e = status_bar(T)
    draw_body(T, e, BODY_TOP, CHAPTER, BODY_BOTTOM - LH)
    e.append(scrim(T))
    cw, ch = 336.0, 504.0
    x, y = (W - cw) / 2, (H - ch) / 2
    scale = cw / 720.0
    pad = 56 * scale
    e.append(f'<rect x="{x}" y="{y}" width="{cw}" height="{ch}" rx="{CARD_R}" '
             f'fill="{T["popover"]}"/>')
    yy = y + pad + 16
    for line in wrap_cjk("清晨我划到湖心去，水面没有一点风，船头推开的地方起一圈圈细纹，"
                         "细纹走到雾里就没了。", 16, cw - 2 * pad, 8):
        e.append(text(x + pad, yy, line, 16, T["fg"]))
        yy += 24
    yy += 10
    e.append(f'<rect x="{x + pad}" y="{yy}" width="{cw - 2 * pad}" height="1" '
             f'fill="{T["border"]}"/>')
    yy += 22
    e.append(text(x + pad, yy, "瓦尔登湖", 14, T["muted"]))
    yy += 20
    e.append(text(x + pad, yy, "第 47 / 218 页 · 22%", 12, T["muted"]))
    return svg("08-share-card", e, T["label"],
               "分享卡：720×1080 @2.0 离屏渲的那张，这里等比缩到 336×504 看版式", T, area="阅读")


if __name__ == "__main__":
    OUT.mkdir(parents=True, exist_ok=True)
    out = {
        "01-reading-clean.svg": clean_screen(
            LIGHT, "01-reading-clean",
            "干净态正文页：上下各一条 28 的带，中间是版心 336、行盒 29.75 的正文"),
        "02-reading-chrome.svg": chrome_screen(LIGHT),
        "03-reading-panel.svg": panel_screen(LIGHT),
        "04-toc-sheet.svg": toc_screen(LIGHT),
        "05-selection.svg": selection_screen(LIGHT),
        "06-note-editor.svg": note_screen(LIGHT),
        "07-image-viewer.svg": image_screen(LIGHT),
        "08-share-card.svg": share_card_screen(LIGHT),
        "09-reading-dark.svg": clean_screen(
            DARK, "09-reading-dark", "干净态正文页的深色对照"),
    }
    for name, body in out.items():
        (OUT / name).write_text(body, encoding="utf-8")
    print("done:", len(out), "screens")
    render_pngs(OUT)
