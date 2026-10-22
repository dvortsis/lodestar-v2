#!/usr/bin/env python3
"""Check the palette table in docs/themes.md (stdlib only).

Reads every fenced ```yaml block, then prints per family and mode:
text contrast on background and surface, the weakest raw categorical
contrast on surface, and the minimum pairwise ΔE_ok (OKLab distance x 100)
for the raw categorical set, its estimated data variants (OKLCH lightness
shifted until >= 3:1 on surface, as the compiler does) and the data
variants under simulated deuteranopia and protanopia (Machado 2009, full
severity). Exits 1 if a hex is malformed or a text gate fails.

    python3 docs/themes_check.py [docs/themes.md] [--markdown]
"""
import math
import re
import sys

HEX = re.compile(r'^#[0-9A-F]{6}$')
KEYS = ['background', 'surface', 'text', 'primary', 'accent', 'categorical']


def parse(path):
    text = open(path, encoding='utf-8').read()
    fams = []
    for block in re.findall(r'```yaml\n(.*?)```', text, re.S):
        fam, mode = {}, None
        for line in block.splitlines():
            m = re.match(r'^(\w+):\s*(.*)$', line)
            if m and m.group(1) in ('light', 'dark'):
                mode = m.group(1)
                fam[mode] = {}
            elif m:
                fam[m.group(1)] = m.group(2).strip().strip('"')
            elif mode and (m := re.match(r'^\s+(\w+):\s*(.*)$', line)):
                v = m.group(2).strip()
                if v.startswith('['):
                    v = [s.strip().strip('"') for s in v.strip('[]').split(',')]
                else:
                    v = v.strip('"')
                fam[mode][m.group(1)] = v
        fams.append(fam)
    return fams


def lin(c):
    return c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4


def delin(c):
    c = max(0.0, min(1.0, c))
    return 12.92 * c if c <= 0.0031308 else 1.055 * c ** (1 / 2.4) - 0.055


def rgb(h):
    return [lin(int(h[i:i + 2], 16) / 255) for i in (1, 3, 5)]


def luminance(lrgb):
    return 0.2126 * lrgb[0] + 0.7152 * lrgb[1] + 0.0722 * lrgb[2]


def contrast(a, b):
    la, lb = luminance(rgb(a)), luminance(rgb(b))
    return (max(la, lb) + 0.05) / (min(la, lb) + 0.05)


def oklab(lrgb):
    r, g, b = lrgb
    l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b) ** (1 / 3)
    m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b) ** (1 / 3)
    s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b) ** (1 / 3)
    return (0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
            1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
            0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s)


def oklab_to_lrgb(L, a, b):
    l = (L + 0.3963377774 * a + 0.2158037573 * b) ** 3
    m = (L - 0.1055613458 * a - 0.0638541728 * b) ** 3
    s = (L - 0.0894841775 * a - 1.2914855480 * b) ** 3
    return [4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
            -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
            -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s]


def to_hex(lrgb):
    return '#' + ''.join(f'{round(delin(c) * 255):02X}' for c in lrgb)


def data_variant(h, surface):
    """Shift OKLCH lightness away from the surface until >= 3:1 (hue kept)."""
    if contrast(h, surface) >= 3:
        return h
    L, a, b = oklab(rgb(h))
    C, H = math.hypot(a, b), math.atan2(b, a)
    step = -0.002 if luminance(rgb(surface)) > 0.18 else 0.002
    while 0 < L < 1:
        L += step
        c = C
        while c > 0:  # reduce chroma until in gamut
            v = oklab_to_lrgb(L, c * math.cos(H), c * math.sin(H))
            if all(-1e-6 <= x <= 1 + 1e-6 for x in v):
                break
            c -= 0.002
        cand = to_hex(oklab_to_lrgb(L, max(c, 0) * math.cos(H), max(c, 0) * math.sin(H)))
        if contrast(cand, surface) >= 3:
            return cand
    return h


