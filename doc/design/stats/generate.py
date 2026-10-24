#!/usr/bin/env python3
"""Folio「统计」目的地（compact 档）SVG 设计稿生成器。

整屏只有一张图：一格一天、按日历排开的热力图。尺与颜色全部来自
`crates/folio-app/src/ui/features/stats/`，逐条对得上：

| 这一格 | 数 | 出处 |
| --- | --- | --- |
| 左右留白 | 12 | `view/mod.rs:43` `GUTTER`（与书架共用同一个数，切目的地时那条脊线不该跳） |
| 一格边长 | 8 | `view/mod.rs:47,49` `CELL_MIN=8` `CELL_MAX=14`，384 宽均出来夹在下限 |
| 格的缝 | 3 | `view/mod.rs:51` `CELL_GAP` |
| 车标车道 | 28 | `view/mod.rs:53` `LABEL_LANE` |
| 月标签车道高 | 18 | `view/mod.rs:55` `MONTH_LANE` |
| 列 × 行 | 53 × 7 | `model/heatmap.rs:26,28` `WEEKS` / `ROWS`（列是周、行是星期几，r=0 是周一） |
| 分档 | 0 / ≥15 / ≥30 / ≥60 分钟 | `model/heatmap.rs:38` `LEVEL_MINUTES`，绝对档不是分位数 |
| 五档颜色 | `muted` + `chart_3` 的 0.30/0.55/0.80/1.00 | `view/mod.rs:67-76` `ramp` |
| 车标 | 只周一 / 周三 / 周五 | `view/mod.rs:57-61` `ROW_LABELS`（七行全标会把车道撑成图的一半宽） |
| 月标签间隔 | 至少 4 列 | `model/heatmap.rs:41` `MONTH_LABEL_GAP` |
| 图例方块 | 5 枚 14×14、缝 4、圆角 `radius_sm`=3 | `view/mod.rs:309-326`（方块用 `CELL_MAX`，不跟着当帧的 8 缩） |

**这一屏在手机上是要横着滚的**：53 列 × pitch 11 = 580 的内容宽，塞不进 384 − 24 − 28 = 332
那一条可用宽（`view/mod.rs:83-98` 实算：格子不缩到 `CELL_MIN` 以下，多出来的部分横向滚，
`view/mod.rs:249`）。稿子画的是**滚到最左**那一帧，也就是一年里最早的那几周 —— 今天落在
最后一列，不滚是看不见的。这一条不是画图的妥协，是实现本来的样子。
"""

import random
import sys
from datetime import date, timedelta
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from _kit import *  # noqa: E402,F403

OUT = Path(__file__).parent

GUTTER = 12.0        # stats/view/mod.rs:43
CELL = 8.0           # 384 宽实算夹到 CELL_MIN
CELL_GAP = 3.0
PITCH = CELL + CELL_GAP
LABEL_LANE = 28.0
MONTH_LANE = 18.0
MONTH_GAP = 4        # heatmap.rs:41 MONTH_LABEL_GAP
WEEKS, ROWS = 53, 7
CELL_R = 3.0         # theme().radius_sm = radius / 2
LEGEND_SQ = 14.0     # 图例那五枚方块用 CELL_MAX

TODAY = date(2026, 10, 24)
GRID_X = GUTTER + LABEL_LANE
# 一格当帧的分钟数由档位反推一个代表值 —— 稿子只需要让深浅摆在对的位置上。
LEVEL_MINUTES = {1: 8, 2: 20, 3: 40, 4: 75}


def start_monday():
    """最早那一列的周一：末列是今天所在那周（周一起），往前数 52 周。"""
    this_monday = TODAY - timedelta(days=TODAY.weekday())
    return this_monday - timedelta(weeks=WEEKS - 1)


def level_of(minutes):
    """0 秒是 0 档，其余按 LEVEL_MINUTES 切（heatmap.rs:204-212）。"""
    if minutes <= 0:
        return 0
    return 1 + sum(1 for t in (15, 30, 60) if minutes >= t)


def ramp(T):
    """五档颜色：0 档 `muted.background`，1..4 档同一个 `chart_3` 递进不透明度。"""
    solid = "#2563eb"
    return [T["muted_bg"], f"{solid}|0.30", f"{solid}|0.55", f"{solid}|0.80", solid]


def square(x, y, size, color, T, rx=CELL_R):
    """一枚方块，颜色可以是 `#hex` 或 `#hex|alpha`（档位的透明度）。"""
    hexv, _, alpha = color.partition("|")
    a = f' fill-opacity="{alpha}"' if alpha else ""
    return (f'<rect x="{x:.1f}" y="{y:.1f}" width="{size}" height="{size}" rx="{rx}" '
            f'fill="{hexv}"{a}/>')


