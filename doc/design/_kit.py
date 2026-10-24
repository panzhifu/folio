#!/usr/bin/env python3
"""Folio 设计稿的共享层：一把尺、两份主题、一张图标表、一批行构造器。

各屏的 `generate.py` 只写「这一屏摆哪几行」，几何与颜色全部从这里取 —— 一个数不许在两份
文件里各写一遍。规矩与 `settings/` 那份定稿一致：**行内不摆灰色说明文字、卡片上方不摆节头**。

每一个数字都必须能在真实代码里找到出处，不许自造样式：

- **几何**来自 `crates/folio-app/src/ui/`（行命中高 44、图标槽 28、组缝 32、卡圆角 8、
  顶栏带 44、底栏 56、Switch 44×24），逐条理由写在使用它的那一屏的文件头注释里；
- **颜色**是 gpui-kit `Default Light` / `Default Dark` 的 token 值（shadcn neutral 系）；
- **图标**抄 `gpui-kit-assets` 里 Lucide 的原 path（24 网格、stroke 2）；
- **文案**取 `crates/folio-core/locales/zh-CN.toml` 的现译。

加图标不要手抄 path —— 从注册表里的原 SVG 抽：

    python3 - <<'PY'
    import xml.etree.ElementTree as ET, pathlib
    p = pathlib.Path.home() / ".cargo/registry/src"
    # 见 assets/icons/<name>.svg，取 path/line/circle/rect 的 attrib
    PY

画布 384×853 是魅族 20（1080×2400 @ 450dpi）的逻辑分辨率；其它设备按同一逻辑网格缩放。
"""

from pathlib import Path
import base64
import shutil
import subprocess

W, H = 384, 853
STATUS = 28          # 系统状态栏高度（逻辑）
TOPBAR = 44          # TopBar 带高
BAR = 56             # 底部导航栏高（shell/navigation/bottom_bar.rs 的 BAR_HEIGHT）
ROW = 44             # 单行行高（可命中），compact.rs 的 ROW_HIT
ICON_SLOT = 28       # 行首图标槽，compact.rs 的 size_7
PAD = 16             # 卡片横向内缩 / 屏幕边距（px_4）
CARD_R = 8           # 分组卡圆角（theme.radius_lg）
GAP_SECT = 32        # 卡与卡的缝（mt_8）
FONT = "Noto Sans CJK SC"
# 真机渲染分辨率：魅族 20 是 1080×2400 @ 450dpi，2.8125x 正好把 384×853 铺满。
PX_W, PX_H = 1080, 2400

# 内容右缘：卡右(368) − px_4(16)。所有右对齐的控件都从这条线往回排。
RIGHT = W - PAD - PAD

# 软件图标：仓库里那一份真资产。稿子里要摆图标本体而不是线描占位，所以直接嵌 data URI。
ICON_PNG = Path(__file__).resolve().parents[2] / "packaging" / "icons" / "folio.png"
ICON_URI = ("data:image/png;base64," + base64.b64encode(ICON_PNG.read_bytes()).decode()) \
    if ICON_PNG.exists() else None

LIGHT = {
    "bg": "#ffffff", "fg": "#0a0a0a", "muted": "#737373", "border": "#e5e5e5",
    "primary": "#171717", "primary_fg": "#fafafa",
    "switch_off": "#e5e5e5", "knob_off": "#ffffff", "knob_on": "#fafafa",
    "btn_bg": "#f5f5f5", "punch_ring": "#d4d4d4",
    # muted.background（neutral-100）—— 书卡占位格、当前项整行底色都取它。
    # 注意与上面那格 `muted` 不是一回事：那一个是 muted.foreground，是**文字**色。
    "muted_bg": "#f5f5f5", "popover": "#ffffff",
    # theme().overlay = #0000000d ⇒ alpha 13/255。满档弹层时 scrim_alpha 是 1.0，
    # 所以浅侧那层遮罩真的只有 5% 黑 —— 这是量出来的，不是画漏了。
    "scrim_a": "0.051",
    "charts": ["#93c5fd", "#3b82f6", "#2563eb", "#1d4ed8", "#1e40af"],
    "label": "浅色 · Default Light",
}
DARK = {
    "bg": "#0a0a0a", "fg": "#fafafa", "muted": "#a3a3a3", "border": "#262626",
    "primary": "#fafafa", "primary_fg": "#171717",
    "switch_off": "#262626", "knob_off": "#a3a3a3", "knob_on": "#171717",
    "btn_bg": "#262626", "punch_ring": "#262626",
    "muted_bg": "#262626", "popover": "#0a0a0a",
    "scrim_a": "0.2",  # theme().overlay = #00000033 ⇒ alpha 51/255
    "charts": ["#93c5fd", "#3b82f6", "#2563eb", "#1d4ed8", "#1e40af"],
    "label": "深色 · Default Dark",
}

