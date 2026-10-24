#!/usr/bin/env python3
"""Folio 首次启动那一屏，与「还没有内容」那四屏（compact 档）SVG 设计稿生成器。

两块放一份是因为它们说的是同一件事：**还没有东西可摆的时候这一屏长什么样**。

| 这一格 | 数 | 出处 |
| --- | --- | --- |
| 欢迎页两侧留白 | 24 | `features/welcome/mod.rs:26` `GUTTER` |
| 内容列最大宽 | 420 | 同上 `:29` `COLUMN`（384 的手机上这一格不起作用，列就是 336） |
| 整列居中 | `items_center` + `justify_center` | 同上 `:66-70` —— 上下左右都居中，不是靠顶 |
| 图标 | **软件图标本体**，64 见下方说明 | 实现现在是 `BookOpen` `.large()` = 24（`features/welcome/mod.rs:75`、`component/icon.rs:186` `large()` 是 `size_6`） |
| 标题 | `text_xl` 20 + SEMIBOLD | 同上 `:78-79`，文案 `welcome.title` |
| 三块之间的缝 | 16（`gap_4`） | 同上 `:70` |
| 两颗按钮之间 | 8（`gap_2`） | 同上 `:87` |
| 按钮 | 高 32、圆角 6、内缩 10、图标 20、图标↔字 8、字 16、**内容居中** | `button.rs:613-615`（`Size::Medium` → `h_8` + `px_2p5`）、`:677-691`（`size_full().justify_center().gap_2()`） |
| 空状态 | 居中、两行、缝 8、标题 16 / 说明 14 次要色、**没有图标** | `components/primitives.rs:31-45` |

**图标这一格与实现不同，是刻意的设计改动**：欢迎页原来摆的是 `IconName::BookOpen` 那枚
24 的线描图标，稿子改成**软件图标本体**（`packaging/icons/folio.png`，192×192，自带深绿
圆角方块底板），并把它放大到 64。两个理由：

- 带自己底板的方块标在 24 上会糊成一团 —— 那本书与那枚书签在 24 像素里只剩一团白，
  认不出是什么；
- 这一屏的任务是「第一次打开，先认出这是哪个应用」，而线描的通用书本形不担这个任务。
  24 那一档是给列表行首的图标用的，不是给整屏唯一那颗标用的。

深色那一侧看得清：底板是深绿渐变，落在 `#0a0a0a` 上边缘会软掉，但里面那本白色的书把形状
撑住了 —— 所以不需要给深色另配一版图标。

**一处文案与界面不符，是这轮量出来的**：`shelf.empty_hint` 现译是「用右边的「导入」选一本
epub、markdown 或纯文本」，而 compact 档那一条顶栏上**没有叫「导入」的东西** —— 手机上是一根
搜索框加一颗三点按钮（`features/shelf/view/toolbar.rs:47-59`），「导入」藏在点开的那条菜单里。
那句话在桌面上成立，在手机上把读者往一个不存在的位置指。稿子照现译画，因为**改文案是另一笔
账**，但这一条得记在这儿。
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from _kit import *  # noqa: E402,F403

OUT = Path(__file__).parent

WELCOME_GUTTER = 24.0
COLUMN = 420.0
ICON = 64.0          # 软件图标本体那一格（实现里那枚线描图标是 24，理由见文件头）
GAP_BLOCK = 16.0     # gap_4
GAP_BUTTONS = 8.0    # gap_2
TITLE_LINE = 28.0    # text_xl 20 的行盒
BODY_LINE = 20.0     # text_base 16
NOTE_LINE = 17.0     # text_sm 14


def welcome_screen(T):
    """首次启动：图标、标题、两颗按钮，整列居中。没有底栏 —— 这一屏还没进主界面。"""
    e = status_bar(T)
    col_w = min(W - 2 * WELCOME_GUTTER, COLUMN)
    x = (W - col_w) / 2
    # 三块的高：图标 64、标题 28、按钮列 32+8+32；两块缝各 16。
    stack = ICON + GAP_BLOCK + TITLE_LINE + GAP_BLOCK + (BUTTON_H * 2 + GAP_BUTTONS)
    y = (H + STATUS - stack) / 2
    e.append(app_icon(W / 2 - ICON / 2, y, ICON))
    y += ICON + GAP_BLOCK
    e.append(text(W / 2, y + 20, "欢迎使用 Folio", 20, T["fg"], 600, anchor="middle"))
    y += TITLE_LINE + GAP_BLOCK
    part, _ = button(T, x, y, "选择书库文件夹…", "primary", "folder-open", w=col_w)
    e += part
    part, _ = button(T, x, y + BUTTON_H + GAP_BUTTONS, "使用默认位置", "ghost", w=col_w)
    e += part
    return e


def empty_state(T, title, note, top=STATUS, bottom=BAR):
    """`empty_state` 那颗组件：两行字在可用区正中，没有图标。

    说明那一行会折行 —— 组里空那一句在三款手机上超过一行宽（`shelf.group_empty_hint`），
    而实现里那是一格普通 `div`，跟着宽度折。稿子按同样的规则折，最多两行。
    """
    lines = wrap_cjk(note, 14, W - 2 * 24.0, 2)
    stack = BODY_LINE + 8.0 + NOTE_LINE * len(lines)
    y = (top + (H - bottom)) / 2 - stack / 2
    e = [text(W / 2, y + 15, title, 16, T["fg"], anchor="middle")]
    for i, line in enumerate(lines):
        e.append(text(W / 2, y + BODY_LINE + 8.0 + i * NOTE_LINE + 13, line, 14, T["muted"],
                      anchor="middle"))
    return e


def shelf_toolbar(T, value=""):
    """书架顶行（compact）：满幅搜索框 + 右侧一颗三点钮。"""
    e = []
    y = STATUS + 8.0                      # py_2 = 8
    box_w = W - 2 * 12.0 - 24.0 - 8.0     # gap_2 px(12)，三点钮占 24
    e += search_box(T, 12.0, y, box_w, value, "搜索书名或作者")
    e.append(icon("ellipsis-vertical", W - 12.0 - 10.0, y + 4, 20, T["fg"]))
    return e


def group_top_bar(T, title, subtitle):
    """组那一页的顶栏：返回（带「返回」二字）+ 组名 + 那一组几本书。"""
    e = [f'<rect x="0" y="{STATUS}" width="{W}" height="{TOPBAR}" fill="{T["bg"]}"/>']
    cy = STATUS + TOPBAR / 2
    e.append(icon("chevron-left", 12 + 6, cy - 10, 20, T["fg"]))
    e.append(text(12 + 32, cy + 5.5, "返回", 14, T["fg"]))
    e.append(text(12 + 32 + tw("返回", 14) + 12, cy - 1, title, 14, T["fg"], 600))
    e.append(text(12 + 32 + tw("返回", 14) + 12, cy + 13, subtitle, 14, T["muted"]))
    return e


if __name__ == "__main__":
    OUT.mkdir(parents=True, exist_ok=True)
    screens = {}

    e = welcome_screen(LIGHT)
    screens["01-welcome.svg"] = svg("01-welcome", e, LIGHT["label"],
                                    "首次启动：图标 + 标题 + 两颗按钮，整列居中，没有底栏", LIGHT, area="欢迎")
    e = welcome_screen(DARK)
    screens["02-welcome-dark.svg"] = svg("02-welcome-dark", e, DARK["label"],
                                         "首次启动的深色对照", DARK, area="欢迎")

    # 四屏空状态。三屏在书架里（顶行那根搜索框跟着在），一屏在笔记里（只有底栏）。
    e = status_bar(LIGHT) + empty_state(LIGHT, "笔记", "还没有添加过批注") + bottom_bar(LIGHT, "笔记")
    screens["03-empty-notes.svg"] = svg("03-empty-notes", e, LIGHT["label"],
                                        "笔记目的地：还没有内容时那两行字", LIGHT, area="空状态")

    e = status_bar(LIGHT) + shelf_toolbar(LIGHT) + \
        empty_state(LIGHT, "书架还是空的", "用右边的「导入」选一本 epub、markdown 或纯文本") + \
        bottom_bar(LIGHT, "书架")
    screens["04-empty-shelf.svg"] = svg("04-empty-shelf", e, LIGHT["label"],
                                        "空书架：顶栏照在，架面换成那两行字", LIGHT, area="空状态")

    e = status_bar(LIGHT) + shelf_toolbar(LIGHT, "百年孤独") + \
        empty_state(LIGHT, "没有书名或作者对得上", "换个词试试 —— 这里搜的是书名和作者，不是正文") + \
        bottom_bar(LIGHT, "书架")
    screens["05-empty-search.svg"] = svg("05-empty-search", e, LIGHT["label"],
                                         "搜不到：搜索框里留着那个词，架面换成那两行字", LIGHT, area="空状态")

    e = status_bar(LIGHT) + group_top_bar(LIGHT, "梭罗", "0 本书") + \
        empty_state(LIGHT, "这一片还没有书",
                    "长按或右键一张卡片，在菜单的「分组」里选这一组，就把那本书放进来",
                    top=STATUS + TOPBAR, bottom=0)
    screens["06-empty-group.svg"] = svg("06-empty-group", e, LIGHT["label"],
                                        "组里空：钻进展屏，底栏收起，那两行字居中在剩下的高里", LIGHT, area="空状态")

    for name, body in screens.items():
        (OUT / name).write_text(body, encoding="utf-8")
    print("done:", len(screens), "screens")
    render_pngs(OUT)