def cells(rng):
    """一份像样的习惯：工作日薄、周末厚、近两个月密、中间空一段夏天。"""
    out = []
    for i in range(WEEKS * ROWS):
        dt = start_monday() + timedelta(days=i)
        if dt > TODAY:
            continue
        age = (TODAY - dt).days
        if 150 <= age <= 185:
            continue
        chance = (0.34 if age > 300 else 0.52) + (0.24 if dt.weekday() >= 5 else 0.0)
        if rng.random() > chance:
            continue
        out.append((i // ROWS, i % ROWS, level_of(rng.choice(list(LEVEL_MINUTES.values())))))
    return out


def month_labels():
    """换月那一列起标；相邻至少隔 4 列；1 月画年份（view/mod.rs:348-353）。"""
    labels, last = [], -99
    for col in range(WEEKS):
        d = start_monday() + timedelta(weeks=col)
        if col - last < MONTH_GAP:
            continue
        labels.append((col, str(d.year) if d.month == 1 else f"{d.month} 月"))
        last = col
    return labels


def heatmap(T, grid, y):
    """月标签一行、左边车标、右边 53×7 的格子。返回 (元素, 块底 y)。"""
    e = []
    colors = ramp(T)
    top = y + MONTH_LANE
    for col, label in month_labels():
        e.append(text(GRID_X + col * PITCH, top - 5, label, 12, T["muted"]))
    for row, label in ((0, "周一"), (2, "周三"), (4, "周五")):
        e.append(text(GUTTER, top + row * PITCH + CELL - 1, label, 12, T["muted"]))
    for col, row, level in grid:
        e.append(square(GRID_X + col * PITCH, top + row * PITCH, CELL, colors[level], T))
    return e, top + ROWS * PITCH - CELL_GAP


def legend(T, y):
    """图例：整块右对齐，`少` 五枚方块 `多`。"""
    e = []
    colors = ramp(T)
    x = W - GUTTER - (tw("少", 14) + 4 + 5 * LEGEND_SQ + 4 * 4 + tw("多", 14))
    e.append(text(x, y + LEGEND_SQ - 2, "少", 14, T["muted"]))
    x += tw("少", 14) + 4
    for i in range(5):
        e.append(square(x, y, LEGEND_SQ, colors[i], T))
        x += LEGEND_SQ + 4
    e.append(text(x, y + LEGEND_SQ - 2, "多", 14, T["muted"]))
    return e, y + LEGEND_SQ


def stats_screen(T, grid, line, name, note):
    """整屏骨架：状态栏 → 眉标 → 图 → 图例 → 结论 → 底栏。"""
    e = status_bar(T)
    y = STATUS + 8
    e.append(text(GUTTER, y + 11, "近 12 个月", 14, T["muted"]))
    y += 28
    part, y = heatmap(T, grid, y)
    e += part
    part, y = legend(T, y + 8)
    e += part
    e.append(text(GUTTER, y + 8 + 11, line, 14, T["muted"]))
    # 内容宽 580 超出画布，右缘那一列是被裁掉的：淡出一点，说明还能往右滚。
    e.append(f'<rect x="{W - 26}" y="{STATUS}" width="26" height="{H - STATUS - BAR}" '
             f'fill="{T["bg"]}" fill-opacity="0.55"/>')
    e += bottom_bar(T, "统计")
    return svg(name, e, T["label"], note, T, area="统计")


def empty_store_screen(T):
    """读不到书库那一条分支（view/mod.rs:196-204）：两行字，图整块不摆。"""
    e = status_bar(T)
    y = STATUS + 8
    e.append(text(GUTTER, y + 16, "读不到书库", 16, T["fg"]))
    e.append(text(GUTTER, y + 37, "统计读的是本地书库，请确认书库目录能打开。", 14, T["muted"]))
    e += bottom_bar(T, "统计")
    return svg("03-stats-no-store", e, T["label"],
               "统计·读不到书库：两行字，热力图整块不摆", T, area="统计")


def readout(grid):
    """结论那三个数由同一份格子念出来：天数、总时长、最长连续。"""
    minutes = {(c, r): LEVEL_MINUTES[lv] for c, r, lv in grid if lv}
    read = sum(minutes.values())
    hours, mins = divmod(read, 60)
    streak = best = 0
    for i in range(WEEKS * ROWS):
        dt = start_monday() + timedelta(days=i)
        if dt > TODAY:
            break
        if minutes.get((i // ROWS, i % ROWS)):
            streak += 1
            best = max(best, streak)
        else:
            streak = 0
    return f"读了 {len(minutes)} 天，一共 {hours} 小时 {mins} 分钟，最长连续 {best} 天"


if __name__ == "__main__":
    OUT.mkdir(parents=True, exist_ok=True)
    grid = cells(random.Random(20261024))
    line = readout(grid)
    out = {
        "01-stats-year.svg": stats_screen(
            LIGHT, grid, line, "01-stats-year",
            "统计·近 12 个月：热力图 + 图例 + 一句结论，横向要滚"),
        "02-stats-none.svg": stats_screen(
            LIGHT, [], "还没有阅读记录", "02-stats-none",
            "统计·一年没读：满网格照画、全 0 档，只把结论换成那一句"),
        "03-stats-no-store.svg": empty_store_screen(LIGHT),
        "04-stats-year-dark.svg": stats_screen(
            DARK, grid, line, "04-stats-year-dark", "统计·近 12 个月的深色对照"),
    }
    for name, body in out.items():
        (OUT / name).write_text(body, encoding="utf-8")
    print("done:", len(out), "screens")
    render_pngs(OUT)
