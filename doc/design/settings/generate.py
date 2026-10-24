#!/usr/bin/env python3
"""Folio 设置界面（compact 档）SVG 设计稿生成器。

十一个屏的几何常量、主题 token、Lucide 图标表与行构造器全部来自 [`../_kit.py`](../_kit.py)
—— 那一份是各屏共用的尺，本文件只写「设置这一屏摆哪几行」。按 2026-10-24 的定稿：
**行内不摆灰色说明文字、卡片上方不摆节头** —— 每行只有标题与控件，组与组只靠
32 的缝分开。几何数字来自 crates/folio-app/src/ui/features/settings/view/compact.rs
的真实实现；颜色取 gpui-kit Default Light / Default Dark 的 token 值；
文案取 locales/zh-CN.toml，取值取 folio-core 配置默认值。
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

def root_screen(T):
    pages = [("palette", "外观"), ("type", "文字"), ("columns-3", "版面"), ("a-large-small", "字体"),
             ("folder-open", "书库"), ("list-tree", "目录"), ("hard-drive", "存储"), ("info", "关于")]
    e = status_bar(T)
    y = STATUS + 8
    card_h = len(pages) * ROW + (len(pages) - 1) * 0.5
    e.append(card(PAD, y, W - 2 * PAD, card_h, T))
    for i, (ic, label) in enumerate(pages):
        ry = y + i * (ROW + 0.5)
        row, _ = push_row(label, icon_name=ic)
        e.append('<g transform="translate(0,{:.2f})">{}</g>'.format(ry, "".join(resolve(row, T))))
        if i:
            e.append(hairline(ry, PAD + 1 + 56, W - PAD - 1, T))
    e += bottom_bar(T)
    return svg("01-root", e, T["label"], "根屏：入口列表，一行一页，右端只有箭头", T)


def appearance_screen(T):
    e = page_screen(T, "外观", [
        [popup_row("浅深", "跟随系统"),
         popup_row("浅色主题", "Default Light"),
         popup_row("深色主题", "Default Dark")],
        [titled_row("主题目录", content_path_button("~/.config/folio/themes"))],
    ])
    return svg("02-appearance", e, T["label"], "外观：浅深与两侧主题，主题目录", T)


def typography_screen(T):
    e = page_screen(T, "文字", [
        [stepper_row("字号", "17"),
         stepper_row("行距", "1.75"),
         stepper_row("段距", "0.75")],
        [stepper_row("首行缩进", "2.00"),
         switch_row("两端对齐", True)],
        [push_row("翻页动画", "左右平移")],
        [push_row("划线方式", "高亮")],
    ])
    return svg("03-typography", e, T["label"], "文字：字号行距段距、缩进对齐、翻页、划线", T)


def layout_screen(T):
    e = page_screen(T, "版面", [
        [stepper_row("最大行长", "0"),
         stepper_row("左右页边距", "24")],
    ])
    return svg("04-layout", e, T["label"], "版面：最大行长与左右页边距", T)


def fonts_screen(T):
    e = page_screen(T, "字体", [
        [text_row("正文字体", "LXGW WenKai")],
    ])
    return svg("05-fonts", e, T["label"], "字体：正文字体家族", T)


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
    ])
    return svg("06-library", e, T["label"], "书库：位置、文件夹监听、Wi-Fi 传书", T)


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
    cap_y, total_y, ring_cy = y + 22, y + 58, y + 160
    legend_y = y + 264
    card_h = 264 + len(STORAGE_SLICES) * 24 + 8
    e.append(card(PAD, y, W - 2 * PAD, card_h, T))
    e.append(text(PAD + PAD, cap_y, "占用", 14, T["muted"]))
    e.append(text(PAD + PAD, total_y, "1.2 GB", 30, T["fg"], 600))
    e += donut(W / 2, ring_cy, 82, 54, T)
    for i, (name, bytes_, pct) in enumerate(STORAGE_SLICES):
        ly = legend_y + i * 24
        e.append(f'<circle cx="{PAD + PAD + 4}" cy="{ly + 11}" r="4" fill="{T["charts"][i]}"/>')
        e.append(text(PAD + PAD + 16, ly + 16.5, name, 16, T["fg"]))
        e.append(text(280, ly + 16, pct, 14, T["muted"], anchor="end"))
        e.append(text(RIGHT, ly + 16.5, f"{bytes_} MB", 16, T["fg"], anchor="end"))
    y += card_h + GAP_SECT
    # 清理：一档一行，右端字节 + ghost 小钮。
    clean = [clean_row("缩略图缓存", "214 MB", "broom"),
             clean_row("漫画解压缓存", "96 MB", "broom"),
             clean_row("旧日志", "4.2 MB", "broom"),
             clean_row("整理数据库", "12 MB", "database")]
    y = draw_card(e, y, clean, T)
    # 位置：两行「路径 + 打开所在文件夹」。
    y = draw_card(e, y, [titled_row("软件数据目录", content_path_button("/home/reader/.local/share/folio")),
                         titled_row("日志目录", content_path_button("/home/reader/.local/share/folio/logs"))], T)
    return svg("08-storage", e, T["label"], "存储：占用环图、清理、数据目录", T)


def about_screen(T):
    e = page_screen(T, "关于", [
        [push_row("版本", "0.1.1", chevron=False)],
        [titled_row("更新", content_status_ghost("还没查过", "检查更新", None)),
         titled_row("更新日志", content_status_ghost("看这一版改了什么", "查看", None)),
         titled_row("反馈日志", content_status_ghost("还没发过", "发送日志", None))],
        [popup_row("界面语言", "简体中文")],
    ])
    return svg("09-about", e, T["label"], "关于：版本、更新与反馈、界面语言", T)


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
]

if __name__ == "__main__":
    OUT.mkdir(parents=True, exist_ok=True)
    for name, fn, theme in SCREENS:
        (OUT / name).write_text(fn(theme), encoding="utf-8")
    print("done:", len(SCREENS), "screens")
    render_pngs(OUT)
