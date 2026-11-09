<#
.SYNOPSIS
    Regenerates the application icon: assets/dock.ico (16, 20, 24, 32, 40, 48, 64, 256 px) and
    assets/icon-256.png.

.DESCRIPTION
    How to regenerate (PowerShell 7, nothing to install; run from the repo root):

        pwsh -NoProfile -File tools/make-icon.ps1
        pwsh -NoProfile -File tools/make-icon.ps1 -Preview <dir>   # also writes enlarged previews

    then rebuild (`cargo build --release --locked`): build.rs embeds assets/dock.ico as icon id 1.

    The design lives in assets/icon.svg (the 256 px master); the geometry below is the same, written
    per size so every stroke lands on whole pixels. Keep both in sync when the design changes.

    Design: a pure black rounded tile with a 1 px dark grey rim (#3A3A3A, keeps the edge visible on
    dark title bars and dark Alt+Tab), a thin white outline concentric with the tile (the screen) and a
    short white pill low in the centre (the dock floating at the bottom of the screen). At 16 and 20 px
    the outline is dropped (it would blur into the rim) and the pill is drawn larger.

    Rendering: each pixel is the average of 16 x 16 samples of exact rounded-rectangle shapes, so the
    output is deterministic and needs no graphics library. Small entries are 32-bit BMP entries with
    alpha, the 256 px entry is PNG-compressed.
#>
param(
    [string]$Out = (Join-Path $PSScriptRoot '..\assets'),
    [string]$Preview = ''
)
$ErrorActionPreference = 'Stop'

Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.IO.Compression;
using System.Collections.Generic;

public static class DockIcon
{
    // Rounded rectangle [X0, X1) x [Y0, Y1) with corner radius R, in pixels.
    struct RRect
    {
        public double X0, Y0, X1, Y1, R;
        public RRect(double x0, double y0, double x1, double y1, double r) { X0 = x0; Y0 = y0; X1 = x1; Y1 = y1; R = r; }
        public RRect Inset(double d) { return new RRect(X0 + d, Y0 + d, X1 - d, Y1 - d, Math.Max(R - d, 0)); }
        public bool Contains(double px, double py)
        {
            double r = Math.Min(R, Math.Min(X1 - X0, Y1 - Y0) / 2);
            double qx = Math.Abs(px - (X0 + X1) / 2) - ((X1 - X0) / 2 - r);
            double qy = Math.Abs(py - (Y0 + Y1) / 2) - ((Y1 - Y0) / 2 - r);
            double ox = Math.Max(qx, 0), oy = Math.Max(qy, 0);
            return Math.Sqrt(ox * ox + oy * oy) + Math.Min(Math.Max(qx, qy), 0) - r < 0;
        }
    }

    // Geometry of one size. variant 0 = owner's sketch (small square in the centre),
    // 1 = pill in the centre, 2 = pill low in the outline (the chosen design).
    struct Geo
    {
        public RRect Tile, Face, Outer, Inner, Mark;
        public bool Outline;
    }

    static int Even(double v) { int n = (int)Math.Round(v / 2, MidpointRounding.AwayFromZero) * 2; return n; }

    static Geo For(int s, int variant)
    {
        var g = new Geo();
        int m = (s + 16) / 32;                    // margin: 1 px up to 40, 2 at 48/64, 8 at 256
        int t = s - 2 * m;                        // tile side
        double r = 0.22 * t;                      // tile corner radius
        g.Tile = new RRect(m, m, s - m, s - m, r);
        g.Outline = s >= 24;
        // Dark grey rim: at 16/20 (no outline) and from 64 up. From 24 to 48 the white outline sits
        // 2-4 px from the edge and already draws it; a rim there would turn the edge into mush.
        int rim = !g.Outline || s >= 64 ? Math.Max(1, (int)Math.Round(s / 128.0)) : 0;
        g.Face = g.Tile.Inset(rim);
        int gap = Math.Max(2, (int)Math.Round(t * 0.075));       // tile edge -> outline
        int stroke = Math.Max(1, (int)Math.Round(s / 64.0));     // outline thickness
        g.Outer = g.Tile.Inset(gap);
        g.Inner = g.Outer.Inset(stroke);
        if (variant == 0)
        {
            int side = Math.Max(2, Even(t * 0.15));
            double o = (s - side) / 2.0;
            g.Mark = new RRect(o, o, o + side, o + side, side * 0.22);
        }
        else
        {
            int w = Even(t * (s <= 20 ? 0.58 : 0.44));
            int h = Math.Max(2, Even(t * 0.085));
            double x = (s - w) / 2.0;
            double y;
            if (variant == 1) y = (s - h) / 2.0;
            else
            {
                // Low, floating above the bottom of the outline like the bar above the screen edge.
                double bottom = g.Outline ? g.Inner.Y1 - 1.6 * gap : g.Face.Y1 - Math.Max(2, gap);
                y = Math.Round(bottom) - h;
            }
            g.Mark = new RRect(x, y, x + w, y + h, h / 2.0);
        }
        return g;
    }

