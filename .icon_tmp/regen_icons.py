"""Regenerate the Tauri *referenced* icon set from the full-frame source icon.png.

Rebuilds (only these, per requested scope):
  icon.ico            -> PNG-compressed entries at 16,24,32,48,64,128,256
  32x32.png
  128x128.png
  128x128@2x.png      -> 256x256
  icon.icns           -> PNG-based chunks 16..512 (incl. @2x)

Downscaling uses premultiplied-alpha Lanczos so the rounded-corner edges stay
clean (the source's transparent corners are (0,0,0,0), which would otherwise
bleed dark halos).
"""
import io, os, shutil, struct
import numpy as np
from PIL import Image

ICONS = os.path.join("src-tauri", "icons")
SRC = os.path.join(ICONS, "icon.png")
BACKUP = os.path.join(".icon_tmp", "orig_backup")
ZOOM = 1.25  # enlarge artwork within the tile so it reads at 16-32px; keeps rounded corners


def hq_resize(src, size):
    """Premultiplied-alpha Lanczos downscale of an RGBA PIL image to size x size."""
    arr = np.asarray(src.convert("RGBA"), dtype=np.float64)
    a = arr[..., 3:4] / 255.0
    pm = arr.copy()
    pm[..., :3] = arr[..., :3] * a                       # premultiply
    pm_im = Image.fromarray(np.clip(pm, 0, 255).astype("uint8"), "RGBA")
    pm_im = pm_im.resize((size, size), Image.LANCZOS)
    out = np.asarray(pm_im, dtype=np.float64)
    a2 = out[..., 3:4] / 255.0
    rgb = np.divide(out[..., :3], a2, out=np.zeros_like(out[..., :3]),
                    where=a2 > 0)                          # un-premultiply
    res = np.concatenate([np.clip(rgb, 0, 255), out[..., 3:4]], axis=-1)
    return Image.fromarray(np.clip(res, 0, 255).astype("uint8"), "RGBA")


def zoom(im, factor):
    """Enlarge the artwork by cropping a centered region (which becomes the new
    full frame). factor 1.25 keeps the central 80%; downstream resizes from this
    crop, so sizes <= the crop stay sharp."""
    if factor == 1.0:
        return im
    w, _ = im.size
    cw = int(round(w / factor))
    x = (w - cw) // 2
    return im.crop((x, x, x + cw, x + cw))


def png_bytes(im):
    buf = io.BytesIO()
    im.save(buf, format="PNG", optimize=True)
    return buf.getvalue()


def build_ico(src, sizes, path):
    blobs = [png_bytes(hq_resize(src, s)) for s in sizes]
    n = len(sizes)
    header = struct.pack("<HHH", 0, 1, n)
    offset = 6 + 16 * n
    entries = b""
    for s, blob in zip(sizes, blobs):
        b = 0 if s >= 256 else s
        entries += struct.pack("<BBBBHHII", b, b, 0, 0, 1, 32, len(blob), offset)
        offset += len(blob)
    with open(path, "wb") as f:
        f.write(header + entries + b"".join(blobs))


def build_icns(src, path):
    # OSType -> pixel size (PNG-encoded). Includes retina (@2x) variants.
    types = [(b"icp4", 16), (b"icp5", 32), (b"ic07", 128), (b"ic08", 256),
             (b"ic09", 512), (b"ic11", 32), (b"ic12", 64), (b"ic13", 256),
             (b"ic14", 512)]
    body = b""
    for ostype, size in types:
        data = png_bytes(hq_resize(src, size))
        body += ostype + struct.pack(">I", len(data) + 8) + data
    with open(path, "wb") as f:
        f.write(b"icns" + struct.pack(">I", len(body) + 8) + body)


def main():
    src = Image.open(SRC).convert("RGBA")
    assert src.size[0] == src.size[1], f"source must be square, got {src.size}"

    # Back up the TRUE originals once; never clobber an existing backup on re-runs.
    os.makedirs(BACKUP, exist_ok=True)
    for name in ("icon.ico", "32x32.png", "128x128.png", "128x128@2x.png", "icon.icns"):
        p = os.path.join(ICONS, name)
        dst = os.path.join(BACKUP, name)
        if os.path.exists(p) and not os.path.exists(dst):
            shutil.copy2(p, dst)

    src = zoom(src, ZOOM)

    build_ico(src, [16, 24, 32, 48, 64, 128, 256], os.path.join(ICONS, "icon.ico"))
    hq_resize(src, 32).save(os.path.join(ICONS, "32x32.png"), optimize=True)
    hq_resize(src, 128).save(os.path.join(ICONS, "128x128.png"), optimize=True)
    hq_resize(src, 256).save(os.path.join(ICONS, "128x128@2x.png"), optimize=True)
    build_icns(src, os.path.join(ICONS, "icon.icns"))
    print("Regenerated icon.ico, 32x32.png, 128x128.png, 128x128@2x.png, icon.icns")
    print(f"(originals backed up to {BACKUP})")


if __name__ == "__main__":
    main()
