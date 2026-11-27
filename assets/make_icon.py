from PIL import Image, ImageDraw, ImageFilter
import math

S = 1024
def lerp(a, b, t): return tuple(int(a[i] + (b[i] - a[i]) * t) for i in range(len(a)))

# background: diagonal gradient in a rounded square
bg = Image.new("RGBA", (S, S))
c1, c2 = (18, 24, 48), (40, 56, 120)
px = bg.load()
for y in range(S):
    for x in range(S):
        t = (x + y) / (2 * S)
        px[x, y] = lerp(c1, c2, t) + (255,)
mask = Image.new("L", (S, S), 0)
ImageDraw.Draw(mask).rounded_rectangle([40, 40, S - 40, S - 40], radius=220, fill=255)
icon = Image.new("RGBA", (S, S), (0, 0, 0, 0))
icon.paste(bg, (0, 0), mask)

# subtle top highlight
hl = Image.new("RGBA", (S, S), (0, 0, 0, 0))

hl_m = Image.new("L", (S, S), 0); hl_m.paste(mask, (0, 0))
icon = Image.alpha_composite(icon, Image.composite(hl, Image.new("RGBA", (S, S)), hl_m))

d = ImageDraw.Draw(icon)
cx, cy, r = S // 2, S // 2 + 10, 330
W = 56
# gauge track
d.arc([cx - r, cy - r, cx + r, cy + r], 135, 405, fill=(64, 78, 140, 255), width=W)
# gauge value: blue -> violet -> amber gradient along the arc
steps = 220
for i in range(steps):
    t = i / steps
    a0 = 135 + 210 * t
    col = lerp((76, 178, 255), (111, 140, 255), t * 2) if t < 0.5 else lerp((111, 140, 255), (255, 165, 61), (t - 0.5) * 2)
    d.arc([cx - r, cy - r, cx + r, cy + r], a0, a0 + 210 / steps + 0.8, fill=col + (255,), width=W)
# round caps
for ang, col in [(135, (76, 178, 255)), (345, (255, 165, 61))]:
    a = math.radians(ang)
    x, y = cx + (r - W / 2) * math.cos(a), cy + (r - W / 2) * math.sin(a)
    d.ellipse([x - W / 2, y - W / 2, x + W / 2, y + W / 2], fill=col + (255,))

def arrow(x, top, bottom, up, col):
    w, head = 70, 120
    if up:
        d.polygon([(x, top), (x - head, top + head), (x + head, top + head)], fill=col)
        d.rounded_rectangle([x - w / 2, top + head - 10, x + w / 2, bottom], radius=30, fill=col)
    else:
        d.polygon([(x, bottom), (x - head, bottom - head), (x + head, bottom - head)], fill=col)
        d.rounded_rectangle([x - w / 2, top, x + w / 2, bottom - head + 10], radius=30, fill=col)

arrow(cx - 95, cy - 150, cy + 190, False, (76, 178, 255, 255))
arrow(cx + 95, cy - 190, cy + 150, True, (255, 165, 61, 255))

icon.save("icon-1024.png")
icon.resize((256, 256), Image.LANCZOS).save("icon-256.png")
icon.resize((128, 128), Image.LANCZOS).save("icon-128.png")
icon.save("app.ico", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
