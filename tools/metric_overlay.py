"""Build English metric overlays from the player's original Screamer Rally PC data."""
import argparse
import hashlib
import os
from pathlib import Path
import struct

from PIL import Image, ImageDraw, ImageFont

ORIGINALS = {
    "DATA/LOCAL.DAT": "83d1b39a5d95c1f00ecc9294c06be62e5e81bd6cf448d7cb89ab1040cdb38cca",
    "GFX/GAME2.DAT": "7ddd09c6103eb017519580f7c54a7c81b8edf2ba56ee5e3213d91936f003cf6c",
    "GFX/GRAF2.DAT": "170a0f1b96c144c016b239966bf13a7dead197730e89a67d22b91ce5acad79cd",
}
CAR_MPH = (112, 110, 113, 115, 120)
GLYPHS = {
    "K": ["1001", "1010", "1100", "1010", "1001"],
    "M": ["10001", "11011", "10101", "10001", "10001"],
    "/": ["001", "001", "010", "100", "100"],
    "H": ["1001", "1001", "1111", "1001", "1001"],
}


def decode_lzw(src, size):
    out = bytearray()
    bit = 0
    width = 9
    previous = b""
    table = []
    while len(out) < size and bit + width <= len(src) * 8:
        code = (int.from_bytes(src[bit//8:bit//8+3], "little") >> (bit % 8)) & ((1 << width) - 1)
        bit += width
        if code == 256:
            table = [bytes([i]) for i in range(256)] + [b"", b""]
            previous = b""
            width = 9
            continue
        if code == 257:
            break
        word = table[code] if code < len(table) else previous + previous[:1]
        out.extend(word)
        if previous and len(table) < 4096:
            table.append(previous + word[:1])
            if len(table) == 1 << width and width < 12:
                width += 1
        previous = word
    if len(out) != size:
        raise ValueError("Invalid LZW stream")
    return out


def sprite(raw, index):
    count, palettes, _ = struct.unpack_from("<3I", raw)
    _, _, w, h, offset, p = struct.unpack_from("<6I", raw, 12 + index * 24)
    colors = 12 + count * 24
    start = colors + palettes * 512 + offset
    palette = struct.unpack_from("<256H", raw, colors + p * 512)
    image = Image.frombytes("P", (w, h), bytes(raw[start:start+w*h]))
    image.putpalette([v for c in palette for v in
                      ((c >> 11) * 255 // 31, ((c >> 5) & 63) * 255 // 63, (c & 31) * 255 // 31)])
    return image, start


def gauge(image):
    # The top row is the black bottom edge of the dashboard. The rows below
    # use transparent palette index zero. Copy that backing from beside MPH.
    high = image.width == 143
    left, right, top, center = (62, 92, 105, 77) if high else (28, 46, 53, 37)
    for y in range(top, image.height):
        background = image.getpixel((right + 1, y))
        ImageDraw.Draw(image).line((left, y, right, y), fill=background)
    letters = [GLYPHS[c] for c in "KM/H"]
    width = sum(len(letter[0]) for letter in letters) + len(letters) - 1
    mask = Image.new("1", (width, 5))
    x = 0
    for letter in letters:
        for y, row in enumerate(letter):
            for dx, value in enumerate(row):
                if value == "1":
                    mask.putpixel((x+dx, y), 1)
        x += len(letter[0]) + 1
    if high:
        mask = mask.resize((23, 7), Image.Resampling.NEAREST)
    x = center - mask.width // 2
    for dx, dy in ((-1,0),(1,0),(0,1),(0,-1)):
        image.paste(31, (x+dx, top+dy, x+dx+mask.width, top+dy+mask.height), mask)
    image.paste(19 if high else 30, (x, top, x+mask.width, top+mask.height), mask)


def car_plate(image, mph, font):
    scale = image.width / 374
    left, right = round(177*scale), round(284*scale)
    colors = image.getpalette()
    def rgb(i):
        return colors[3*i:3*i+3]
    # Keep the brushed metal behind the text and both adjacent emblems intact.
    for y in range(round(9*scale), round(37*scale)):
        for x in range(left, right):
            if max(rgb(image.getpixel((x,y)))) < 145:
                image.putpixel((x,y), image.getpixel((right + x % 2, y)))
    label = f"{round(mph * 1.609344)} km/h"
    bbox = font.getbbox(label)
    mask = Image.new("L", (bbox[2]-bbox[0], bbox[3]-bbox[1]))
    ImageDraw.Draw(mask).text((-bbox[0],-bbox[1]),label,font=font,fill=255)
    mask = mask.resize((round(99*scale), round(19*scale)), Image.Resampling.LANCZOS)
    px, py = round(180*scale), round(13*scale)
    # Match the original antialiased black print with the plate's own palette.
    neutral = [i for i in range(1,256) if max(rgb(i))-min(rgb(i)) < 25]
    gray = {v:min(neutral,key=lambda i:abs(sum(rgb(i))//3-v)) for v in range(256)}
    for y in range(mask.height):
        for x in range(mask.width):
            alpha = mask.getpixel((x,y))
            if alpha:
                bg = sum(rgb(image.getpixel((px+x,py+y))))//3
                image.putpixel((px+x,py+y),gray[(bg*(255-alpha)+12*alpha)//255])


def convert_entry(name, raw, font):
    if name == "GAME.TXT":
        if raw.splitlines().count(b"MPH") != 1:
            raise ValueError("Unexpected speed unit")
        return raw.replace(b"MPH", b"KMH")
    if name.startswith("GAME"):
        img, start = sprite(raw, 22)
        gauge(img)
        raw[start:start+img.width*img.height] = img.tobytes()
    else:
        for i, mph in enumerate(CAR_MPH, start=94):
            img, start = sprite(raw, i)
            car_plate(img, mph, font)
            raw[start:start+img.width*img.height] = img.tobytes()
    return raw


def overlay(data, font):
    directory_size, = struct.unpack_from("<I", data, len(data)-4)
    directory = bytearray(data[-directory_size-4:])
    output = bytearray()
    for i in range(0, directory_size, 32):
        name = bytes(v ^ 58 for v in directory[i:i+16]).split(b"\0")[0].decode("ascii")
        if not name:
            break
        offset, stored, size, flags = struct.unpack_from("<4I", directory, i+16)
        payload = data[offset:offset+stored]
        if name in {"GAME.TXT","GAMEH.SPR","GAMEL.SPR","MENUH.SPR","MENUL.SPR"}:
            raw = decode_lzw(payload, size) if flags & 2 else bytearray(payload)
            payload = convert_entry(name, raw, font)
            # DAT supports ordinary stored entries. Size stays identical to the decoded original.
            size = len(payload)
            flags = 0
        struct.pack_into("<4I", directory, i+16, len(output), len(payload), size, flags)
        output.extend(payload)
    return output + directory


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("original", type=Path, help="Folder containing the three original English DAT files")
    parser.add_argument("output", type=Path, help="New overlay directory")
    parser.add_argument("--font", default="arialbi.ttf", help="Path to a bold italic TrueType font")
    args = parser.parse_args()
    font = ImageFont.truetype(args.font, 72)
    converted = {}
    for relative, digest in ORIGINALS.items():
        src = args.original / relative
        if not src.exists():
            src = args.original / Path(relative).name
        data = src.read_bytes()
        if hashlib.sha256(data).hexdigest() != digest:
            raise ValueError(f"Unexpected original file: {src}")
        dest = args.output / relative
        if dest.resolve() == src.resolve():
            raise ValueError("Choose an output directory separate from the originals")
        converted[dest] = overlay(data, font)
    for dest, payload in converted.items():
        dest.parent.mkdir(parents=True, exist_ok=True)
        temp = dest.with_suffix(".tmp")
        temp.write_bytes(payload)
        os.replace(temp, dest)


if __name__ == "__main__":
    main()