# ── Lucide 图标（抄自 gpui-kit-assets 0.7.1，24 网格，stroke 2） ───────────────
ICONS = {
    "palette": {"p": ['M12 22a1 1 0 0 1 0-20 10 9 0 0 1 10 9 5 5 0 0 1-5 5h-2.25a1.75 1.75 0 0 0-1.4 2.8l.3.4a1.75 1.75 0 0 1-1.4 2.8z'],
                "dots": [(13.5, 6.5), (17.5, 10.5), (6.5, 12.5), (8.5, 7.5)]},
    "type": {"p": ['M12 4v16', 'M4 7V5a1 1 0 0 1 1-1h14a1 1 0 0 1 1 1v2', 'M9 20h6']},
    "columns-3": {"rect": (3, 3, 18, 18, 2), "lines": ['M9 3v18', 'M15 3v18']},
    "a-large-small": {"p": ['m15 16 2.536-7.328a1.02 1.02 1 0 1 1.928 0L22 16', 'M15.697 14h5.606',
                            'm2 16 4.039-9.69a.5.5 0 0 1 .923 0L11 16', 'M3.304 13h6.392']},
    "folder-open": {"p": ['m6 14 1.5-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.54 6a2 2 0 0 1-1.95 1.5H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.9a2 2 0 0 1 1.69.9l.81 1.2a2 2 0 0 0 1.67.9H18a2 2 0 0 1 2 2v2']},
    "list-tree": {"p": ['M8 5h13', 'M13 12h8', 'M13 19h8', 'M3 10a2 2 0 0 0 2 2h3', 'M3 5v12a2 2 0 0 0 2 2h3']},
    "hard-drive": {"p": ['M10 16h.01', 'M2.212 11.577a2 2 0 0 0-.212.896V18a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-5.527a2 2 0 0 0-.212-.896L18.55 5.11A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z', 'M21.946 12.013H2.054', 'M6 16h.01']},
    "info": {"p": ['M12 16v-4', 'M12 8h.01'], "circles": [(12, 12, 10)]},
    "chart-column": {"p": ['M3 3v16a2 2 0 0 0 2 2h16', 'M18 17V9', 'M13 17V5', 'M8 17v-3']},
    "notebook-pen": {"p": ['M13.4 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-7.4', 'M2 6h4', 'M2 10h4', 'M2 14h4', 'M2 18h4',
                           'M21.378 5.626a1 1 0 1 0-3.004-3.004l-5.01 5.012a2 2 0 0 0-.506.854l-.837 2.87a.5.5 0 0 0 .62.62l2.87-.837a2 2 0 0 0 .854-.506z']},
    "circle-user": {"p": ['M7 20.662V19a2 2 0 0 1 2-2h6a2 2 0 0 1 2 2v1.662'], "circles": [(12, 12, 10), (12, 10, 3)]},
    "library": {"p": ['m16 6 4 14', 'M12 6v14', 'M8 8v12', 'M4 4v16']},
    "chevron-right": {"p": ['m9 18 6-6-6-6']},
    "chevron-left": {"p": ['m15 18-6-6 6-6']},
    "chevron-down": {"p": ['m6 9 6 6 6-6']},
    "check": {"p": ['M20 6 9 17l-5-5']},
    "minus": {"p": ['M5 12h14']},
    "plus": {"p": ['M5 12h14', 'M12 5v14']},
    "wifi": {"p": ['M12 20h.01', 'M2 8.82a15 15 0 0 1 20 0', 'M5 12.859a10 10 0 0 1 14 0', 'M8.5 16.429a5 5 0 0 1 7 0']},
    "signal": {"p": ['M2 20h.01', 'M7 20v-4', 'M12 20v-8', 'M17 20V8']},
    "broom": {"p": ['M13.5 10.5 22 2', 'M14.734 13.841a2 2 0 00-.314-2.42L12.58 9.58a2 2 0 00-2.421-.314l-7.657 4.461A1 1 0 002.3 15.3l6.403 6.403a1 1 0 001.571-.204z', 'm5 18 2-2', 'm7.699 10.7 5.602 5.601']},
    "database": {"p": ['M3 5V19A9 3 0 0 0 21 19V5', 'M3 12A9 3 0 0 0 21 12'], "ellipses": [(12, 5, 9, 3)]},
    # ── 书架 / 阅读 / 统计 / 欢迎 那几屏要用的，同一次从 gpui-kit-assets 抽 ──
    "search": {"p": ['m21 21-4.34-4.34'], "circles": [(11, 11, 8)]},
    "search-x": {"p": ['m13.5 8.5-5 5', 'm8.5 8.5 5 5', 'm21 21-4.3-4.3'], "circles": [(11, 11, 8)]},
    "ellipsis": {"dots": [(12, 12), (19, 12), (5, 12)]},
    "ellipsis-vertical": {"dots": [(12, 12), (12, 5), (12, 19)]},
    "link": {"p": ['M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71', 'M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71']},
    "scan-search": {"p": ['M3 7V5a2 2 0 0 1 2-2h2', 'M17 3h2a2 2 0 0 1 2 2v2', 'M21 17v2a2 2 0 0 1-2 2h-2', 'M7 21H5a2 2 0 0 1-2-2v-2', 'm16 16-1.9-1.9'], "circles": [(12, 12, 3)]},
    "trash": {"p": ['M10 11v6', 'M14 11v6', 'M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6', 'M3 6h18', 'M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2']},
    "copy": {"rect": (8, 8, 14, 14, 2), "p": ['M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2']},
    "download": {"p": ['M12 15V3', 'M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4', 'm7 10 5 5 5-5']},
    "map-pin": {"p": ['M20 10c0 4.993-5.539 10.193-7.399 11.799a1 1 0 0 1-1.202 0C9.539 20.193 4 14.993 4 10a8 8 0 0 1 16 0'], "circles": [(12, 10, 3)]},
    "sticky-note": {"p": ['M21 9a2.4 2.4 0 0 0-.706-1.706l-3.588-3.588A2.4 2.4 0 0 0 15 3H5a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2z', 'M15 3v5a1 1 0 0 0 1 1h5']},
    "sliders-horizontal": {"p": ['M10 5H3', 'M12 19H3', 'M14 3v4', 'M16 17v4', 'M21 12h-9', 'M21 19h-5', 'M21 5h-7', 'M8 10v4', 'M8 12H3']},
    "sun": {"p": ['M12 2v2', 'M12 20v2', 'm4.93 4.93 1.41 1.41', 'm17.66 17.66 1.41 1.41', 'M2 12h2', 'M20 12h2', 'm6.34 17.66-1.41 1.41', 'm19.07 4.93-1.41 1.41'], "circles": [(12, 12, 4)]},
    "sun-medium": {"p": ['M12 3v1', 'M12 20v1', 'M3 12h1', 'M20 12h1', 'm18.364 5.636-.707.707', 'm6.343 17.657-.707.707', 'm5.636 5.636.707.707', 'm17.657 17.657.707.707'], "circles": [(12, 12, 4)]},
    "moon": {"p": ['M20.985 12.486a9 9 0 1 1-9.473-9.472c.405-.022.617.46.402.803a6 6 0 0 0 8.268 8.268c.344-.215.825-.004.803.401']},
    "refresh-cw": {"p": ['M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8', 'M21 3v5h-5', 'M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16', 'M8 16H3v5']},
    "chevron-up": {"p": ['m18 15-6-6-6 6']},
    "bookmark": {"p": ['M17 3a2 2 0 0 1 2 2v15a1 1 0 0 1-1.496.868l-4.512-2.578a2 2 0 0 0-1.984 0l-4.512 2.578A1 1 0 0 1 5 20V5a2 2 0 0 1 2-2z']},
    "arrow-left": {"p": ['m12 19-7-7 7-7', 'M19 12H5']},
    "file-text": {"p": ['M6 22a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h8a2.4 2.4 0 0 1 1.704.706l3.588 3.588A2.4 2.4 0 0 1 20 8v12a2 2 0 0 1-2 2z', 'M14 2v5a1 1 0 0 0 1 1h5', 'M10 9H8', 'M16 13H8', 'M16 17H8']},
    "clock-3": {"p": ['M12 6v6h4'], "circles": [(12, 12, 10)]},
    "flame": {"p": ['M12 3q1 4 4 6.5t3 5.5a1 1 0 0 1-14 0 5 5 0 0 1 1-3 1 1 0 0 0 5 0c0-2-1.5-3-1.5-5q0-2 2.5-4']},
    "highlighter": {"p": ['m9 11-6 6v3h9l3-3', 'm22 12-4.6 4.6a2 2 0 0 1-2.8 0l-5.2-5.2a2 2 0 0 1 0-2.8L14 4']},
    "pen-line": {"p": ['M13 21h8', 'M21.174 6.812a1 1 0 0 0-3.986-3.987L3.842 16.174a2 2 0 0 0-.5.83l-1.321 4.352a.5.5 0 0 0 .623.622l4.353-1.32a2 2 0 0 0 .83-.497z']},
    "folder-plus": {"p": ['M12 10v6', 'M9 13h6', 'M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z']},
    "upload": {"p": ['M12 3v12', 'm17 8-5-5-5 5', 'M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4']},
    "share-2": {"lines": ['M8.59 13.51L15.42 17.49', 'M15.41 6.51L8.59 10.49'], "circles": [(18, 5, 3), (6, 12, 3), (18, 19, 3)]},
    "book-open": {"p": ['M12 5v16', 'M20.001 19A2 2 0 0022 17V5a2 2 0 00-1.999-2L16 3.002A5 5 0 0012 5a5 5 0 00-4-2H4a2 2 0 00-2 2v12a2 2 0 001.999 2H8a5 5 0 014 2 5 5 0 014-2z']},
    "x": {"p": ['M18 6 6 18', 'm6 6 12 12']},
    "list": {"p": ['M3 5h.01', 'M3 12h.01', 'M3 19h.01', 'M8 5h13', 'M8 12h13', 'M8 19h13']},
    "settings-2": {"p": ['M14 17H5', 'M19 7h-9'], "circles": [(17, 17, 3), (7, 7, 3)]},
    "image": {"rect": (3, 3, 18, 18, 2), "p": ['m21 15-3.086-3.086a2 2 0 0 0-2.828 0L6 21'],
              "circles": [(9, 9, 2)]},
    "circle-check": {"p": ['m16 9-5.5 5.5L8 12'], "circles": [(12, 12, 10)]},
    "wifi-off": {"p": ['M12 20h.01', 'M8.5 16.429a5 5 0 0 1 7 0', 'M5 12.859a10 10 0 0 1 5.17-2.69', 'M19 12.859a10 10 0 0 0-2.007-1.523', 'M2 8.82a15 15 0 0 1 4.177-2.643', 'M22 8.82a15 15 0 0 0-11.288-3.764', 'm2 2 20 20']},
}


