#!/usr/bin/env python3
"""生成 wslc-panel 的图标（`assets/wslc-panel.ico` + 一张预览 png）。

# 为什么要有这个脚本

图标是**画出来的**，不是找来的：

- 参考项目（`E:\\cnb\\wsl-dashboard-ref`）的图标是 **GPL-3.0-only**，
  按 `AGENTS.md` §6 一行都不能进本仓库；
- 也不能随手拿个网图 —— 许可证说不清。

所以用 Pillow 把图形**程序化**画出来，脚本就是图形的源码，
改配色 / 改形状都只改它再重跑。

用法：`python assets/make-icon.py`（在仓库根执行；依赖 Pillow）

# 设计：一个字形，不是一整个界面

**这是第三版，前两版都被判"丑"，值得把教训写下来：**

| 版本 | 画了什么 | 为什么不行 |
|---|---|---|
| v1 | 三块堆叠的集装箱横条 | 读起来像"汉堡菜单"或数据库图标，不像一个面板 |
| v2 | 深色底 + 面板窗口 + 侧边栏 + 三行列表 | ① 深底在**暗色任务栏**上糊成一片（实机截图确认）；② 信息密度太高，32px 以下碎成一团 |
| **v3** | **亮蓝底 + 白色 `>_`** | 见下 |

关键教训是**信息密度**：在图标里画"面板 + 侧边栏 + 三行列表"，
在 16/32px 上必然退化成几个色块。隔壁那排好认的图标（Outlook 的信封、
bilibili 的电视、dev-sidecar 的分子）都是**一个大而粗的形状**。

# 现在这个图形

**亮蓝渐变底 + 白色 `>_` 提示符。**

- `>_` 是这个程序本质的直接表达：它是 `wslc.exe` / `wsl.exe` 的图形外壳；
- 只有两个笔画（一条折线 + 一个圆点），**16px 下仍然成立**；
- **亮底**：Windows 任务栏默认是深色，深底图标会融进背景（这是 v2 被否掉
  的直接原因）。参考那排成功的图标也全是亮底。
- 四周留白：箭头没有顶到边，缩小后不会和圆角打架。

# 尺寸

7 档 16/24/32/48/64/128/256 —— Windows 按 DPI 与场景挑最合适的那层，
缺哪层就缩放（会糊）。
"""

from pathlib import Path

from PIL import Image, ImageDraw

SIZES = [16, 24, 32, 48, 64, 128, 256]

# 亮蓝渐变。上面亮、下面略暗，给平涂的形状一点体积感；
# 色相不动，只是明度差 —— 换色相会让白色字形的对比度在某一端塌掉。
BG_TOP = (59, 130, 246)     # #3B82F6
BG_BOTTOM = (29, 78, 216)   # #1D4ED8
WHITE = (255, 255, 255)


def _lerp(a, b, t):
    return tuple(round(x + (y - x) * t) for x, y in zip(a, b))


def _tile(size):
    """竖向渐变的圆角方块（画布）。"""
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    grad = Image.new("RGBA", (size, size))
    d = ImageDraw.Draw(grad)
    for y in range(size):
        d.line([(0, y), (size, y)], fill=_lerp(BG_TOP, BG_BOTTOM, y / max(size - 1, 1)) + (255,))
    mask = Image.new("L", (size, size), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        [0, 0, size - 1, size - 1],
        radius=max(2, round(size * 0.23)),
        fill=255,
    )
    img.paste(grad, (0, 0), mask)
    return img


def _stroke(d, points, width, color=WHITE):
    """粗折线 + **圆头**。

    PIL 的 `line` 有 `joint="curve"`（拐点圆滑）但**端点仍然是方的**，
    小尺寸下方头会显得毛糙。所以两端各补一个半径为半线宽的圆点。
    """
    d.line(points, fill=color + (255,), width=width, joint="curve")
    radius = width / 2
    for x, y in (points[0], points[-1]):
        d.ellipse([x - radius, y - radius, x + radius, y + radius], fill=color + (255,))


def draw_icon(size):
    """画一张 size×size 的图标。"""
    img = _tile(size)
    d = ImageDraw.Draw(img)

    # 统一从 16 单位网格换算（`u = size / 16`），保证任何尺寸下比例一致 ——
    # 这是小尺寸还能认出来的前提。
    u = size / 16.0

    # 线宽：16px 时 1.9u ≈ 1.9px，取整到 2px 才不会消失。
    width = max(2, round(1.9 * u))

    # `>`：从左上到中右再回左下。
    _stroke(d, [(5.4 * u, 5.2 * u), (8.9 * u, 8.0 * u), (5.4 * u, 10.8 * u)], width)

    # `_`：一个粗圆点（不是短横线 —— 短横线在 16px 下会和箭头糊成一块）。
    d.rounded_rectangle(
        [10.4 * u, 10.6 * u, 13.2 * u, 10.6 * u + width],
        radius=width / 2,
        fill=WHITE + (255,),
    )

    return img


def main():
    root = Path(__file__).resolve().parent.parent
    assets = root / "assets"
    assets.mkdir(exist_ok=True)

    frames = [draw_icon(s) for s in SIZES]

    ico = assets / "wslc-panel.ico"
    # Pillow 写 ICO：小尺寸存 BMP 层、256 存 PNG 层（Windows 两者都认）。
    frames[-1].save(ico, format="ICO", sizes=[(s, s) for s in SIZES])
    print(f"写出 {ico}  ({ico.stat().st_size} 字节)")

    # 预览图：各尺寸并排，方便在 PR 里肉眼确认（**不参与构建**）。
    # 背景涂成深灰 —— 任务栏默认就是这个色，图标必须在它上面成立。
    pad = 8
    total_w = sum(SIZES) + pad * (len(SIZES) + 1)
    max_h = max(SIZES)
    sheet = Image.new("RGBA", (total_w, max_h + pad * 2), (32, 32, 32, 255))
    x = pad
    for frame in frames:
        sheet.paste(frame, (x, pad + (max_h - frame.height) // 2), frame)
        x += frame.width + pad
    preview = assets / "wslc-panel-icon-preview.png"
    sheet.save(preview)
    print(f"写出 {preview}  ({preview.stat().st_size} 字节)")


if __name__ == "__main__":
    main()