PROTAN = [[0.152286, 1.052583, -0.204868], [0.114503, 0.786281, 0.099216], [-0.003882, -0.048116, 1.051998]]
DEUTAN = [[0.367322, 0.860646, -0.227968], [0.280085, 0.672501, 0.047413], [-0.011820, 0.042940, 0.968881]]


def simulate(h, mat):
    c = rgb(h)
    return [max(0.0, min(1.0, sum(mat[i][j] * c[j] for j in range(3)))) for i in range(3)]


def min_de(colors, f=rgb):
    labs = [oklab(f(c)) for c in colors]
    best = (1e9, None)
    for i in range(len(labs)):
        for j in range(i + 1, len(labs)):
            d = 100 * math.dist(labs[i], labs[j])
            if d < best[0]:
                best = (d, (i + 1, j + 1))
    return best


def check(fams):
    rows, errors = [], []
    for fam in fams:
        fid = fam.get('id', '?')
        for mode in ('light', 'dark'):
            t = fam.get(mode)
            if not t or any(k not in t for k in KEYS):
                errors.append(f'{fid}/{mode}: missing keys')
                continue
            vals = [t[k] for k in KEYS[:5]] + list(t['categorical'])
            bad = [v for v in vals if not HEX.match(v)]
            if bad or len(t['categorical']) != 6:
                errors.append(f'{fid}/{mode}: bad hex or categorical count {bad}')
                continue
            need = 7.0 if fid == 'high-contrast' else 4.5
            cb, cs = contrast(t['text'], t['background']), contrast(t['text'], t['surface'])
            if min(cb, cs) < need:
                errors.append(f'{fid}/{mode}: text {min(cb, cs):.2f}:1 < {need}:1')
            cat = t['categorical']
            data = [data_variant(c, t['surface']) for c in cat]
            rows.append(dict(
                id=fid, mode=mode, need=need, tb=cb, ts=cs,
                cmin=min(contrast(c, t['surface']) for c in cat),
                raw=min_de(cat), data=min_de(data),
                deu=min_de(data, lambda h: simulate(h, DEUTAN)),
                pro=min_de(data, lambda h: simulate(h, PROTAN)),
                variants=[d for c, d in zip(cat, data) if c != d]))
    return rows, errors


def fmt(d):
    return f'{d[0]:.1f} ({d[1][0]}–{d[1][1]})'


def main():
    args = [a for a in sys.argv[1:] if not a.startswith('--')]
    rows, errors = check(parse(args[0] if args else 'docs/themes.md'))
    if '--markdown' in sys.argv:
        print('| Theme | Text/bg | Text/surface | Min cat. on surface | ΔE raw | ΔE data | ΔE deutan | ΔE protan | Flags |')
        print('|---|---|---|---|---|---|---|---|---|')
    for r in rows:
        flags = []
        if min(r['tb'], r['ts']) < r['need']:
            flags.append(f"**FAIL text < {r['need']:g}:1**")
        if r['data'][0] < 5:
            flags.append('warn ΔE < 5')
        if min(r['deu'][0], r['pro'][0]) < 5:
            flags.append('CVD < 5')
        if r['cmin'] < 3:
            flags.append(f"{len(r['variants'])} data variant(s)")
        if '--markdown' in sys.argv:
            print(f"| {r['id']} {r['mode']} | {r['tb']:.2f} | {r['ts']:.2f} | {r['cmin']:.2f} | "
                  f"{fmt(r['raw'])} | {fmt(r['data'])} | {fmt(r['deu'])} | {fmt(r['pro'])} | {'; '.join(flags) or 'ok'} |")
        else:
            print(f"{r['id']:16} {r['mode']:5} text {r['tb']:5.2f}/{r['ts']:5.2f}  cat>={r['cmin']:4.2f}  "
                  f"dE raw {fmt(r['raw']):12} data {fmt(r['data']):12} deu {fmt(r['deu']):12} pro {fmt(r['pro']):12} "
                  f"{' '.join(r['variants'])}  {'; '.join(flags)}")
    for e in errors:
        print('ERROR', e, file=sys.stderr)
    sys.exit(1 if errors else 0)


if __name__ == '__main__':
    main()