def icon(name, x, y, size, color):
    s = size / 24.0
    parts = [f'<g transform="translate({x:.2f},{y:.2f}) scale({s:.5f})" fill="none" stroke="{color}" '
             f'stroke-width="2" stroke-linecap="round" stroke-linejoin="round">']
    spec = ICONS[name]
    if "rect" in spec:
        rx, ry, rw, rh, rr = spec["rect"]
        parts.append(f'<rect x="{rx}" y="{ry}" width="{rw}" height="{rh}" rx="{rr}"/>')
    for d in spec.get("p", []):
        parts.append(f'<path d="{d}"/>')
    for line in spec.get("lines", []):
        parts.append(f'<path d="{line}"/>')
    for cx, cy, r in spec.get("circles", []):
        parts.append(f'<circle cx="{cx}" cy="{cy}" r="{r}"/>')
    for cx, cy, rx, ry in spec.get("ellipses", []):
        parts.append(f'<ellipse cx="{cx}" cy="{cy}" rx="{rx}" ry="{ry}"/>')
    for cx, cy in spec.get("dots", []):
        parts.append(f'<circle cx="{cx}" cy="{cy}" r="1" fill="{color}" stroke="none"/>')
    parts.append("</g>")
    return "".join(parts)


def tw(s, size):
    """字宽估算：CJK 一字一号，拉丁平均 0.56 号。"""
    return sum(size if ord(c) > 0x2E80 else size * 0.56 for c in s)


