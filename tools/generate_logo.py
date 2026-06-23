from __future__ import annotations

from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter


ROOT = Path(__file__).resolve().parents[1]
ASSETS = ROOT / "assets"


def rgba(hex_value: str, alpha: int = 255) -> tuple[int, int, int, int]:
    value = hex_value.removeprefix("#")
    return (
        int(value[0:2], 16),
        int(value[2:4], 16),
        int(value[4:6], 16),
        alpha,
    )


def p(size: int, raw: list[tuple[float, float]]) -> list[tuple[int, int]]:
    return [(round(x * size), round(y * size)) for x, y in raw]


def dark_bg(size: int, rounded: bool) -> Image.Image:
    small = Image.new("RGBA", (96, 96), rgba("#081326"))
    px = small.load()
    for y in range(96):
        for x in range(96):
            t = y / 95
            glow = max(0.0, 1.0 - (((x / 95) - 0.42) ** 2 + ((y / 95) - 0.22) ** 2) ** 0.5 * 2.0)
            px[x, y] = (
                round(5 + 9 * (1 - t)),
                round(12 + 17 * (1 - t) + 7 * glow),
                round(27 + 31 * (1 - t) + 20 * glow),
                255,
            )
    img = small.resize((size, size), Image.Resampling.BICUBIC)
    if rounded:
        mask = Image.new("L", (size, size), 0)
        ImageDraw.Draw(mask).rounded_rectangle(
            (0, 0, size - 1, size - 1),
            radius=round(size * 0.225),
            fill=255,
        )
        img.putalpha(mask)
    return img


def draw_mark(layer: Image.Image, bounds: tuple[float, float, float, float]) -> None:
    w, h = layer.size
    x, y, bw, bh = bounds
    size = min(w, h)

    def m(points: list[tuple[float, float]]) -> list[tuple[int, int]]:
        return [
            (
                round((x + px * bw) * w),
                round((y + py * bh) * h),
            )
            for px, py in points
        ]

    glow = Image.new("RGBA", layer.size, (0, 0, 0, 0))
    gd = ImageDraw.Draw(glow, "RGBA")
    upper = m([(0.16, 0.08), (0.90, 0.440), (0.792, 0.530), (0.265, 0.305)])
    lower = m([(0.265, 0.695), (0.792, 0.470), (0.90, 0.560), (0.16, 0.92)])
    gd.polygon(upper, fill=rgba("#58e7ff", 150))
    gd.polygon(lower, fill=rgba("#1ab9ee", 150))
    glow = glow.filter(ImageFilter.GaussianBlur(size * 0.024))
    layer.alpha_composite(glow)

    d = ImageDraw.Draw(layer, "RGBA")
    shadow = round(size * 0.008)
    d.polygon([(px + shadow, py + shadow) for px, py in upper], fill=rgba("#020714", 115))
    d.polygon([(px + shadow, py + shadow) for px, py in lower], fill=rgba("#020714", 115))

    d.polygon(upper, fill=rgba("#49dbff"))
    d.polygon(lower, fill=rgba("#16a7dc"))

    facets = [
        ([(0.16, 0.08), (0.375, 0.185), (0.300, 0.355), (0.265, 0.305)], "#f2ffff", 238),
        ([(0.375, 0.185), (0.645, 0.317), (0.585, 0.455), (0.300, 0.355)], "#74e7f8", 232),
        ([(0.645, 0.317), (0.900, 0.440), (0.792, 0.530), (0.585, 0.455)], "#4fd4f5", 230),
        ([(0.265, 0.695), (0.300, 0.645), (0.375, 0.815), (0.16, 0.920)], "#dcfbff", 226),
        ([(0.300, 0.645), (0.585, 0.545), (0.645, 0.683), (0.375, 0.815)], "#2ec3ee", 232),
        ([(0.585, 0.545), (0.792, 0.470), (0.900, 0.560), (0.645, 0.683)], "#20abd9", 232),
        ([(0.300, 0.355), (0.635, 0.500), (0.300, 0.645), (0.455, 0.500)], "#061225", 255),
    ]
    for points, fill, alpha in facets:
        d.polygon(m(points), fill=rgba(fill, alpha))

    line_w = max(3, round(size * 0.009))
    d.line(m([(0.185, 0.095), (0.900, 0.440)]), fill=rgba("#f7ffff", 220), width=line_w)
    d.line(m([(0.185, 0.905), (0.900, 0.560)]), fill=rgba("#d7fbff", 182), width=line_w)
    d.line(m([(0.900, 0.440), (0.900, 0.560)]), fill=rgba("#8ff3ff", 155), width=max(2, line_w - 1))


def draw_cursor(layer: Image.Image, rect: tuple[float, float, float, float]) -> None:
    w, h = layer.size
    x, y, rw, rh = rect
    box = (
        round(x * w),
        round(y * h),
        round((x + rw) * w),
        round((y + rh) * h),
    )
    size = min(w, h)

    glow = Image.new("RGBA", layer.size, (0, 0, 0, 0))
    gd = ImageDraw.Draw(glow, "RGBA")
    gd.rounded_rectangle(box, radius=round(size * 0.012), fill=rgba("#67eaff", 150))
    glow = glow.filter(ImageFilter.GaussianBlur(size * 0.020))
    layer.alpha_composite(glow)

    d = ImageDraw.Draw(layer, "RGBA")
    off = round(size * 0.012)
    shadow = (box[0] + off, box[1] + off, box[2] + off, box[3] + off)
    d.rounded_rectangle(shadow, radius=round(size * 0.012), fill=rgba("#020713", 120))
    d.rounded_rectangle(box, radius=round(size * 0.006), fill=rgba("#d8fbff"))
    d.polygon(
        [
            (box[0], box[1]),
            (box[2], box[1]),
            (box[0], box[3]),
        ],
        fill=rgba("#f5ffff", 150),
    )
    d.polygon(
        [
            (box[2], box[1]),
            (box[2], box[3]),
            (box[0], box[3]),
        ],
        fill=rgba("#31bde8", 140),
    )
    d.rounded_rectangle(
        (box[0], box[1], box[2], box[3]),
        radius=round(size * 0.006),
        outline=rgba("#5ee4ff"),
        width=max(2, round(size * 0.010)),
    )


def make_icon(size: int) -> Image.Image:
    scale = 3
    work = size * scale
    img = dark_bg(work, rounded=True)
    # draw_mark(img, (0.195, 0.225, 0.520, 0.450))
    # draw_cursor(img, (0.715, 0.405, 0.070, 0.190))
    return img.resize((size, size), Image.Resampling.LANCZOS)


def main() -> None:
    ASSETS.mkdir(exist_ok=True)

    icon = make_icon(1024)
    icon.save(ASSETS / "glacia-term-icon.png")
    icon512 = icon.resize((512, 512), Image.Resampling.LANCZOS)
    icon512.save(ASSETS / "glacia-term-icon-512.png")
    icon.resize((256, 256), Image.Resampling.LANCZOS).save(ASSETS / "glacia-term-icon-256.png")

    # Windows ICO — multiple sizes so Explorer/taskbar all look crisp.
    ico_sizes = [(16, 16), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)]
    ico_frames = [icon512.resize(s, Image.Resampling.LANCZOS) for s in ico_sizes]
    ico_frames[0].save(
        ASSETS / "glacia-term-icon.ico",
        format="ICO",
        sizes=ico_sizes,
        append_images=ico_frames[1:],
    )


if __name__ == "__main__":
    main()
