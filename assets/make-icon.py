#!/usr/bin/env python3
"""生成 wslc-panel 的图标（`assets/wslc-panel.ico` + 一张预览 png）。

# 为什么要有这个脚本

图标是**画出来的**，不是找来的：

- 参考项目（`E:\\cnb\\wsl-dashboard-ref`）的图标是 **GPL-3.0-only**，
  按 `AGENTS.md` §6 一行都不能进本仓库；
- 也不能随手拿个网图 —— 许可证说不清。

所以这里用 Pillow 把图形**程序化**画出来，脚本就是图形的源码，
以后改配色 / 改形状都只改它再重跑：`python assets/make-icon.py`。

# 图形是什么意思

- 深色圆角底 = 面板窗口；
- 左边一条蓝色高亮带 = **侧边栏**（这个程序是"侧边栏 + 内容区"的形状）；
- 右边三行**长度错开**的横条 = 容器列表，首行青色 = 当前选中的那一行；
- 第三行半透明 = 列表没铺满，暗示"还能往下滚"。

**不是**集装箱、也不是企鹅：
- 企鹅是 Linux 的通用符号，会和同类工具撞脸（参考项目已经用了）；
- 集装箱方块在小尺寸下会糊成一个色块，而"侧边栏 + 列表"在 16px 上
  仍然能读出"左边一条、右边几行"的结构 —— 这是选它的主要原因。

# 小尺寸可读性（这是设计的硬约束）

16×16 只有 256 个像素，所以：

- 所有尺寸都从同一个 **16 单位网格**换算（`u = size / 16`），比例不会走样；
- 侧边栏做成一条**竖直矩形**，而不是带圆角的胶囊（圆角在 16px 上会吃掉 2px，
  剩下的宽度不足以看出"这是一条"）；
- 圆角半径随尺寸缩放，16px 时只给 3。

用法：`python assets/make-icon.py`（在仓库根执行；依赖 Pillow）
"""

from pathlib import Path

from PIL import Image, ImageDraw

# 尺寸：Windows 按 DPI 与场景挑最合适的那一层，缺哪层就缩放（会糊）。
# 16 = 任务栏 / 小列表；32 = 桌面；48 = 中等图标；256 = 大图标 / 安装包。
SIZES = [16, 24, 32, 48, 64, 128, 256]

# 配色：和 `theme.rs` 的深色系同一个方向，但刻意更饱和一点 ——
# 图标在浅色桌面上也要立得住。
BG_TOP = (30, 41, 59)       # #1E293B
BG_BOTTOM = (12, 18, 30)    # 接近纯黑，让高亮色更跳
PANEL = (37, 51, 71)        # #253347 窗口主体
SIDEBAR = (59, 130, 246)    # #3B82F6 侧边栏
ROW_ACTIVE = (56, 189, 248)  # #38BDF8 选中行
ROW = (100, 116, 139)       # #64748B 普通行


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
        radius=max(2, round(size * 0.22)),
        fill=255,
    )
    img.paste(grad, (0, 0), mask)
    return img


def draw_icon(size):
    """画一张 size×size 的图标。"""
    img = _tile(size)
    d = ImageDraw.Draw(img)

    # 统一从 16 单位网格换算（见模块说明"小尺寸可读性"）。
    u = size / 16.0
    r = lambda n: max(1, round(n * u))  # noqa: E731 - 局部小助手，不值得单开函数

    # 1) 窗口主体
    d.rounded_rectangle(
        [1.8 * u, 2.2 * u, 14.2 * u, 13.8 * u],
        radius=r(1.5),
        fill=PANEL + (255,),
    )

    # 2) 侧边栏：一条竖矩形，上下各留 1.5u 让窗口的圆角露出来。
    #    先铺整条，再把上下两小段用窗口色盖回去 —— 比"只画矩形中间一段"
    #    少一处坐标计算，效果一样。
    d.rectangle([1.8 * u, 3.7 * u, 5.1 * u, 12.3 * u], fill=SIDEBAR + (255,))
    d.rectangle([1.8 * u, 2.2 * u, 5.1 * u, 3.7 * u], fill=PANEL + (255,))
    d.rectangle([1.8 * u, 12.3 * u, 5.1 * u, 13.8 * u], fill=PANEL + (255,))

    # 3) 三行列表：长度错开（像真实内容），首行高亮。
    rows = [
        (6.3, 4.7, 12.7, ROW_ACTIVE, 255),
        (6.3, 8.0, 11.6, ROW, 255),
        (6.3, 11.3, 13.1, ROW, 170),  # 半透明 = 还没铺满
    ]
    for x0, y0, x1, color, alpha in rows:
        d.rounded_rectangle(
            [x0 * u, y0 * u, x1 * u, (y0 + 1.7) * u],
            radius=r(0.5),
            fill=color + (alpha,),
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
    pad = 8
    total_w = sum(SIZES) + pad * (len(SIZES) + 1)
    max_h = max(SIZES)
    sheet = Image.new("RGBA", (total_w, max_h + pad * 2), (245, 245, 245, 255))
    x = pad
    for frame in frames:
        sheet.paste(frame, (x, pad + (max_h - frame.height) // 2), frame)
        x += frame.width + pad
    preview = assets / "wslc-panel-icon-preview.png"
    sheet.save(preview)
    print(f"写出 {preview}  ({preview.stat().st_size} 字节)")


if __name__ == "__main__":
    main()
