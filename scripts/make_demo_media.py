"""Generates made-up demo media for the README screenshots.

    python scripts/make_demo_media.py C:\\demo\\media

Everything is drawn or synthesised here (gradients, shapes, ffmpeg test patterns,
sine tones, invented file names and subtitle lines); no real image, video, name or
channel is used. Needs Pillow and ffmpeg on PATH. Safe to run again: it rewrites the
same files.
"""
import colorsys
import math
import random
import subprocess
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

if len(sys.argv) != 2:
    sys.exit(__doc__)
out = Path(sys.argv[1])
rng = random.Random(7)  # fixed seed: the same files every run


def gradient(w, h, hue, hue2):
    """Diagonal two-colour gradient."""
    img = Image.new("RGB", (w, h))
    px = img.load()
    c1 = colorsys.hsv_to_rgb(hue, 0.65, 0.95)
    c2 = colorsys.hsv_to_rgb(hue2, 0.75, 0.55)
    for y in range(h):
        for x in range(w):
            t = (x / w + y / h) / 2
            px[x, y] = tuple(int(255 * (a + (b - a) * t)) for a, b in zip(c1, c2))
    return img


def shapes(img, n):
    d = ImageDraw.Draw(img, "RGBA")
    w, h = img.size
    for _ in range(n):
        r = rng.randint(min(w, h) // 12, min(w, h) // 4)
        x, y = rng.randint(0, w), rng.randint(0, h)
        col = tuple(int(255 * v) for v in colorsys.hsv_to_rgb(rng.random(), 0.5, 1.0)) + (110,)
        if rng.random() < 0.5:
            d.ellipse([x - r, y - r, x + r, y + r], fill=col)
        else:
            d.rectangle([x - r, y - r // 2, x + r, y + r // 2], fill=col)
    return img


def font(size):
    for name in ("msjh.ttc", "segoeui.ttf", "arial.ttf"):
        try:
            return ImageFont.truetype(name, size)
        except OSError:
            pass
    return ImageFont.load_default()


def make_images():
    d = out / "images"
    d.mkdir(parents=True, exist_ok=True)
    names = [
        "sunset_harbor", "mint_garden", "paper_lantern", "blue_hour", "orange_peel",
        "cloud_study", "night_market", "pine_ridge", "coral_reef", "lavender_field",
        "tea_house", "neon_street", "snow_cabin", "desert_dune", "lemon_grove",
        "river_bend", "autumn_path", "glass_tower", "moss_stone", "rain_window",
        "paper_boats", "plum_blossom", "salt_flat", "ember_glow",
    ]
    # Mixed shapes so the layouts (grid, proportional rows) have something to show.
    sizes = [(1600, 1000), (1200, 1200), (900, 1350), (1920, 1080), (1000, 1500), (1500, 1000)]
    for i, name in enumerate(names):
        w, h = sizes[i % len(sizes)]
        img = shapes(gradient(w // 2, h // 2, (i * 0.137) % 1, (i * 0.137 + 0.18) % 1), 9)
        img = img.resize((w, h))
        ImageDraw.Draw(img).text((w // 20, h // 20), f"{i + 1:02d}", fill=(255, 255, 255), font=font(h // 14))
        img.save(d / f"{name}.png" if i % 3 else d / f"{name}.jpg", quality=88)


def ffmpeg(*args):
    subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", *args], check=True)


def make_video(name, w, h, seconds, tone, label):
    f = out / "video" / name
    f.parent.mkdir(parents=True, exist_ok=True)
    ffmpeg(
        "-f", "lavfi", "-i", f"testsrc2=size={w}x{h}:rate=30:duration={seconds}",
        "-f", "lavfi", "-i", f"sine=frequency={tone}:sample_rate=48000:duration={seconds}",
        "-c:v", "libx264", "-preset", "veryfast", "-crf", "30", "-pix_fmt", "yuv420p",
        "-c:a", "aac", "-b:a", "96k", "-shortest", str(f),
    )


def make_audio():
    d = out / "audio"
    d.mkdir(parents=True, exist_ok=True)
    for name, hz in (("theme_loop_a.m4a", 330), ("theme_loop_b.m4a", 440)):
        ffmpeg("-f", "lavfi", "-i", f"sine=frequency={hz}:duration=45", "-c:a", "aac", "-b:a", "64k", str(d / name))


SUBS = [
    "大家好，今天來玩一個小遊戲",
    "先把設定調好，馬上開始",
    "這一關比想像中難欸",
    "再來一次，這次一定過",
    "哇，剛剛那個太漂亮了",
    "謝謝大家的留言，我有看到",
    "我們休息一下，喝口水",
    "好，繼續往下一個區域",
    "這裡有隱藏的通道，大家看",
    "最後一關了，加油",
    "今天就玩到這裡，下次見",
]


def srt_time(s):
    return f"{int(s // 3600):02d}:{int(s % 3600 // 60):02d}:{int(s % 60):02d},{int(s % 1 * 1000):03d}"


def make_subs(name, seconds):
    f = out / "video" / name
    step = seconds / len(SUBS)
    lines = []
    for i, text in enumerate(SUBS):
        lines += [str(i + 1), f"{srt_time(i * step + 1)} --> {srt_time(i * step + step - 1)}", text, ""]
    f.write_text("\n".join(lines), encoding="utf-8")


def make_near_duplicates():
    """Slightly brightened, 90 % size copies, for the "possible duplicates" screenshot.
    They go in a second folder (media2) that is imported after the main one."""
    d = out.parent / (out.name + "2")
    d.mkdir(parents=True, exist_ok=True)
    from PIL import ImageEnhance
    for name, tint in (("sunset_harbor", 1.06), ("mint_garden", 0.94), ("neon_street", 1.08), ("coral_reef", 0.92)):
        src = next((out / "images").glob(name + ".*"))
        im = Image.open(src).convert("RGB")
        im = ImageEnhance.Brightness(im).enhance(tint).resize((int(im.width * 0.9), int(im.height * 0.9)))
        im.save(d / f"{name}_edit.jpg", quality=85)


make_images()
make_near_duplicates()
make_video("demo_stream_01.mp4", 1280, 720, 110, 220, "a")
make_subs("demo_stream_01.srt", 110)
make_video("demo_stream_02.mp4", 1280, 720, 75, 262, "b")
make_video("demo_vertical_01.mp4", 720, 1280, 20, 523, "c")
make_audio()
print("done:", out)
