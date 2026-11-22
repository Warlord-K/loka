import argparse
import colorsys
import json
import shutil
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
SHIPPED = ROOT / "public" / "sprites" / "style-a"
WORLD = ROOT / "sprites" / "style-a"
CAST = ROOT / "sprites" / "cast"
MAX_W = 1024
GAP = 2
SWAP_ROLES = {"skin", "hair", "shirt", "pants", "suit", "tie"}
VIEWS = ["front", "side", "back"]


def white_mask(path):
    grey = Image.open(path).convert("L")
    out = Image.new("RGBA", grey.size, (0, 0, 0, 0))
    out.putdata([(255, 255, 255, 255) if v > 127 else (0, 0, 0, 0) for v in grey.getdata()])
    return out


def mean_under(img, mask):
    a, m = img.load(), mask.load()
    acc = [0, 0, 0, 0]
    for y in range(img.height):
        for x in range(img.width):
            if m[x, y][3] and a[x, y][3]:
                p = a[x, y]
                acc[0] += p[0]
                acc[1] += p[1]
                acc[2] += p[2]
                acc[3] += 1
    return "#%02x%02x%02x" % tuple(round(acc[i] / max(1, acc[3])) for i in range(3))


def shade_of(c, ref):
    v, vr = max(c), max(ref)
    if v <= vr:
        return round(v / vr - 1, 3)
    return round((v - vr) / (255 - vr), 3)


def lightness(rgb):
    return colorsys.rgb_to_hls(*(v / 255 for v in rgb))[1]


def lightness_shade(c, ref):
    l, lr = lightness(c), lightness(ref)
    if l <= lr:
        return round(max(-1, l / lr - 1) if lr else 0, 3)
    return round(min(1, (l - lr) / (1 - lr)) if lr < 1 else 0, 3)


def rgb(hexcode):
    return tuple(int(hexcode[i:i + 2], 16) for i in (1, 3, 5))


def wall_swaps(img, masks, sign):
    a = img.load()
    loaded = [m.load() for m in masks]
    counts = {}
    for y in range(img.height):
        for x in range(img.width):
            p = a[x, y]
            if not p[3] or any(m[x, y][3] for m in loaded):
                continue
            if sign and sign[0] <= x < sign[0] + sign[2] and sign[1] <= y < sign[1] + sign[3]:
                continue
            h, s, v = colorsys.rgb_to_hsv(p[0] / 255, p[1] / 255, p[2] / 255)
            if 0.06 <= h <= 0.15 and 0.08 <= s <= 0.32 and v >= 0.78:
                counts[p[:3]] = counts.get(p[:3], 0) + 1
    if not counts:
        return {}
    ref = max(counts, key=counts.get)
    return {"#%02x%02x%02x" % c: ["wall", shade_of(c, ref)] for c, n in counts.items() if n >= 8}


def scaled_points(item, img):
    sx = img.width / item.get("w", img.width)
    sy = img.height / item.get("h", img.height)
    out = {}
    for name, value in (item.get("points") or {}).items():
        if not isinstance(value, list) or len(value) not in (2, 4) or not all(isinstance(n, (int, float)) for n in value):
            continue
        if len(value) == 2:
            out[name] = [round(value[0] * sx), round(value[1] * sy)]
        else:
            out[name] = [round(value[0] * sx), round(value[1] * sy), round(value[2] * sx), round(value[3] * sy)]
    return out


def face_radius(img, centre):
    px = img.load()

    def light(x, y):
        return 0 <= x < img.width and 0 <= y < img.height and px[x, y][3] and min(px[x, y][:3]) > 200

    start = next(((centre[0] + dx, centre[1] + dy) for r in range(1, 8) for dx in range(-r, r + 1) for dy in (-r, r) if light(centre[0] + dx, centre[1] + dy)), None)
    if not start:
        return 0
    seen, stack = {start}, [start]
    while stack:
        x, y = stack.pop()
        for n in ((x + 1, y), (x - 1, y), (x, y + 1), (x, y - 1)):
            if n not in seen and light(*n) and abs(n[0] - centre[0]) < 40 and abs(n[1] - centre[1]) < 40:
                seen.add(n)
                stack.append(n)
    return max(((x - centre[0]) ** 2 + (y - centre[1]) ** 2) ** 0.5 for x, y in seen)


