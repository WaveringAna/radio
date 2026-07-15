from pathlib import Path

from PIL import Image, ImageDraw


OUT = Path(__file__).resolve().parents[1] / "static" / "assets"
PIXEL_FONT = {
    "n": ["00000", "11010", "11110", "11010", "11010", "11010", "00000"],
    "i": ["010", "000", "110", "010", "010", "111", "000"],
    "r": ["0000", "1010", "1110", "1100", "1000", "1000", "0000"],
}


def rect(draw, xy, fill):
    draw.rectangle(xy, fill=fill)


def poly(draw, points, fill):
    draw.polygon(points, fill=fill)


def line(draw, points, fill, width=1):
    draw.line(points, fill=fill, width=width)


def draw_heart(draw, x, y, color, shade):
    pixels = [
        "01010",
        "11111",
        "11111",
        "01110",
        "00100",
    ]
    for py, row in enumerate(pixels):
        for px, bit in enumerate(row):
            if bit == "1":
                rect(draw, (x + px, y + py, x + px, y + py), shade if py > 3 else color)


def draw_text(draw, text, x, y, scale, fill, shade):
    cursor = x
    for ch in text:
        glyph = PIXEL_FONT[ch]
        for gy, row in enumerate(glyph):
            for gx, bit in enumerate(row):
                if bit == "1":
                    xy = (
                        cursor + gx * scale,
                        y + gy * scale,
                        cursor + (gx + 1) * scale - 1,
                        y + (gy + 1) * scale - 1,
                    )
                    shadow_xy = (xy[0] + 1, xy[1] + 1, xy[2] + 1, xy[3] + 1)
                    rect(draw, shadow_xy, shade)
                    rect(draw, xy, fill)
        cursor += (len(glyph[0]) + 1) * scale


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    img = Image.new("RGB", (88, 31), "#10081d")
    d = ImageDraw.Draw(img)

    # Button frame and subtle inner panel.
    rect(d, (0, 0, 87, 30), "#050208")
    rect(d, (1, 1, 86, 29), "#4b2a73")
    rect(d, (2, 2, 85, 28), "#13091f")
    rect(d, (30, 4, 83, 26), "#170b28")
    line(d, [(31, 5), (82, 5)], "#27123f")
    line(d, [(31, 25), (82, 25)], "#07030d")

    # Left badge area.
    rect(d, (3, 4, 29, 26), "#1e0c31")
    line(d, [(30, 5), (30, 25)], "#3b1d59")

    # Protogen ear, helmet shell, cheek, and neck.
    poly(d, [(7, 10), (1, 8), (11, 8)], "#8d54df")
    poly(d, [(7, 10), (3, 8), (11, 8)], "#d8adff")
    poly(d, [(19, 8), (27, 8), (22, 12)], "#7041bd")
    poly(d, [(20, 8), (25, 8), (22, 10)], "#b477ff")
    poly(d, [(4, 10), (14, 6), (24, 9), (26, 15), (21, 21), (8, 21), (3, 16)], "#7240c1")
    poly(d, [(6, 10), (15, 7), (23, 10), (24, 15), (20, 18), (8, 19), (4, 15)], "#a86bff")
    poly(d, [(18, 14), (26, 15), (22, 20), (14, 20)], "#5d2fa6")
    poly(d, [(20, 16), (26, 17), (22, 19)], "#c897ff")
    rect(d, (10, 20, 15, 24), "#5a2ca0")
    rect(d, (7, 24, 19, 25), "#8750d8")

    # Dark visor with solid heart eye and small right eye glint.
    poly(d, [(5, 11), (15, 9), (22, 11), (23, 14), (19, 17), (7, 17), (4, 14)], "#16061f")
    line(d, [(7, 10), (15, 10), (21, 12)], "#ffd0f2")
    draw_heart(d, 7, 11, "#ff5fcf", "#d938aa")

    # Tiny muzzle vents and face outline help it read as a protogen at 88x31.
    rect(d, (20, 16, 21, 16), "#2b103a")
    rect(d, (23, 17, 24, 17), "#2b103a")
    line(d, [(3, 16), (8, 20), (19, 20), (25, 16)], "#d0a2ff")
    line(d, [(4, 10), (11, 8), (22, 9)], "#d0a2ff")

    # Crisp manual pixel text, no resampling distortion.
    draw_text(d, "niri", 41, 9, 2, "#f5e9ff", "#5e347e")
    rect(d, (37, 14, 38, 15), "#ff6bd3")
    rect(d, (78, 14, 79, 15), "#ff6bd3")

    img.save(OUT / "niri-88x31.png")
    img.resize((352, 124), Image.Resampling.NEAREST).save(OUT / "niri-88x31-preview-4x.png")


if __name__ == "__main__":
    main()
