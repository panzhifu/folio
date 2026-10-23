#!/usr/bin/env python3
r"""从 CHANGELOG.md 里取出某个版本那一段，当作 GitHub release 的正文。

为什么要有这个脚本而不是在 workflow 里 `sed` 一段：CHANGELOG 的段标题是
`## [0.1.1] - 2026-10-24` 这种形状，`sed` 那一条要么写成一串看不懂的地址算术，
要么写成 `/^## \[/,/^## \[/` 这种**会把下一段也带进去**的区间。两种都会在 CI 里
安静地产出一个正文多了半篇的 release —— 而那正是读者第一眼看到的东西。

规则只有三条，都写在下面的判定里：

1. 段标题必须**逐字符**匹配 `## [<版本>]`，前缀后缀都不算（`[0.1.1-rc1]` 不是 `[0.1.1]`）。
2. 段尾是下一个 `## ` 或文件结束。`###` 小标题不算段尾（那是段内的「新增 / 变更 / …」）。
3. 找不到那一段就**非零退出**。发一个没有正文的 release 比不发更糟：读者会以为这版什么都没改。
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

# `## [0.1.1] - 2026-10-24`；日期那一截可选（起草时还没有）。
SECTION = re.compile(r"^## \[(?P<version>[^\]]+)\](?:[^\n]*)?$")


def extract(text: str, want: str) -> str:
    lines = text.splitlines()
    start: int | None = None
    for index, line in enumerate(lines):
        if start is None:
            match = SECTION.match(line)
            if match and match.group("version") == want:
                start = index + 1
            continue
        # 已经在段内：只有**另一个** `## ` 标题才算段尾。
        if line.startswith("## "):
            return "\n".join(lines[start:index]).strip()
    if start is None:
        available = [
            m.group("version")
            for m in (SECTION.match(line) for line in lines)
            if m is not None
        ]
        raise SystemExit(
            f"CHANGELOG.md 里没有版本 {want!r} 那一段。"
            f"现有的段：{', '.join(available) if available else '（一个都没有）'}"
        )
    # 段一直到文件结束。
    return "\n".join(lines[start:]).strip()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True, help="不带 v 前缀的版本号，如 0.1.1")
    parser.add_argument("--out", type=Path, help="写到这里；省略则写标准输出")
    parser.add_argument("changelog", type=Path, nargs="?", default=Path("CHANGELOG.md"))
    args = parser.parse_args()

    body = extract(args.changelog.read_text(encoding="utf-8"), args.version.lstrip("v"))
    if args.out is None:
        sys.stdout.write(body + "\n")
    else:
        args.out.write_text(body + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