def truncate(s, size, max_w):
    """放不下就截成省略号 —— 实现里 min_w_0 + text_ellipsis 干的是同一件事。"""
    if tw(s, size) <= max_w:
        return s
    while s and tw(s + "…", size) > max_w:
        s = s[:-1]
    return s + "…"


def text(x, y, s, size, color, weight=400, anchor="start"):
    return (f'<text x="{x}" y="{y}" font-family="{FONT}" font-size="{size}" font-weight="{weight}" '
            f'fill="{color}" text-anchor="{anchor}">{s}</text>')


def hairline(y, x0, x1, T):
    return f'<rect x="{x0}" y="{y}" width="{x1 - x0}" height="0.5" fill="{T["border"]}"/>'


def status_bar(T):
    e = [f'<rect x="0" y="0" width="{W}" height="{STATUS}" fill="{T["bg"]}"/>']
    e.append(text(16, STATUS / 2 + 4.5, "12:30", 13, T["fg"], 500))
    e.append(f'<circle cx="{W / 2}" cy="14" r="5.5" fill="#000" stroke="{T["punch_ring"]}" stroke-width="1"/>')
    r = W - PAD
    e.append(icon("wifi", r - 60, 7.5, 14, T["fg"]))
    e.append(icon("signal", r - 40, 7.5, 14, T["fg"]))
    e.append(f'<rect x="{r - 24}" y="8" width="18" height="10" rx="2.5" fill="none" stroke="{T["fg"]}" stroke-width="1.6"/>')
    e.append(f'<rect x="{r - 22}" y="10" width="12" height="6" rx="1" fill="{T["fg"]}"/>')
    e.append(f'<rect x="{r - 4.5}" y="11" width="2" height="4" rx="1" fill="{T["fg"]}"/>')
    return e


def top_bar(title, T):
    e = []
    cy = STATUS + 16
    e.append(icon("chevron-left", PAD + 6, cy - 10, 20, T["fg"]))
    e.append(text(PAD + 40, cy + 5.5, title, 16, T["fg"], 600))
    return e


