#!/usr/bin/env python3
"""Folio「笔记」目的地（compact 档）SVG 设计稿生成器。

三屏：一条流、搜索有结果、深色对照。**这一档整格从零设计** —— 实现里 `NotesView` 只有
30 行，画的是空状态（`features/notes/mod.rs:18-30`），而批注早就在库里：
`annotations(book_id, anchor_json, note, color, style, stale, created_at, updated_at)`
（`folio-core/src/store/annotations.rs:32-60`、`schema.rs:61`）。所以这三张是**稿先到**，
下面每一条数都要在 README 里回答「实现里有没有对应物」。

主形状：全库一条流，按时间倒序，卡左缘一条色标吃划线那五档。

| 这一格 | 数 | 出处 |
| --- | --- | --- |
| 卡的左右内缩 | 16 | `_kit.PAD`（与设置那一档的卡同一条脊线，切目的地时不跳） |
| 卡圆角 / 描边 | 8 / 1 | `CARD_R` = `theme.radius_lg`、`card()` |
| 卡与卡的缝 | 12 | 同一条流里的同类卡片挨着排；32 那道缝是**组与组**的（`GAP_SECT`） |
| 左缘色标 | 宽 3、距卡内左缘 2、上下各内缩 12、圆角 1.5 | 目录当前章那条左标记条同一档（`toc_panel.rs:62-64,68`：2.9 宽、距左 2.1） |
| 内容左界 | 32 | `PAD + PAD`，与所有行构造器同一条线 |
| 引文 | 16 号、行高 28、最多 3 行 | 字号取列表里的标题档（`book_card.rs:361-378` 书名的 16）；行高按正文那条比例 1.75 落到 16 号 |
| 批注 | 14 号、行高 22、最多 3 行、次要前景色 | 次要一档取 `text_sm` 14（与目录标题同号，`toc_panel.rs:1318`） |
| 底行 | 12 号次要色 | 书卡作者那一档（`book_card.rs:361-378` 的 12） |
| 卡内上下内缩 | 12 | `pt_3`，阅读页底栏卡片用的也是这个数（`regions.rs:83-105`） |
| 搜索框 | 满幅、Medium 档、有词即出清除钮 | `search_box()`：`input.rs:531-535` 那条判据 |
| 底栏「笔记」那颗的计数 | 高 14 的胶囊、10 号字、`primary` 底 | **实现里没有**：`Store::annotation_count`（`annotations.rs:176`）算好了但全 App 零调用点 |

引文与批注那几段文字是稿子里的占位内容（与 `reader/generate.py` 那份合成章节同一口气），
不代表任何一本真书里真有的句子；色标按「这一条划成哪一档」逐张换着摆，五档在
`_kit.INK_COLORS`（顺序 = `ANNOTATION_COLORS`）。

空的那一格不在这儿画第二遍：一条流都没有时摆的是
[`../welcome/03-empty-notes.svg`](../welcome/03-empty-notes.svg) 那颗 `empty_state`
（「笔记 / 还没有添加过批注」），与书架空、搜不到同一份构件。
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from _kit import *  # noqa: E402,F403

OUT = Path(__file__).parent

CARD_X, CARD_W = PAD, W - 2 * PAD          # 卡占 16..368
TEXT_X = PAD + PAD                          # 内容左界 32
BAR_W, BAR_INSET, BAR_TOP = 3.0, 2.0, 12.0  # 左缘色标：宽、距卡内左缘、上下内缩
PAD_Y = 12.0                                # 卡内上下内缩
QUOTE_FS, QUOTE_LH, QUOTE_MAX = 16.0, 28.0, 3
NOTE_FS, NOTE_LH, NOTE_MAX = 14.0, 22.0, 3
META_FS, META_LH = 12.0, 16.0
GAP_QN, GAP_NM = 6.0, 10.0                  # 引文与批注之间、正文与底行之间
CARD_GAP = 12.0                             # 卡与卡的缝

# 一份批注：(书名, 引文, 你写的话（空串＝只划了没写）, 日期, 第几档色)。
# 按时间倒序摆 —— 与架面那条「最近翻开的在前」同一个方向感（`books.rs:147-148` 的 ORDER BY）。
NOTES = [
    ("瓦尔登湖", "划桨的手不需要想。想的是另一件事：水把船托住，船才认得方向。",
     "这一句和上一章「冰下的水」是同一件事的两头。", "10 月 24 日", 4),
    ("瓦尔登湖", "湖是风景中最活泼、最伸展的。",
     "", "10 月 23 日", 1),
    ("百年孤独", "多年以后，面对行刑队，奥雷里亚诺·布恩迪亚上校将会回想起父亲带他去见识冰块的那个遥远的下午。",
     "三个时态叠在开头一句里，难怪人人都抄它。", "10 月 21 日", 2),
    ("雪国", "穿过县界长长的隧道，便是雪国。",
     "夜里读到这里，窗外正好在下雪。", "10 月 19 日", 0),
    ("卡拉马佐夫兄弟", "爱具体的人，不要爱抽象的人类。",
     "这一条要抄下来贴在书桌前。", "10 月 16 日", 3),
    ("人类简史", "帝国是一种把许多不同的民族塞进同一个政治框架里的尝试。",
     "和上一本的「文明」那一章可以并起来读。", "10 月 12 日", 4),
]


def note_card(T, y, item):
    """一张批注卡。返回 (元素, 卡底 y)。

    高度是**算出来的**，不是写死的：引文几行、有没有写话、底行一行 —— 三样凑出几种高。
    实现里列表项也是这个形状（`List` 的 item 自己量高），所以稿子不许挑一个数硬摆。
    """
    book, quote, note, day, ink = item
    e = []
    q_lines = wrap_cjk(quote, QUOTE_FS, CARD_W - (TEXT_X - PAD) - 12, QUOTE_MAX)
    n_lines = wrap_cjk(note, NOTE_FS, CARD_W - (TEXT_X - PAD) - 12, NOTE_MAX) if note else []
    h = PAD_Y + len(q_lines) * QUOTE_LH
    if n_lines:
        h += GAP_QN + len(n_lines) * NOTE_LH
    h += GAP_NM + META_LH + PAD_Y
    e.append(card(CARD_X, y, CARD_W, h, T))
    # 左缘色标：吃这一条划的那一档，不跟着主题走（换浅色主题它还是那五枚圆点的颜色）。
    e.append(f'<rect x="{CARD_X + BAR_INSET}" y="{y + BAR_TOP}" width="{BAR_W}" '
             f'height="{h - 2 * BAR_TOP}" rx="{BAR_W / 2}" fill="{INK_COLORS[ink]}"/>')
    cy = y + PAD_Y + QUOTE_FS
    for line in q_lines:
        e.append(text(TEXT_X, cy, line, QUOTE_FS, T["fg"]))
        cy += QUOTE_LH
    if n_lines:
        cy += GAP_QN - (QUOTE_LH - QUOTE_FS)
        for line in n_lines:
            e.append(text(TEXT_X, cy, line, NOTE_FS, T["muted"]))
            cy += NOTE_LH
    # 底行：左边「出自哪本书」，右边「哪天划的」。都不做可点 —— 点整张卡才跳回原句。
    my = y + h - PAD_Y - 2
    e.append(text(TEXT_X, my, truncate(f"《{book}》", META_FS, CARD_W * 0.6), META_FS, T["muted"]))
    e.append(text(PAD + CARD_W - PAD, my, day, META_FS, T["muted"], anchor="end"))
    return e, y + h


def flow_screen(T, name, label, query="", items=None):
    """一条流：搜索框 + 眉标（几条、出自几本）+ 一批卡 + 底栏。"""
    items = items if items is not None else NOTES
    e = status_bar(T)
    y = STATUS + 10
    e += search_box(T, PAD, y, W - 2 * PAD, query, "搜索书名或批注")
    y += 32 + 12
    books = len({it[0] for it in items})
    head = f"{len(items)} 条 · 出自 {books} 本" if items else "对不上"
    e.append(text(PAD, y + 11, head, 14, T["muted"]))
    y += 14 + 16
    for item in items:
        part, y = note_card(T, y, item)
        e += part
        y += CARD_GAP
    e += bottom_bar(T, "笔记", count=len(items) if not query else None)
    note = ("笔记·一条流：全库按时间倒序，卡左缘那条色标吃划线那五档；"
            "点整张卡跳回书里那一句（`anchor::relocate` 那四级降级链兜住找不到的）"
            if not query else
            "笔记·搜索有结果：框里有词就出清除钮，眉标跟着换成命中数；"
            "搜的是批注原文与书名，不是正文 —— 正文那一套在目录那屏的「全文」tab 里")
    return svg(name, e, label, note, T, area="笔记")


if __name__ == "__main__":
    OUT.mkdir(parents=True, exist_ok=True)
    hit = [n for n in NOTES if "湖" in n[1] or "湖" in n[0]]
    out = {
        "01-notes-flow.svg": flow_screen(LIGHT, "01-notes-flow", LIGHT["label"]),
        "02-notes-search.svg": flow_screen(LIGHT, "02-notes-search", LIGHT["label"], "湖", hit),
        "03-notes-dark.svg": flow_screen(DARK, "03-notes-dark", DARK["label"]),
    }
    for name, body in out.items():
        (OUT / name).write_text(body, encoding="utf-8")
    print("done:", len(out), "screens")
    render_pngs(OUT)
