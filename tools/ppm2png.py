"""Convert P6 PPM (as written by RENDER_DUMP_FRAME) to PNG for inspection."""
import struct, sys, zlib

def ppm_to_png(src: str, dst: str) -> None:
    with open(src, "rb") as f:
        data = f.read()
    # Header: P6\n<w> <h>\n255\n
    magic_end = data.index(b"\n")
    assert data[:magic_end] == b"P6", "not a P6 ppm"
    rest = data[magic_end + 1:]
    dims_line, rest = rest.split(b"\n", 1)
    w, h = dims_line.split()
    width, height = int(w), int(h)
    raw = rest.split(b"\n", 1)[1]  # skip "255" line
    assert len(raw) >= width * height * 3, "truncated pixel data"
    raw = raw[: width * height * 3]

    def chunk(tag: bytes, payload: bytes) -> bytes:
        out = struct.pack(">I", len(payload)) + tag + payload
        return out + struct.pack(">I", zlib.crc32(tag + payload) & 0xFFFFFFFF)

    stride = width * 3
    scanlines = b"".join(b"\x00" + raw[y * stride : (y + 1) * stride] for y in range(height))
    png = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(scanlines, 6))
        + chunk(b"IEND", b"")
    )
    with open(dst, "wb") as f:
        f.write(png)
    print(f"{src} ({width}x{height}) -> {dst}")

if __name__ == "__main__":
    ppm_to_png(sys.argv[1], sys.argv[2])