def card(x, y, w, h, T):
    return (f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{CARD_R}" '
            f'fill="{T["bg"]}" stroke="{T["border"]}" stroke-width="1"/>')


def bottom_bar(T, active="我的"):
    tabs = [("library", "书架"), ("chart-column", "统计"), ("notebook-pen", "笔记"), ("circle-user", "我的")]
    e = [f'<rect x="0" y="{H - BAR}" width="{W}" height="{BAR}" fill="{T["bg"]}"/>']
    for i, (ic, label) in enumerate(tabs):
        cx = (i + 0.5) * W / 4
        on = label == active
        ink = T["primary"] if on else T["muted"]
        e.append(icon(ic, cx - 11, H - BAR + 7, 22, ink))
        e.append(text(cx, H - BAR + 44, label, 12, ink, anchor="middle"))
    return e


# ── 屏级构件：遮罩、弹层、按钮、输入框、封面格 ─────────────────────────────────
#
# 这一批的数全部来自实现，出处写在各自的注释里；只有设置那一档用得着的行构造器
# 留在下面的「行构造器」一节，不进这一节。

RADIUS = 6           # theme().radius —— Button 的圆角、Input 的圆角、封面格 rounded_md 都是它
RADIUS_SM = 3        # theme().radius_sm = radius / 2
BUTTON_H = 32        # Button 的默认档 Size::Medium → h_8；Input 的默认高同为 32
HANDLE_BAND = 36     # 弹层顶部可拖那一条带（components/bottom_sheet.rs 的 HANDLE_BAND）
HANDLE_W, HANDLE_H = 32.0, 4.0


def wrap_cjk(s, size, max_w, max_lines=99):
    """按字宽贪心折行。中日韩没有空格可断，实现里也是逐字量的，这里同形。"""
    lines, cur = [], ""
    for ch in s:
        if cur and tw(cur + ch, size) > max_w:
            lines.append(cur)
            cur = ch
            if len(lines) == max_lines:
                return lines
        else:
            cur += ch
    if cur and len(lines) < max_lines:
        lines.append(cur)
    return lines[:max_lines]


def scrim(T, y0=0.0, y1=H):
    """弹层背后那一层：theme().overlay 的 alpha × scrim_alpha（满档时是 1.0）。
    浅侧只有 5% 黑 —— 这是量出来的，不是画漏了。"""
    return (f'<rect x="0" y="{y0}" width="{W}" height="{y1 - y0}" '
            f'fill="#000000" fill-opacity="{T["scrim_a"]}"/>')


def app_icon(x, y, size):
    """软件图标本体：仓库里那一份真资产（`packaging/icons/folio.png`，192×192，
    自带圆角方块底板），按 data URI 嵌进来。

    画一个自造的"像那么回事"的图标是不诚实的 —— 稿子的规矩是每一格都有出处。
    拿不到那个文件时回落到 `book-open` 那枚线性图标并出声：设计稿生成器不该因为
    少一份资产就红，但也不能悄悄换个东西顶上。
    """
    if ICON_URI is None:
        print(f"缺 {ICON_PNG}，这一格回落到 book-open")
        return icon("book-open", x, y, size, "#0a0a0a")
    return (f'<image x="{x:.2f}" y="{y:.2f}" width="{size}" height="{size}" '
            f'href="{ICON_URI}"/>')


def sheet(T, ratio):
    """底部弹层外壳：高 = 视口 × ratio，只圆上两角（radius_lg），带把手那一条带。

    返回 (元素表, 面板顶 y)。内容从 `顶 y + HANDLE_BAND` 往下摆。
    """
    height = H * ratio
    y = H - height
    e = [f'<rect x="0" y="{y:.2f}" width="{W}" height="{height:.2f}" '
         f'rx="{CARD_R}" fill="{T["popover"]}"/>',
         # SVG 的 rx 会把下沿一起圆掉，而实现里 bottom_left/right 是 0 —— 补一方块。
         f'<rect x="0" y="{y + CARD_R:.2f}" width="{W}" height="{height - CARD_R - 0.01:.2f}" '
         f'fill="{T["popover"]}"/>',
         f'<rect x="{(W - HANDLE_W) / 2:.2f}" y="{y + (HANDLE_BAND - HANDLE_H) / 2:.2f}" '
         f'width="{HANDLE_W}" height="{HANDLE_H}" rx="{HANDLE_H / 2}" fill="{T["border"]}"/>']
    return e, y


