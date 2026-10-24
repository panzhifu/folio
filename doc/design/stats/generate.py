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
# 时长条那一排：条子沿用热力图一格的 8×3，行高 40（比 44 那一档矮，因为这一屏没有可点
# 的行 —— 榜单只是读，不回答「按下去会怎样」）。条子左界给书名留 132，右界离分钟数 12。
BAR_X0, BAR_W, ROW_H = GUTTER + 144.0, 96.0, 40.0
CHART_SOLID = "#2563eb"          # `chart_3`：`ramp` 最重那一档与时长条同一枚颜色
# 一格当帧的分钟数由档位反推一个代表值 —— 稿子只需要让深浅摆在对的位置上。
LEVEL_MINUTES = {1: 8, 2: 20, 3: 40, 4: 75}
# 榜单五本的权重（合计 0.86，剩下那 0.14 是榜外的书）。稿子里的数必须与格子那一份
# 总量对得上：先按权重切总时长，再让条子按最长那本铺满 —— 不许各编各的。
TOP_WEIGHTS = [0.31, 0.21, 0.16, 0.11, 0.07]


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
    solid = CHART_SOLID
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


def hours_minutes(minutes):
    """把分钟数念成一句人话。与 `model/heatmap.rs:48-61` 的 `duration()` 同一口径：
    够一小时就说「几小时几分」，不够就只说分钟 —— 榜单那五行不许出现「1975 分钟」。"""
    h, m = divmod(int(round(minutes)), 60)
    return f"{h} 小时 {m} 分" if h else f"{m} 分"


def stats_screen(T, grid, line, name, note, avg=None, top=None):
    """整屏骨架：状态栏 → 眉标 → 图 → 图例 → 结论 →（有数才摆）平均与榜单 → 底栏。"""
    e = status_bar(T)
    y = STATUS + 8
    e.append(text(GUTTER, y + 11, "近 12 个月", 14, T["muted"]))
    y += 28
    part, y = heatmap(T, grid, y)
    e += part
    part, y = legend(T, y + 8)
    e += part
    e.append(text(GUTTER, y + 8 + 11, line, 14, T["muted"]))
    y += 8 + 14
    if avg:
        # 第二行结论：同一个「活跃天平均」也是 14 次要色，不另起一张卡 —— 它回答的还是
        # 「这一年读成什么样」，与上面那句同一段话。
        e.append(text(GUTTER, y + 20, avg, 14, T["muted"]))
        y += 20 + 14
    if top:
        # 榜单：眉标一行 + 五行「书名 | 时长条 | 时长」。条子的高与圆角直接沿用热力图
        # 那一格的数（`CELL` 8、`CELL_R` 3），颜色沿用 `ramp` 最重那一档 —— 同一屏里
        # 出现第二种条子尺就是自造样式。时长是次要信息，14 号次要色，右缘对齐。
        y += 26
        e.append(text(GUTTER, y, "读得最多的 5 本", 14, T["muted"]))
        y += 14
        widest = max(m for _, m in top) or 1
        for title, minutes in top:
            row_y = y + ROW_H / 2
            label = hours_minutes(minutes)
            assert tw(label, 14) <= W - GUTTER - (BAR_X0 + BAR_W) - 12, \
                f"时长那串字放不下：{label} 要 {tw(label, 14):.0f}"
            e.append(text(GUTTER, row_y + 6, truncate(title, 16, BAR_X0 - GUTTER - 12), 16, T["fg"]))
            e.append(f'<rect x="{BAR_X0}" y="{row_y - CELL / 2:.1f}" width="{BAR_W}" '
                     f'height="{CELL}" rx="{CELL_R}" fill="{T["muted_bg"]}"/>')
            e.append(f'<rect x="{BAR_X0}" y="{row_y - CELL / 2:.1f}" '
                     f'width="{BAR_W * minutes / widest:.1f}" height="{CELL}" rx="{CELL_R}" '
                     f'fill="{CHART_SOLID}"/>')
            e.append(text(W - GUTTER, row_y + 5, label, 14, T["muted"], anchor="end"))
            y += ROW_H
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


def minutes_of(grid):
    """格子 → 「哪天读了多少分钟」。结论那三句、平均那一行与榜单全部从这一份念出来。"""
    return {(c, r): LEVEL_MINUTES[lv] for c, r, lv in grid if lv}


def readout(grid):
    """结论那三个数由同一份格子念出来：天数、总时长、最长连续。"""
    minutes = minutes_of(grid)
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


def avg_line(grid):
    """「平均每个活跃天」—— 与上面那句同一个分子分母，不另取一份数。

    实现里这一格不用新埋点：`ReadingSummary::average_per_active_day`
    （`folio-core/src/store/types.rs:284`）算的就是总秒数除以活跃天数。
    """
    minutes = minutes_of(grid)
    if not minutes:
        return ""
    return f"平均每个活跃天 {round(sum(minutes.values()) / len(minutes))} 分钟"


def top_books(grid):
    """榜单五本：把格子里的总时长按权重切给 `SHELF` 的前五本（书名取自那一份样例书库）。

    同样零新埋点 —— `Store::top_books`（`folio-core/src/store/reading.rs:214-240`）
    按 `reading_sessions` 分组求和，早就写好了，只是这一屏从没调过它。
    """
    total = sum(minutes_of(grid).values())
    if not total:
        return []
    books = [(title, round(total * w)) for (title, _, _), w in zip(SHELF, TOP_WEIGHTS)]
    return [(title, minutes) for title, minutes in books if minutes > 0]


if __name__ == "__main__":
    OUT.mkdir(parents=True, exist_ok=True)
    grid = cells(random.Random(20261024))
    line, avg, top = readout(grid), avg_line(grid), top_books(grid)
    out = {
        "01-stats-year.svg": stats_screen(
            LIGHT, grid, line, "01-stats-year",
            "统计·近 12 个月：热力图 + 图例 + 两句结论 + 榜单五本，横向要滚",
            avg=avg, top=top),
        # 一年没读：满网格照画、全 0 档，但**平均与榜单都不摆** —— 分母是零，
        # 而「读得最多的 5 本：无」不是一句需要占一屏的话。
        "02-stats-none.svg": stats_screen(
            LIGHT, [], "还没有阅读记录", "02-stats-none",
            "统计·一年没读：满网格照画、全 0 档，只把结论换成那一句，平均与榜单都不摆"),
        "03-stats-no-store.svg": empty_store_screen(LIGHT),
        "04-stats-year-dark.svg": stats_screen(
            DARK, grid, line, "04-stats-year-dark", "统计·近 12 个月的深色对照",
            avg=avg, top=top),
    }
    for name, body in out.items():
        (OUT / name).write_text(body, encoding="utf-8")
    print("done:", len(out), "screens")
    print("     结论：", line, "|", avg)
    print("     榜单：", top)
    render_pngs(OUT)