    // Straight-alpha RGBA, top-down.
    public static byte[] Render(int s, int variant)
    {
        var g = For(s, variant);
        const int N = 16;
        var px = new byte[s * s * 4];
        for (int y = 0; y < s; y++)
            for (int x = 0; x < s; x++)
            {
                double cr = 0, cg = 0, cb = 0, ca = 0;
                for (int j = 0; j < N; j++)
                    for (int i = 0; i < N; i++)
                    {
                        double sx = x + (i + 0.5) / N, sy = y + (j + 0.5) / N;
                        if (!g.Tile.Contains(sx, sy)) continue;
                        int c = 0x3A;                                     // rim
                        if (g.Face.Contains(sx, sy)) c = 0x00;           // black face
                        if (g.Outline && g.Outer.Contains(sx, sy) && !g.Inner.Contains(sx, sy)) c = 0xFF;
                        if (g.Mark.Contains(sx, sy)) c = 0xFF;
                        cr += c; cg += c; cb += c; ca += 1;
                    }
                int o = (y * s + x) * 4;
                if (ca > 0)
                {
                    px[o] = (byte)Math.Round(cr / ca); px[o + 1] = (byte)Math.Round(cg / ca); px[o + 2] = (byte)Math.Round(cb / ca);
                    px[o + 3] = (byte)Math.Round(255 * ca / (N * N));
                }
            }
        return px;
    }

    static uint[] crcTable;
    static uint Crc(byte[] b, int off, int len)
    {
        if (crcTable == null)
        {
            crcTable = new uint[256];
            for (uint n = 0; n < 256; n++)
            {
                uint c = n;
                for (int k = 0; k < 8; k++) c = (c & 1) != 0 ? 0xEDB88320u ^ (c >> 1) : c >> 1;
                crcTable[n] = c;
            }
        }
        uint crc = 0xFFFFFFFFu;
        for (int i = off; i < off + len; i++) crc = crcTable[(crc ^ b[i]) & 0xFF] ^ (crc >> 8);
        return crc ^ 0xFFFFFFFFu;
    }

    static void Be32(Stream st, uint v) { st.WriteByte((byte)(v >> 24)); st.WriteByte((byte)(v >> 16)); st.WriteByte((byte)(v >> 8)); st.WriteByte((byte)v); }
    static void Le16(Stream st, int v) { st.WriteByte((byte)v); st.WriteByte((byte)(v >> 8)); }
    static void Le32(Stream st, int v) { Le16(st, v & 0xFFFF); Le16(st, (v >> 16) & 0xFFFF); }

    static void Chunk(Stream st, string type, byte[] data)
    {
        var buf = new byte[4 + data.Length];
        for (int i = 0; i < 4; i++) buf[i] = (byte)type[i];
        Array.Copy(data, 0, buf, 4, data.Length);
        Be32(st, (uint)data.Length);
        st.Write(buf, 0, buf.Length);
        Be32(st, Crc(buf, 0, buf.Length));
    }

    // RGBA (straight alpha) to PNG bytes.
    public static byte[] Png(byte[] rgba, int w, int h)
    {
        var ms = new MemoryStream();
        ms.Write(new byte[] { 137, 80, 78, 71, 13, 10, 26, 10 }, 0, 8);
        var ihdr = new MemoryStream();
        Be32(ihdr, (uint)w); Be32(ihdr, (uint)h);
        ihdr.Write(new byte[] { 8, 6, 0, 0, 0 }, 0, 5);
        Chunk(ms, "IHDR", ihdr.ToArray());
        var raw = new MemoryStream();
        for (int y = 0; y < h; y++) { raw.WriteByte(0); raw.Write(rgba, y * w * 4, w * 4); }
        var z = new MemoryStream();
        using (var zs = new ZLibStream(z, CompressionLevel.SmallestSize, true)) { raw.Position = 0; raw.CopyTo(zs); }
        Chunk(ms, "IDAT", z.ToArray());
        Chunk(ms, "IEND", new byte[0]);
        return ms.ToArray();
    }

    // 32-bit BMP icon entry: BITMAPINFOHEADER (double height), BGRA bottom-up, then the AND mask.
    static byte[] Dib(byte[] rgba, int s)
    {
        var ms = new MemoryStream();
        Le32(ms, 40); Le32(ms, s); Le32(ms, s * 2); Le16(ms, 1); Le16(ms, 32);
        Le32(ms, 0); Le32(ms, 0); Le32(ms, 0); Le32(ms, 0); Le32(ms, 0); Le32(ms, 0);
        for (int y = s - 1; y >= 0; y--)
            for (int x = 0; x < s; x++)
            {
                int o = (y * s + x) * 4;
                ms.WriteByte(rgba[o + 2]); ms.WriteByte(rgba[o + 1]); ms.WriteByte(rgba[o]); ms.WriteByte(rgba[o + 3]);
            }
        int stride = ((s + 31) / 32) * 4;
        for (int y = s - 1; y >= 0; y--)
        {
            var row = new byte[stride];
            for (int x = 0; x < s; x++)
                if (rgba[(y * s + x) * 4 + 3] == 0) row[x / 8] |= (byte)(0x80 >> (x % 8));
            ms.Write(row, 0, stride);
        }
        return ms.ToArray();
    }