def button(T, x, y, label, kind="secondary", icon_name=None, w=None, anchor="left"):
    """一颗按钮：高 32、圆角 6、横向内缩 10、图标 20 与字之间 8、字 16。

    `kind` 取 primary（底 primary / 字 primary_fg）、secondary 与 ghost（底 btn_bg）、
    outline（透明 + 描边）。返回 (元素表, 宽)，调用方据此排下一颗。

    给了 `w` 就是**定宽那颗**：内容在盒子里居中 —— 组件的按钮内容是一条
    `size_full().items_center().justify_center()` 的 h_flex，撑满时字不会靠左。
    """
    inner = 28 if icon_name else 0
    natural = 10 + inner + tw(label, 16) + 10
    bw = w if w is not None else natural
    if anchor == "right":
        x = x - bw
    fill = {"primary": T["primary"], "secondary": T["btn_bg"],
            "ghost": T["btn_bg"], "outline": "none"}[kind]
    ink = T["primary_fg"] if kind == "primary" else T["fg"]
    stroke = f' stroke="{T["border"]}" stroke-width="1"' if kind == "outline" else ""
    e = [f'<rect x="{x:.2f}" y="{y}" width="{bw:.2f}" height="{BUTTON_H}" '
         f'rx="{RADIUS}" fill="{fill}"{stroke}/>']
    cx = x + (bw - (inner + tw(label, 16))) / 2 if w is not None else x + 10
    if icon_name:
        e.append(icon(icon_name, cx, y + 6, 20, ink))
        cx += 28
    e.append(text(cx, y + BUTTON_H / 2 + 5.5, label, 16, ink))
    return e, bw


def input_box(T, x, y, w, value="", placeholder=None, h=BUTTON_H):
    """输入框：高 32、圆角 6、描 border、内缩 10、字 14（Input 走 text_sm）。"""
    e = [f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{RADIUS}" fill="none" '
         f'stroke="{T["border"]}" stroke-width="1"/>']
    body = value or placeholder or ""
    if body:
        ink = T["fg"] if value else T["muted"]
        e.append(text(x + 10, y + h / 2 + 5, truncate(body, 14, w - 20), 14, ink))
    return e


def search_box(T, x, y, w, value="", placeholder=None):
    """带放大镜前缀的搜索框，**框里有词时右端多一枚清除钮**。

    三处的数都来自组件本身，不是挑了个顺眼的：

    - 前缀 `Icon::Search.small()` = 12 档里的 `size_3p5` = **14**，落在容器左内缩
      `input_px()` = **10**（Medium 那一档，`sizing.rs:147-155`）处；
    - 前缀与文字之间是 `gap_x` = **6**（Medium，`input.rs:512-516`），所以文字从
      `x + 10 + 14 + 6 = x + 30` 起排；
    - 清除钮是 `clear_button()` = `Button.xsmall()` 包一枚 `IconName::Close`
      （`input/clear_button.rs:9-15`）。XSmall 那一档钮高 `h_5` = **20**、内缩 `px_1` = 4，
      图标 `size_3` = **12**（`icon.rs:183`）⇒ 钮盒 4+12+4 = 20 宽；整条 suffix 挂在
      `pr(input_px())` 上，所以**钮的右缘落在 `x + w − 10`**。

    这颗钮只在 `state.text(cx).len() > 0` 时才在（`input.rs:531-535`）—— 空框不摆，
    所以它同时也是「这一屏正在搜索」的标记。点它走 `state.clean()` 再把焦点放回框里，
    词清空后书架跟着回到满架那一片。
    """
    e = [f'<rect x="{x}" y="{y}" width="{w}" height="{BUTTON_H}" rx="{RADIUS}" fill="none" '
         f'stroke="{T["border"]}" stroke-width="1"/>',
         icon("search", x + 10, y + 9, 14, T["muted"])]
    body = value or placeholder or ""
    right = x + w - 10
    if value:
        # 清除钮：20 宽的钮盒贴右内缩，12 的 x 居中在盒里。
        bx = right - 20
        e.append(icon("x", bx + 4, y + BUTTON_H / 2 - 6, 12, T["muted"]))
        right = bx - 6          # 文字在钮左侧 6 那一条缝前截住
    if body:
        ink = T["fg"] if value else T["muted"]
        e.append(text(x + 30, y + BUTTON_H / 2 + 5, truncate(body, 14, right - (x + 30)),
                      14, ink))
    return e


def book_cover(T, x, y, w, h, title=None, art=None):
    """一格封面：圆角 6、底色 `muted.background`。

    没有封面图时把书名画在格子里（居中、次要前景色）—— 与实现里那条「没有封面的书也要
    照样占住一格」同一处理；最多三行，放不下截省略号。
    """
    e = [f'<rect x="{x:.2f}" y="{y:.2f}" width="{w:.2f}" height="{h:.2f}" '
         f'rx="{RADIUS}" fill="{T["muted_bg"]}"/>']
    if art:
        e.append(art)
    elif title:
        lines = wrap_cjk(title, 16, w - 16, 3)
        lh = 22.0
        first = y + h / 2 - (len(lines) - 1) * lh / 2 + 5.5
        for i, line in enumerate(lines):
            e.append(text(x + w / 2, first + i * lh, truncate(line, 16, w - 16),
                          16, T["muted"], anchor="middle"))
    return e