def blank_clock_face(img, centre, reach):
    px = img.load()
    inside = [
        (x, y)
        for y in range(int(centre[1] - reach), int(centre[1] + reach) + 1)
        for x in range(int(centre[0] - reach), int(centre[0] + reach) + 1)
        if (x - centre[0]) ** 2 + (y - centre[1]) ** 2 <= reach * reach
    ]
    counts = {}
    for x, y in inside:
        counts[px[x, y]] = counts.get(px[x, y], 0) + 1
    face = max(counts, key=counts.get)
    for x, y in inside:
        px[x, y] = face


class Shelf:
    def __init__(self, top):
        self.x, self.y, self.row = 0, top, 0
        self.parts = []

    def place(self, im):
        if self.x + im.width > MAX_W:
            self.y += self.row + GAP
            self.x, self.row = 0, 0
        at = [self.x, self.y]
        self.parts.append((im, at))
        self.x += im.width + GAP
        self.row = max(self.row, im.height)
        return at

    def bottom(self):
        return self.y + self.row


def thing_spec(export, item, shelf):
    img = Image.open(export / item["file"]).convert("RGBA")
    spec = {
        "density": item["density"],
        "anchor": [item["anchor"]["x"], item["anchor"]["y"]],
        "frames": [shelf.place(img) + [img.width, img.height]],
    }
    points = scaled_points(item, img)
    sign = item.get("sign")
    sign_rect = [sign["x"], sign["y"], sign["w"], sign["h"]] if sign else points.get("sign")
    if sign_rect:
        spec["sign"] = sign_rect
    if "clock_face" in points:
        radius = face_radius(img, points["clock_face"])
        if radius:
            cx, cy = points["clock_face"]
            points["clock_rim"] = [cx, round(cy - radius * 0.8)]
            blank_clock_face(img, (cx, cy), radius * 0.65)
    listed = {}
    for name, value in points.items():
        if name == "sign":
            continue
        listed[name] = [value] if len(value) == 2 else [value[:2], [value[0] + value[2], value[1] + value[3]]]
    if listed:
        spec["points"] = listed
    if item.get("masks"):
        masks = {role: white_mask(export / path) for role, path in item["masks"].items()}
        spec["masks"] = {role: {"at": shelf.place(m), "base": mean_under(img, m)} for role, m in masks.items()}
        swaps = wall_swaps(img, list(masks.values()), sign_rect)
        if swaps:
            spec["swaps"] = swaps
    return spec


def body_swaps(entry):
    defaults = entry.get("region_defaults") or {}
    out = {}
    for colour, region in zip(entry.get("palette", []), entry.get("palette_regions", [])):
        if region in SWAP_ROLES and region in defaults:
            out[colour.lower()] = [region, lightness_shade(rgb(colour), rgb(defaults[region]))]
    return out


def shifted(point, by):
    return [point[0] + by[0], point[1] + by[1]]


def body_spec(cast, entry, sheet, side_faces):
    walk = entry["walk"]
    anchors = entry.get("accessory_anchors") or {}
    hands = entry.get("hand_walk_offsets") or {}
    layers = entry.get("hand_walk_layers") or {}
    views = {}
    for view in VIEWS:
        rect = entry.get("views", {}).get(view)
        frames = walk.get("frames", {}).get(view)
        if not rect or not frames:
            continue
        base = anchors.get(view, {})
        offsets = walk.get("offsets", {}).get(view, [])
        hand = hands.get(view, [])
        layer = layers.get(view, [])
        walk_frames = []
        for k, frame in enumerate(frames):
            bob = offsets[k] if k < len(offsets) else [0, 0]
            slots = {name: shifted(p, bob) for name, p in base.items()}
            if "hand" in base:
                slots["hand"] = shifted(base["hand"], hand[k] if k < len(hand) else bob)
            entry_frame = {"at": frame[:2], "slots": slots}
            if k < len(layer) and layer[k] in ("over", "under"):
                entry_frame["layers"] = {"hand": layer[k]}
            walk_frames.append(entry_frame)
        views[view] = {"idle": [{"at": rect[:2], "slots": base}], "walk": walk_frames}
    species = [entry["name"], "person"] if entry["name"] == "human" else [entry["name"]]
    return {
        "species": species,
        "atlas": sheet,
        "density": cast.get("pixel_density", 4),
        "size": entry["frame_size"],
        "anchor": entry["anchor"],
        "sideFaces": side_faces,
        "classes": {"head": entry.get("accessory_size_class", 60), "body": entry.get("accessory_body_class", 44)},
        "views": views,
        "swaps": body_swaps(entry),
    }


