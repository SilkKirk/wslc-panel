#!/usr/bin/env python3
"""生成 wslc-panel 的图标（从 `assets/source-art.png` 截取角色头部）。

# 为什么是"截头部"而不是整张插画

`source-art.png` 是一张 1067×600 的横版立绘。整张缩进图标的话，32px 下只剩
一团淡紫 + 浅蓝的色块 —— 这是插画转图标最典型的失败方式。

桌面上的 March7th Launcher 给了正确的示范：**只截头部**。脸在小尺寸下仍然可辨，
而且有辨识度。

# 为什么不再用代码画

前六版都是在这个脚本里用 Pillow 手绘（汉堡菜单 → 深色面板 → `>_` 提示符 →
集装箱 → 层叠板 → 六边形 → 涟漪 / 波 / 点阵 / 螺旋），全部被否。
结论：**用代码拼图标到不了专业完成度**。所以改成"用户给图 + 自动裁切"。

# 它是怎么做的

1. 从 `source-art.png` 裁出头部方框（坐标见 `CROP`，写的是**相对值**，
   换分辨率不用改）；
2. 缩放到目标尺寸 —— **每档都从裁好的大图单独缩**，逐级缩会累积模糊；
3. 套 22% 圆角的遮罩（Windows 11 应用图标的圆角比例）；
4. 加一道极淡的内高光 —— Windows 11 那套图标都有，不加会显得"贴上去的"。

用法：`python assets/make-icon.py`
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

from PIL import Image, ImageDraw

SIZES = [16, 24, 32, 48, 64, 128, 256]

# 头部裁切框，相对坐标（左, 上, 右, 下）。
#
# 这三个值是在 `source-art.png` 上试了三版定下来的：
#   · 更紧（0.255 / 0.10 / 0.445 / 0.52）会切掉额头；
#   · 更宽（0.170 / 0.02 / 0.520 / 0.60）脸在图标里太小；
#   · 现在这组：脸完整、带发饰与一点肩膀，32px 仍能认出是个人物。
CROP = (0.225, 0.06, 0.475, 0.56)

# 圆角半径占边长的比例。Windows 11 的应用图标大致在这个量级（22%~23%），
# 和相邻的 Outlook / 商店应用属于同一套视觉语言。
CORNER_RATIO = 0.22

# 内高光：白色、极低透明度、贴着圆角内侧一圈。
# 这一层是"看起来像系统图标"和"看起来像一张图片"的分界。
INNER_GLOW_ALPHA = 46

SS = 4  # 超采样倍数：先按 4 倍画，最后缩回去，边缘才干净

# 预览图的两种背景：任务栏默认深色 + 资源管理器浅色。
DARK_BG = (32, 32, 32, 255)
LIGHT_BG = (243, 243, 243, 255)


def crop_head(src: Image.Image) -> Image.Image:
    """按 `CROP` 裁出正方形头部图。"""
    w, h = src.size
    left, top, right, bottom = CROP
    box = (int(left * w), int(top * h), int(right * w), int(bottom * h))
    crop = src.crop(box)

    # 裁出来的框不一定正方（原图比例会变），以短边为准居中。
    cw, ch = crop.size
    side = min(cw, ch)
    return crop.crop(
        ((cw - side) // 2, (ch - side) // 2, (cw - side) // 2 + side, (ch - side) // 2 + side)
    )


def render(head: Image.Image, size: int) -> Image.Image:
    """把头部图渲染成 size×size 的圆角图标。"""
    canvas = size * SS
    body = head.resize((canvas, canvas), Image.LANCZOS)

    mask = Image.new("L", (canvas, canvas), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        [0, 0, canvas - 1, canvas - 1], radius=round(canvas * CORNER_RATIO), fill=255
    )

    out = Image.new("RGBA", (canvas, canvas), (0, 0, 0, 0))
    out.paste(body, (0, 0), mask)

    # 内高光：贴着圆角内缘一圈，宽度也按画布缩放
    glow_layer = Image.new("RGBA", (canvas, canvas), (0, 0, 0, 0))
    inset = max(1, round(canvas * 0.012))
    ImageDraw.Draw(glow_layer).rounded_rectangle(
        [inset, inset, canvas - inset, canvas - inset],
        radius=round(canvas * (CORNER_RATIO - 0.006)),
        outline=(255, 255, 255, INNER_GLOW_ALPHA),
        width=max(1, round(canvas * 0.010)),
    )
    out = Image.alpha_composite(out, glow_layer)

    return out.resize((size, size), Image.LANCZOS)


def write_preview(frames: list[Image.Image], out: Path) -> None:
    """各尺寸并排，深色与浅色两种背景各一行 —— 图标必须在两边都成立。"""
    pad = 10
    # 每个格子按**最大档**的宽度算，图标居中。
    # ⚠️ 不能"逐个累加宽度"：那会让小格挤在一起、大格互相重叠。
    cell = max(SIZES) + pad
    total_w = cell * len(SIZES) + pad
    row_h = max(SIZES) + pad * 2

    sheet = Image.new("RGBA", (total_w, row_h * 2), DARK_BG)
    for row, bg in enumerate((DARK_BG, LIGHT_BG)):
        y0 = row * row_h
        ImageDraw.Draw(sheet).rectangle([0, y0, total_w, y0 + row_h], fill=bg)
        for i, frame in enumerate(frames):
            x = pad + i * cell + (max(SIZES) - frame.width) // 2
            y = y0 + pad + (max(SIZES) - frame.height) // 2
            sheet.paste(frame, (x, y), frame)
    sheet.save(out)


def main() -> None:
    ap = argparse.ArgumentParser(description="从立绘截取头部生成 wslc-panel 的 .ico")
    ap.add_argument(
        "--from",
        dest="source",
        type=Path,
        default=None,
        help="源图（默认 assets/source-art.png）",
    )
    args = ap.parse_args()

    root = Path(__file__).resolve().parent.parent
    assets = root / "assets"
    source = args.source or (assets / "source-art.png")
    if not source.exists():
        sys.exit(f"找不到源图：{source}")

    src = Image.open(source).convert("RGBA")
    head = crop_head(src)

    # 存一份裁好的 256 方图：以后调尺寸/调边距不必重新算裁切框
    head.resize((256, 256), Image.LANCZOS).save(assets / "wslc-panel-256.png")

    frames = [render(head, s) for s in SIZES]

    ico = assets / "wslc-panel.ico"
    # 小尺寸存 BMP、256 存 PNG —— 这是 ICO 的标准做法，Pillow 自动处理。
    frames[-1].save(ico, format="ICO", sizes=[(s, s) for s in SIZES])
    print(f"源图 : {source.name}")
    print(f"写出 : {ico}  ({ico.stat().st_size} 字节，{len(SIZES)} 档)")

    preview = assets / "wslc-panel-icon-preview.png"
    write_preview(frames, preview)
    print(f"预览 : {preview}")


if __name__ == "__main__":
    main()