# ── 行构造器：都按 y=0 造，落卡时整体平移。除注明外行高都是 44。 ────────────────

def r_title(s, x=None):
    """单行行的标题：行都按 y=0 造，基线落在 44 行的视觉中心。"""
    return text(x or PAD + PAD, ROW / 2 + 5.5, s, 16, "__FG__")


def push_row(title, value=None, chevron=True, icon_name=None):
    """「进去还有一屏 / 就地显示当前值」：标题 + 可选当前值 + 箭头。"""
    e = []
    cy = ROW / 2
    tx = PAD + PAD
    if icon_name:
        e.append(icon(icon_name, tx + 6, cy - 8, 16, "__FG__"))
        tx += ICON_SLOT + 12
    e.append(text(tx, cy + 5.5, title, 16, "__FG__"))
    if value is not None:
        e.append(text(RIGHT - 14 - 12, cy + 5.5, value, 16, "__MUTED__", anchor="end"))
    if chevron:
        e.append(icon("chevron-right", RIGHT - 14, cy - 7, 14, "__MUTED__"))
    return e, ROW


def choice_row(title, checked):
    """选项屏的一行：勾是唯一的右端标记。"""
    e = [text(PAD + PAD, ROW / 2 + 5.5, title, 16, "__FG__")]
    if checked:
        e.append(icon("check", RIGHT - 14, ROW / 2 - 7, 14, "__PRIMARY__"))
    return e, ROW


def stepper_row(title, value):
    """数值步进行：标题 | 值(右缘 272) | − 框 280..312 | + 框 320..352。"""
    e = [r_title(title)]
    e.append(text(RIGHT - 80, ROW / 2 + 5.5, value, 16, "__FG__", anchor="end"))
    for name, bx in (("minus", RIGHT - 72), ("plus", RIGHT - 32)):
        e.append(icon(name, bx + 6, ROW / 2 - 10, 20, "__FG__"))
    return e, ROW


def switch_row(title, on):
    e = [r_title(title), switch_ctl(RIGHT - 22, ROW / 2, on)]
    return e, ROW


def switch_ctl(cx_right_center, cy, on):
    """Switch::large()：44×24，knob 18。cx 是中心。"""
    x, y = cx_right_center - 22, cy - 12
    track = "__PRIMARY__" if on else "__SW_OFF__"
    knob = "__KNOB_ON__" if on else "__KNOB_OFF__"
    kx = x + 44 - 12 if on else x + 12
    return (f'<rect x="{x}" y="{y}" width="44" height="24" rx="12" fill="{track}"/>'
            f'<circle cx="{kx}" cy="{cy}" r="9" fill="{knob}"/>')


def popup_row(title, label):
    """就地浮层那档下拉：右端一颗 outline 小按钮（标签 + 下箭头），高 28。"""
    e = [r_title(title)]
    bw = 12 + tw(label, 14) + 6 + 14 + 12
    bx = RIGHT - bw
    e.append(f'<rect x="{bx}" y="{ROW / 2 - 14}" width="{bw:.1f}" height="28" rx="6" '
             f'fill="none" stroke="__BORDER__" stroke-width="1"/>')
    e.append(text(bx + 12, ROW / 2 + 4.5, label, 14, "__FG__"))
    e.append(icon("chevron-down", bx + 12 + tw(label, 14) + 6, ROW / 2 - 7, 14, "__FG__"))
    return e, ROW


def ghost_button(label, icon_name, right):
    """ghost 小按钮：图标 14 + 14px 标签，无底无框，返回 (元素, 左缘)。"""
    bw = 8 + 14 + 6 + tw(label, 14) + 8
    bx = right - bw
    e = [icon(icon_name, bx + 8, ROW / 2 - 7, 14, "__FG__"),
         text(bx + 8 + 20, ROW / 2 + 4.5, label, 14, "__FG__")]
    return e, bx


def clean_row(title, bytes_text, icon_name):
    """存储·清理的一行：名字 | 字节(右对齐在钮左侧 72 列) | ghost 小钮。"""
    e = [r_title(title)]
    btn, bx = ghost_button("清掉", icon_name, RIGHT)
    e += btn
    e.append(text(bx - 12, ROW / 2 + 5.5, bytes_text, 16, "__FG__", anchor="end"))
    return e, ROW


def text_row(title, value=""):
    """自由文本：标题在上，输入框整宽在下。78 高。"""
    e = [text(PAD + PAD, 24, title, 16, "__FG__"),
         f'<rect x="{PAD + PAD}" y="34" width="{RIGHT - PAD - PAD}" height="36" rx="6" '
         f'fill="none" stroke="__BORDER__" stroke-width="1"/>']
    if value:
        e.append(text(PAD + PAD + 12, 57, value, 16, "__FG__"))
    return e, 78