def accessory_spec(entry, sheet):
    sizes = {}
    for cls, cut in (entry.get("sizes") or {}).items():
        sizes[str(cls)] = {
            view: {"rect": v["rect"], "pivot": v["pivot"], "layer": v.get("layer", "over")}
            for view, v in cut.items()
            if view in VIEWS
        }
    return {
        "slot": entry["attach_to"],
        "basis": "body" if entry.get("size_basis") == "body_width" else "head",
        "atlas": sheet,
        "sizes": sizes,
    }


def pack_cast(cast_dir, out):
    cast = json.loads((cast_dir / "manifest.json").read_text())
    target = out / "cast"
    if target.exists():
        shutil.rmtree(target)
    (target / "bodies").mkdir(parents=True)
    (target / "accessories").mkdir(parents=True)
    side_faces = cast.get("side_faces", "right")
    bodies, accessories = {}, {}
    for entry in cast.get("bodies", []):
        name = Path(entry["atlas"]).name
        shutil.copyfile(cast_dir / entry["atlas"], target / "bodies" / name)
        bodies[entry["name"]] = body_spec(cast, entry, f"cast/bodies/{name}", side_faces)
    for key, entry in (cast.get("accessories") or {}).items():
        name = Path(entry["atlas"]).name
        shutil.copyfile(cast_dir / entry["atlas"], target / "accessories" / name)
        accessories[key] = accessory_spec(entry, f"cast/accessories/{name}")
    return bodies, accessories, cast.get("sources_url")


def main():
    parser = argparse.ArgumentParser(description="Pack the world export and the cast export into the style A atlas and manifest the engine loads.")
    parser.add_argument("--world", type=Path, default=WORLD, help="world export: manifest.json, sprites/ and variants/ (default: frontend/sprites/style-a)")
    parser.add_argument("--cast", type=Path, default=CAST, help="cast export: manifest.json, bodies/ and accessories/ (default: frontend/sprites/cast)")
    parser.add_argument("--out", type=Path, default=SHIPPED, help="folder the engine loads, rewritten in place")
    args = parser.parse_args()

    world = json.loads((args.world / "manifest.json").read_text())
    shelf = Shelf(0)
    things = {item["name"]: thing_spec(args.world, item, shelf) for item in world["items"]}
    packed = Image.new("RGBA", (MAX_W, shelf.bottom()), (0, 0, 0, 0))
    for im, at in shelf.parts:
        packed.alpha_composite(im, tuple(at))
    args.out.mkdir(parents=True, exist_ok=True)
    packed.save(args.out / "atlas.png", optimize=True)

    bodies, accessories, cast_sources = pack_cast(args.cast, args.out)
    sources = {k: v for k, v in (("world", world.get("sources_url")), ("cast", cast_sources)) if isinstance(v, str)}
    manifest = {"version": 1, "atlas": "atlas.png", "sources": sources, "bodies": bodies, "accessories": accessories, "things": things}
    (args.out / "manifest.json").write_text(json.dumps(manifest, separators=(",", ":")))
    print(f"atlas {packed.size[0]}x{packed.size[1]}, {len(things)} things, {len(bodies)} bodies, {len(accessories)} accessories")


if __name__ == "__main__":
    main()
