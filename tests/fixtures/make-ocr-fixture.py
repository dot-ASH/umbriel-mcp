from PIL import Image, ImageDraw, ImageFont
import sys
font_path = "/usr/share/fonts/noto/NotoSans-Regular.ttf"
def render(width, height, size, out):
    img = Image.new("RGB", (width, height), (24, 28, 34))
    d = ImageDraw.Draw(img)
    f = ImageFont.truetype(font_path, size)
    d.text((60, 60), "Umbriel OCR Probe", font=f, fill=(235, 235, 235))
    x = 60
    for label in ("Settings", "Save", "Cancel"):
        w = d.textlength(label, font=f)
        d.rounded_rectangle((x - 12, 150, x + w + 12, 150 + size + 24), radius=6, fill=(60, 70, 90))
        d.text((x, 162), label, font=f, fill=(250, 250, 250))
        x += int(w) + 70
    d.text((60, 270), "quick brown fox 12345", font=f, fill=(200, 210, 220))
    d.text((60, 340), "Open File", font=f, fill=(200, 210, 220))
    img.save(out)
render(3840, 1080, 16, "/tmp/ocrfix/wide16.png")
render(1280, 420, 28, "/tmp/ocrfix/big28.png")