    public static byte[] Ico(int[] sizes, int variant)
    {
        var images = new List<byte[]>();
        foreach (int s in sizes)
        {
            var rgba = Render(s, variant);
            images.Add(s >= 256 ? Png(rgba, s, s) : Dib(rgba, s));
        }
        var ms = new MemoryStream();
        Le16(ms, 0); Le16(ms, 1); Le16(ms, sizes.Length);
        int offset = 6 + 16 * sizes.Length;
        for (int i = 0; i < sizes.Length; i++)
        {
            int s = sizes[i];
            ms.WriteByte((byte)(s >= 256 ? 0 : s)); ms.WriteByte((byte)(s >= 256 ? 0 : s));
            ms.WriteByte(0); ms.WriteByte(0); Le16(ms, 1); Le16(ms, 32);
            Le32(ms, images[i].Length); Le32(ms, offset);
            offset += images[i].Length;
        }
        foreach (var im in images) ms.Write(im, 0, im.Length);
        return ms.ToArray();
    }

    // Preview sheet: one row per variant, each size composited over black, #202020 and white,
    // enlarged `zoom` times (nearest neighbour), plus the 256 px icon at 1:1.
    public static byte[] Sheet(int[] sizes, int[] variants, int zoom)
    {
        int[] bgs = { 0x00, 0x20, 0xFF };
        int cell = 64 * zoom / 4 + 8;
        int rowH = Math.Max(bgs.Length * cell, 256 + 16);
        int w = sizes.Length * cell + 256 + 16, h = variants.Length * rowH;
        var img = new byte[w * h * 4];
        for (int i = 0; i < w * h; i++) { img[i * 4] = 0x80; img[i * 4 + 1] = 0x80; img[i * 4 + 2] = 0x80; img[i * 4 + 3] = 255; }
        for (int vi = 0; vi < variants.Length; vi++)
        {
            for (int b = 0; b < bgs.Length; b++)
                for (int si = 0; si < sizes.Length; si++)
                {
                    int s = sizes[si];
                    int z = s >= 64 ? 1 : Math.Max(1, (cell - 8) / s);
                    var px = Render(s, variants[vi]);
                    int ox = si * cell + 4, oy = vi * rowH + b * cell + 4;
                    for (int y = 0; y < cell - 8; y++)
                        for (int x = 0; x < cell - 8; x++)
                        {
                            int sx = x / z, sy = y / z;
                            double a = 0, c = 0;
                            if (sx < s && sy < s) { int o = (sy * s + sx) * 4; a = px[o + 3] / 255.0; c = px[o]; }
                            byte v = (byte)Math.Round(c * a + bgs[b] * (1 - a));
                            int d = ((oy + y) * w + ox + x) * 4;
                            img[d] = v; img[d + 1] = v; img[d + 2] = v; img[d + 3] = 255;
                        }
                }
            var big = Render(256, variants[vi]);
            for (int y = 0; y < 256; y++)
                for (int x = 0; x < 256; x++)
                {
                    int o = (y * 256 + x) * 4; double a = big[o + 3] / 255.0;
                    byte v = (byte)Math.Round(big[o] * a + 0x20 * (1 - a));
                    int d = ((vi * rowH + 8 + y) * w + sizes.Length * cell + 8 + x) * 4;
                    img[d] = v; img[d + 1] = v; img[d + 2] = v; img[d + 3] = 255;
                }
        }
        return Png(img, w, h);
    }
}
'@

$variant = 2
$sizes = 16, 20, 24, 32, 40, 48, 64, 256
$Out = [System.IO.Path]::GetFullPath($Out)
New-Item -ItemType Directory -Force $Out | Out-Null
[System.IO.File]::WriteAllBytes((Join-Path $Out 'dock.ico'), [DockIcon]::Ico($sizes, $variant))
[System.IO.File]::WriteAllBytes((Join-Path $Out 'icon-256.png'), [DockIcon]::Png([DockIcon]::Render(256, $variant), 256, 256))
Write-Host "wrote $Out\dock.ico and $Out\icon-256.png"

if ($Preview) {
    $Preview = [System.IO.Path]::GetFullPath($Preview)
    New-Item -ItemType Directory -Force $Preview | Out-Null
    $small = 16, 20, 24, 32, 40, 48
    [System.IO.File]::WriteAllBytes((Join-Path $Preview 'icon-variants.png'), [DockIcon]::Sheet($small, @(0, 1, 2), 8))
    Write-Host "wrote $Preview\icon-variants.png (rows: square, centred pill, low pill)"
}
