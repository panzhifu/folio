#!/usr/bin/env python3
"""Folio「书架」目的地（compact 档）SVG 设计稿生成器。

八屏：主架面、搜索有结果、组那一页、书信息、扫描 sheet、导入浮层、书卡菜单、深色对照。
（「还没有内容」那三屏在 [`../welcome/`](../welcome/generate.py) —— 同一颗 `empty_state`，
不在这儿画第二遍。）

尺全部来自实现，逐条对得上：

| 这一格 | 数 | 出处 |
| --- | --- | --- |
| 一行几本 | 3 | `shelf/view/grid.rs:10` `COLUMNS=3`，`:74-77` compact 档恒返回它（钉在 `view/mod.rs:1442`） |
| 列间距 | 8 | `grid.rs:13` `GAP`（`.gap_2()`） |
| 左右留白 | 12 | `grid.rs:23` `GUTTER`（统计那一屏共用同一个数） |
| 封面宽高比 | 2:3 | `shelf/components/book_card.rs:55` `COVER_RATIO` |
| 一格宽 / 封面高 | 114.67 / 172 | 384 实算：`(384−24−8×2)/3`，再 `/ (2/3)`（`grid.rs:53-61`） |
| 每排上下留白 | 4 | `view/mod.rs:1264-1271` `py_1`；架面与底栏之间**没有**额外留白 |
| 封面格圆角 | 6 | `book_card.rs:297` `rounded_md` = `theme().radius` |
| 无封面那一格 | 格内画书名、居中、次要前景色 | `book_card.rs:347-359` |
| 书名 / 作者 | 16 单行省略 / 12 次要色，缝 4 | `book_card.rs:361-378`、`:315` `gap_1` |
| 组卡 | 与书卡同格同高，里面 2×2 小图 | `book_card.rs:399-501` |
| 组卡的小图 | 内缩 6、缝 4、圆角 2、空槽 `muted` | `book_card.rs:149-166` |
| 顶行 | 搜索框 32 高 + 右侧一颗 24 的三点钮，`px(12) py_2` | `shelf/view/toolbar.rs:47-59` |
| 框里有词时 | 右端一枚 20 宽的清除钮（`Button.xsmall()` 包 12 的 `Close`），钮右缘落在框内缩 10 | `input/clear_button.rs:9-15`、`input.rs:531-535,614` |
| 组那一页的顶栏 | 高 44、`PAD_X` 12、标题 14 SEMIBOLD、副标题「N 本书」14 次要色 | `components/top_bar.rs:36,41,43,282-290` + `view/mod.rs:1109-1120` |
| 书信息 | 封面 96×144、正文 `px(12)`、块缝 12、事实行标签列 80 | `shelf/view/book_info.rs:53,56,62,299-446` |
| 弹层 | 扫描 0.85、普通 0.6 档高；把手带 36、胶囊 32×4 | `shelf/overlay/mod.rs:51-52`、`components/bottom_sheet.rs:44-47` |
| 扫描行 | 行 36、分组头 28、内容 `px(12)` | `shelf/view/scan.rs:34,36`、`sheets.rs:130-136` |
| 菜单 | 浮层，`Anchor::TopRight`，最小宽 140 | `toolbar.rs:91`、`book_menu.rs:40` |

**封面位是占位，不是配色**：真机上有封面的书摆的是解码出来的那张图。稿子里那些方块一律
画成 neutral-300 压薄的占位灰，只表示「这里是一张图」—— 别把它读成界面颜色。没封面的那一格
才是实现本来的样子（格内书名、居中、次要前景色）。

**量出来的一条不符**：`shelf.empty_hint` 说「用右边的「导入」…」，而 compact 的顶栏上只有
一根搜索框和一颗三点钮，「导入」藏在菜单里。那句在桌面成立、在手机把读者往一个不存在的位置指。
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from _kit import *  # noqa: E402,F403

OUT = Path(__file__).parent

COLUMNS = 3          # grid.rs:10
GAP = 8.0            # grid.rs:13
GUTTER = 12.0        # grid.rs:23
COVER_RATIO = 2 / 3  # book_card.rs:55
CARD = (W - 2 * GUTTER - GAP * (COLUMNS - 1)) / COLUMNS   # 114.67
COVER_H = CARD / COVER_RATIO                              # 172.0
TITLE_LINE, AUTHOR_LINE = 20.0, 15.0
CELL_H = COVER_H + 4 + TITLE_LINE + 4 + AUTHOR_LINE       # 215
TOOLBAR_H = 8 + BUTTON_H + 8
GRID_TOP = STATUS + TOOLBAR_H + 4

# 封面位的占位灰：neutral-300 压薄。刻意不用主题那五档图表色 —— 那样会被读成
# 「封面是蓝色的」，而这一格要说的只是「这里是一张解码出来的图」。
ART = "#d4d4d4"
ART_A = "0.35"

TILE_PAD, TILE_GAP = 6.0, 4.0
TILE_W = (CARD - 2 * TILE_PAD - TILE_GAP) / 2
TILE_H = (COVER_H - 2 * TILE_PAD - TILE_GAP) / 2

# 一份架面：(书名, 作者, 有没有封面图)。封面色按顺序取主题那五档图表色。
SHELF = [
    ("百年孤独", "加西亚·马尔克斯", True),
    ("瓦尔登湖", "亨利·戴维·梭罗", True),
    ("卡拉马佐夫兄弟", "陀思妥耶夫斯基", True),
    ("雪国", "川端康成", True),
    ("平沙落雁", None, False),
    ("人类简史", "尤瓦尔·赫拉利", True),
    ("庄子的蝴蝶", "佚名", False),
    ("长夜难明", "紫金陈", True),
    ("城南旧事", "林海音", True),
]
GROUPS = [("梭罗", 6), ("民国书信", 3)]


def cover_art(T, x, y, index=0):
    """封面位的一块占位灰 —— 代表「这里是一张解码出来的图」，不是界面配色。"""
    return (f'<rect x="{x + 0.5:.1f}" y="{y + 0.5:.1f}" width="{CARD - 1:.1f}" '
            f'height="{COVER_H - 1:.1f}" rx="{RADIUS - 0.5}" fill="{ART}" fill-opacity="{ART_A}"/>')


def book_cell(T, x, y, title, author, has_art, index):
    """一格书卡：封面格 + 书名 + 作者。"""
    e = book_cover(T, x, y, CARD, COVER_H, title=None if has_art else title,
                   art=cover_art(T, x, y, index) if has_art else None)
    e.append(text(x, y + COVER_H + 4 + 15, truncate(title, 16, CARD), 16, T["fg"]))
    if author:
        e.append(text(x, y + COVER_H + 4 + TITLE_LINE + 4 + 12,
                      truncate(author, 12, CARD), 12, T["muted"]))
    return e


def group_cell(T, x, y, name, count, filled):
    """一张组卡：同格同高，里面 2×2 小图槽，底下组名与「N 本」。"""
    e = [f'<rect x="{x:.2f}" y="{y:.2f}" width="{CARD:.2f}" height="{COVER_H:.2f}" '
         f'rx="{RADIUS}" fill="{T["popover"]}"/>']
    for i in range(4):
        tx = x + TILE_PAD + (i % 2) * (TILE_W + TILE_GAP)
        ty = y + TILE_PAD + (i // 2) * (TILE_H + TILE_GAP)
        if i < filled:
            e.append(f'<rect x="{tx:.1f}" y="{ty:.1f}" width="{TILE_W:.1f}" height="{TILE_H:.1f}" '
                     f'rx="2" fill="{ART}" fill-opacity="{ART_A}"/>')
        else:
            e.append(f'<rect x="{tx:.1f}" y="{ty:.1f}" width="{TILE_W:.1f}" height="{TILE_H:.1f}" '
                     f'rx="2" fill="{T["muted_bg"]}"/>')
    e.append(text(x, y + COVER_H + 4 + 15, truncate(name, 16, CARD), 16, T["fg"]))
    e.append(text(x, y + COVER_H + 4 + TITLE_LINE + 4 + 12, f"{count} 本", 12, T["muted"]))
    return e


def cell_x(i):
    return GUTTER + i * (CARD + GAP)


def cell_y(row):
    return GRID_TOP + row * (CELL_H + GAP)


def toolbar(T, value=""):
    """顶行：满幅搜索框 + 右侧一颗三点钮（ghost + small = 24×24 纯图标）。"""
    e = []
    y = STATUS + 8.0
    box_w = W - 2 * GUTTER - 24.0 - GAP
    e += search_box(T, GUTTER, y, box_w, value, "搜索书名或作者")
    e.append(icon("ellipsis-vertical", W - GUTTER - 20.0, y + 4, 20, T["fg"]))
    return e


def shelf_body(T, books, groups=()):
    """架面：组卡与书排在同一条流上，谁最近被翻开谁的卡就在先。"""
    e = []
    cells = [group_cell(T, 0, 0, n, c, 4) for n, c in groups]
    cells += [book_cell(T, 0, 0, t, a, art, i) for i, (t, a, art) in enumerate(books)]
    for i, cell in enumerate(cells):
        row, col = divmod(i, COLUMNS)
        if row * (CELL_H + GAP) + GRID_TOP > H - BAR:
            break
        e.append(f'<g transform="translate({cell_x(col):.2f},{cell_y(row):.2f})">'
                 f'{"".join(cell)}</g>')
    return e


def grid_screen(T, name, note, books=SHELF, groups=GROUPS, query=""):
    e = status_bar(T) + toolbar(T, query) + shelf_body(T, books, groups) + bottom_bar(T, "书架")
    return svg(name, e, T["label"], note, T, area="书架")


def group_page_screen(T):
    """组那一页：顶栏换成返回 + 组名 + 几本书，底栏收起（钻进展屏）。"""
    e = status_bar(T)
    cy = STATUS + TOPBAR / 2
    e.append(icon("chevron-left", GUTTER + 6, cy - 10, 20, T["fg"]))
    e.append(text(GUTTER + 32, cy + 5.5, "返回", 14, T["fg"]))
    tx = GUTTER + 32 + tw("返回", 14) + 12
    e.append(text(tx, cy - 1, "梭罗", 14, T["fg"], 600))
    e.append(text(tx, cy + 13, "6 本书", 14, T["muted"]))
    top = STATUS + TOPBAR + 4
    for i, (title, author, art) in enumerate(SHELF[:6]):
        row, col = divmod(i, COLUMNS)
        cell = book_cell(T, 0, 0, title, author, art, i)
        e.append(f'<g transform="translate({cell_x(col):.2f},{top + row * (CELL_H + GAP):.2f})">'
                 f'{"".join(cell)}</g>')
    return svg("03-group-page", e, T["label"],
               "组那一页：顶栏是返回 + 组名 + 几本书，底栏整条收起，也没有搜索行", T, area="书架")


def book_info_screen(T):
    """书信息页：TopBar「信息」→ 封面 + 两格输入 → 发丝线 → 书目事实行。"""
    e = status_bar(T)
    cy = STATUS + TOPBAR / 2
    e.append(icon("chevron-left", GUTTER + 6, cy - 10, 20, T["fg"]))
    e.append(text(GUTTER + 32, cy + 5.5, "信息", 14, T["fg"], 600))
    y = STATUS + TOPBAR + 12
    e += book_cover(T, GUTTER, y, 96, 144, art=(
        f'<rect x="{GUTTER + 0.5}" y="{y + 0.5}" width="95" height="143" rx="{RADIUS - 0.5}" '
        f'fill="{ART}" fill-opacity="{ART_A}"/>'))
    fx = GUTTER + 96 + 12
    fw = W - GUTTER - fx
    for i, (label, value) in enumerate([("书名", "瓦尔登湖"), ("作者", "亨利·戴维·梭罗")]):
        ry = y + i * 46
        e.append(text(fx, ry + 12, label, 12, T["muted"]))
        e += input_box(T, fx, ry + 16, fw, value)
    y += max(144, 46 * 2 + 32) + 12
    e.append(hairline(y, GUTTER, W - GUTTER, T))
    y += 12
    facts = [("丛书", "世界文学"), ("主题", "自然文学 / 超验主义"), ("简介",
             "梭罗在瓦尔登湖畔独居两年两个月的记录。"), ("出版社", "上海译文出版社"),
             ("出版年", "2006"), ("语言", "中文"), ("ISBN", "978753274"),
             ("版权", "译本版权归出版社"), ("格式", "EPUB"), ("大小", "1.8 MB")]
    for label, value in facts:
        e.append(text(GUTTER, y + 15, label, 14, T["muted"]))
        e.append(text(GUTTER + 80, y + 16, truncate(value, 16, W - GUTTER - 80), 16, T["fg"]))
        y += 28
    # 底部提交条：primary「保存」整宽。
    e.append(f'<rect x="0" y="{H - 48}" width="{W}" height="48" fill="{T["bg"]}"/>')
    part, _ = button(T, GUTTER, H - 48 + 8, "保存", "primary", w=W - 2 * GUTTER)
    e += part
    return svg("04-book-info", e, T["label"],
               "书信息：封面 96×144 + 两格可改的书名/作者，发丝线下面是只有有值才摆的书目行", T, area="书架")


def scan_sheet_screen(T):
    """扫描 sheet：0.85 档高，头部一句汇总 + 三颗钮，下面是勾选清单。"""
    e = status_bar(T) + toolbar(T) + shelf_body(T, SHELF[:3], GROUPS[:1]) + bottom_bar(T, "书架")
    part, top = sheet(T, 0.85)
    e.append(scrim(T))
    e += part
    y = top + HANDLE_BAND
    e.append(text(GUTTER, y + 18, "在设备里找书", 16, T["fg"], 600))
    e.append(text(GUTTER, y + 38, "找到 12 本，勾选后导入", 14, T["muted"]))
    y += 50
    e.append(f'<rect x="0" y="{y}" width="{W}" height="0.5" fill="{T["border"]}"/>')
    y += 1
    rows = [("沙乡年鉴.epub", "1.2 MB", True), ("夜航西飞.epub", "860 KB", True),
            ("缓慢的河流.pdf", "4.4 MB", False), ("给青年诗人的信.txt", "22 KB", True),
            ("湖岸船坞.md", "9 KB", False), ("月光下的裁缝.epub", "2.7 MB", False),
            ("两京十五日.epub", "5.1 MB", False)]
    for name, size, checked in rows:
        e.append(text(GUTTER + 28, y + 23, truncate(name, 16, W - 2 * GUTTER - 28 - 60), 16, T["fg"]))
        e.append(text(W - GUTTER, y + 23, size, 12, T["muted"], anchor="end"))
        box = f'<rect x="{GUTTER}" y="{y + 9}" width="16" height="16" rx="3"'
        e.append(box + f' fill="{T["primary"]}"/>' if checked else
                 box + f' fill="none" stroke="{T["border"]}" stroke-width="1"/>')
        y += 36
    e.append(f'<rect x="0" y="{y}" width="{W}" height="0.5" fill="{T["border"]}"/>')
    y += 1
    btns = [("全选", "outline"), ("清除", "outline"), ("导入所选 3 本", "primary")]
    x = GUTTER
    for label, kind in btns:
        part, bw = button(T, x, y + 10, label, kind)
        e += part
        x += bw + 8
    return svg("05-scan-sheet", e, T["label"],
               "扫描 sheet：0.85 档高，标题 + 汇总 + 勾选清单（行 36）+ 底部三颗钮", T, area="书架")


def menu(T, x, y, items, width=180.0):
    """一条浮层菜单：项高 32、图标 16、内缩 12，`checked` 那一项右端一枚勾。"""
    h = 8 + len(items) * 32 + 8
    e = [f'<rect x="{x}" y="{y}" width="{width}" height="{h}" rx="{RADIUS}" '
         f'fill="{T["popover"]}" stroke="{T["border"]}" stroke-width="1"/>']
    yy = y + 8
    for item in items:
        kind = item[0]
        if kind == "item":
            _, label, icon_name, checked = item
            if icon_name:
                e.append(icon(icon_name, x + 12, yy + 8, 16, T["fg"]))
            e.append(text(x + 12 + (24 if icon_name else 0), yy + 21, label, 14, T["fg"]))
            if checked:
                e.append(icon("check", x + width - 26, yy + 9, 14, T["primary"]))
        elif kind == "head":
            e.append(text(x + 12, yy + 21, item[1], 12, T["muted"]))
        elif kind == "sep":
            e.append(f'<rect x="{x + 8}" y="{yy + 16}" width="{width - 16}" height="0.5" '
                     f'fill="{T["border"]}"/>')
        yy += 32
    return e, h


def import_menu_screen(T):
    """三点导入浮层：从顶栏那颗钮往下开（Anchor::TopRight）。"""
    e = status_bar(T) + toolbar(T) + shelf_body(T, SHELF[:6])
    e.append(scrim(T))
    items = [("item", "导入", "plus", False), ("item", "链接导入", "link", False),
             ("item", "扫描", "scan-search", False)]
    part, _ = menu(T, W - GUTTER - 140, STATUS + TOOLBAR_H, items, width=140)
    e += part
    e += bottom_bar(T, "书架")
    return svg("06-import-menu", e, T["label"],
               "三点导入浮层：最小宽 140，三条动作，右缘对齐那颗三点钮", T, area="书架")


def book_menu_screen(T):
    """书卡长按菜单：信息 / 分享 / 分组节头 + 各组开关 / 新建分组 / 分隔线 / 删除。"""
    e = status_bar(T) + toolbar(T) + shelf_body(T, SHELF[:6])
    e.append(scrim(T))
    items = [("item", "信息", "info", False), ("item", "分享", "share-2", False),
             ("head", "分组"), ("item", "梭罗", None, True), ("item", "民国书信", None, False),
             ("item", "新建分组", "folder-plus", False), ("sep",), ("item", "删除", "trash", False)]
    part, _ = menu(T, W - GUTTER - 180, STATUS + TOOLBAR_H + 60, items)
    e += part
    e += bottom_bar(T, "书架")
    return svg("07-book-menu", e, T["label"],
               "书卡长按菜单：分组那一节把每一组摆成可勾的一行，勾是右端唯一的标记", T, area="书架")


if __name__ == "__main__":
    OUT.mkdir(parents=True, exist_ok=True)
    out = {
        "01-shelf-grid.svg": grid_screen(
            LIGHT, "01-shelf-grid", "主架面：三列一排，组卡与书排在同一条流上",
            books=SHELF, groups=GROUPS),
        "02-shelf-search.svg": grid_screen(
            LIGHT, "02-shelf-search", "搜索有结果：词非空时查的是全库，进过组的也搜得出来",
            books=[b for b in SHELF if "史" in b[0] or "夜" in b[0]], groups=(), query="史"),
        "03-group-page.svg": group_page_screen(LIGHT),
        "04-book-info.svg": book_info_screen(LIGHT),
        "05-scan-sheet.svg": scan_sheet_screen(LIGHT),
        "06-import-menu.svg": import_menu_screen(LIGHT),
        "07-book-menu.svg": book_menu_screen(LIGHT),
        "08-shelf-grid-dark.svg": grid_screen(
            DARK, "08-shelf-grid-dark", "主架面的深色对照", books=SHELF, groups=GROUPS),
    }
    for name, body in out.items():
        (OUT / name).write_text(body, encoding="utf-8")
    print("done:", len(out), "screens")
    render_pngs(OUT)