def caption_value_row(caption, value):
    """「建议位置」这类行：小灰标 + 值（放不下截成省略号）。64 高。"""
    return [text(PAD + PAD, 22, caption, 14, "__MUTED__"),
            text(PAD + PAD, 48, truncate(value, 16, RIGHT - PAD - PAD), 16, "__FG__")], 64


def titled_row(title, content):
    """标题在上、一行内容在下（内容画在 0..320 的局部坐标里，落位时平移 32）。70 高。"""
    e = [text(PAD + PAD, 24, title, 16, "__FG__")]
    inner, h = content
    e.append(f'<g transform="translate({PAD + PAD},{34 + (28 - h) / 2:.1f})">{"".join(inner)}</g>')
    return e, 34 + 28 + 8


def content_path_button(path):
    """titled_row 的内容：灰色路径（放不下截成省略号）+ 「打开所在文件夹」静默钮。"""
    bw = 20 + tw("打开所在文件夹", 14)
    bx = RIGHT - PAD - PAD - bw
    shown = truncate(path, 14, bx - 12)
    return [text(0, 18.5, shown, 14, "__MUTED__"),
            f'<rect x="{bx}" y="0" width="{bw:.1f}" height="28" rx="6" fill="__BTN_BG__"/>',
            text(bx + 10, 18.5, "打开所在文件夹", 14, "__FG__")], 28


def content_status_ghost(status, btn_label, btn_icon=None):
    """titled_row 的内容：灰色状态字 + ghost 小钮（关于页三行）。"""
    bw = 8 + (20 if btn_icon else 0) + tw(btn_label, 14) + 8
    bx = RIGHT - PAD - PAD - bw
    inner = [text(0, 18.5, status, 14, "__MUTED__"),
             text(bx + 8 + (20 if btn_icon else 0), 18.5, btn_label, 14, "__FG__")]
    if btn_icon:
        inner.insert(1, icon(btn_icon, bx + 8, 7, 14, "__FG__"))
    return inner, 28


def content_button_leading(label):
    """独占一行的一颗静默钮（选择文件夹…），贴内容左缘。"""
    bw = 20 + tw(label, 14)
    return ([f'<rect x="{PAD + PAD}" y="8" width="{bw:.1f}" height="28" rx="6" fill="__BTN_BG__"/>',
             text(PAD + PAD + 10, 26.5, label, 14, "__FG__")], 44)


# ── 卡片装配 ──────────────────────────────────────────────────────────────────

TOKENS = ("__FG__", "fg", "__MUTED__", "muted", "__BORDER__", "border", "__PRIMARY__", "primary",
          "__SW_OFF__", "switch_off", "__KNOB_ON__", "knob_on", "__KNOB_OFF__", "knob_off",
          "__BTN_BG__", "btn_bg", "__PRIMARY_FG__", "primary_fg")


def resolve(els, T):
    """把行构造器留下的颜色占位换成这一侧的真值。"""
    out = []
    for el in els:
        if isinstance(el, str):
            for i in range(0, len(TOKENS), 2):
                el = el.replace(TOKENS[i], T[TOKENS[i + 1]])
        out.append(el)
    return out


def draw_card(e, y, rows, T):
    """在 y 画一张卡：行依次平移落位，行间发丝线（缩进 16），返回下一张卡的 y。"""
    h = sum(rh for _, rh in rows) + (len(rows) - 1) * 0.5
    e.append(card(PAD, y, W - 2 * PAD, h, T))
    ry = y
    for i, (row, rh) in enumerate(rows):
        if i:
            e.append(hairline(ry, PAD + 1 + PAD, W - PAD - 1, T))
        e.append('<g transform="translate(0,{:.2f})">{}</g>'.format(ry, "".join(resolve(row, T))))
        ry += rh + 0.5
    return ry + GAP_SECT


def page_screen(T, title, cards):
    """子页骨架：状态栏 + 顶栏 + 一串无节头的卡。"""
    e = status_bar(T)
    e += top_bar(title, T)
    y = STATUS + TOPBAR + 8
    for rows in cards:
        y = draw_card(e, y, rows, T)
    return e


def svg(name, body, label, note, T, area="设置"):
    head = (f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" '
            f'viewBox="0 0 {W} {H}">\n'
            f'<title>Folio {area} · {label}</title>\n'
            f'<desc>{note}</desc>\n'
            f'<rect width="{W}" height="{H}" fill="{T["bg"]}"/>\n')
    return head + "\n".join(body) + "\n</svg>\n"


def render_pngs(out):
    tool = shutil.which("rsvg-convert")
    if not tool:
        print("rsvg-convert 不在 PATH，PNG 跳过（装 librsvg 后重跑可补）")
        return
    for svg_path in sorted(Path(out).glob("*.svg")):
        png = svg_path.with_suffix(".png")
        subprocess.run([tool, "-w", str(PX_W), "-h", str(PX_H), str(svg_path), "-o", str(png)],
                       check=True)
        print("png :", png.name)
