#!/usr/bin/env python3
"""Natural Earth の国境と都市を、旅の地図が読むテキスト形式に変換する。

入力(どちらもパブリックドメイン。https://github.com/nvkelso/natural-earth-vector の geojson/):
- ne_50m_admin_0_countries.geojson … 国の境界と日本語名(NAME_JA)
- ne_10m_populated_places.geojson   … 都市の位置・人口と日本語名(NAME_JA)

出力:
- world_countries.txt: 「@<国コード> <日本語名>」の名前表と、1 行 = 1 リングの
  「<国コード> <経度>,<緯度> ...」。Douglas-Peucker(0.02 度)で簡略化し、ごく小さい島は省く。
- world_cities.txt: 「<経度>,<緯度> <人口> <日本語名>」。日本以外で日本語名のある都市だけ。

使い方:
  python3 tools/world_geojson_to_txt.py ne_50m_admin_0_countries.geojson \
      ne_10m_populated_places.geojson crates/miharashi-render/assets
"""
import json
import sys

TOLERANCE = 0.02
MIN_AREA = 0.002  # 度^2。これより小さいリングは省く(小島)
# 表示に合わせて言い換える国名
NAME_OVERRIDES = {"TWN": "台湾", "USA": "アメリカ", "GBR": "イギリス", "KOR": "韓国", "PRK": "北朝鮮"}


def simplify(points, tol):
    """Douglas-Peucker(端点は残す)。"""
    if len(points) < 3:
        return points
    keep = [False] * len(points)
    keep[0] = keep[-1] = True
    stack = [(0, len(points) - 1)]
    while stack:
        a, b = stack.pop()
        (x1, y1), (x2, y2) = points[a], points[b]
        dx, dy = x2 - x1, y2 - y1
        norm = (dx * dx + dy * dy) ** 0.5
        best, idx = 0.0, None
        for i in range(a + 1, b):
            x0, y0 = points[i]
            if norm == 0:
                d = ((x0 - x1) ** 2 + (y0 - y1) ** 2) ** 0.5
            else:
                d = abs(dy * x0 - dx * y0 + x2 * y1 - y2 * x1) / norm
            if d > best:
                best, idx = d, i
        if idx is not None and best > tol:
            keep[idx] = True
            stack += [(a, idx), (idx, b)]
    return [p for p, k in zip(points, keep) if k]


def area(ring):
    return abs(sum(x1 * y2 - x2 * y1 for (x1, y1), (x2, y2) in zip(ring, ring[1:] + ring[:1]))) / 2


def countries(path, out):
    with open(path, encoding="utf-8") as f:
        features = json.load(f)["features"]
    names, rings = [], []
    for feature in features:
        p = feature["properties"]
        code = p.get("ADM0_A3") or p.get("ISO_A3")
        name = NAME_OVERRIDES.get(code) or p.get("NAME_JA") or p.get("NAME")
        names.append(f"@{code} {name}")
        g = feature["geometry"]
        polygons = g["coordinates"] if g["type"] == "MultiPolygon" else [g["coordinates"]]
        for polygon in polygons:
            for ring in polygon:
                ring = [(round(x, 3), round(y, 3)) for x, y in ring]
                if area(ring) < MIN_AREA:
                    continue
                s = simplify(ring, TOLERANCE)
                if len(s) >= 4:
                    rings.append(f"{code} " + " ".join(f"{x:g},{y:g}" for x, y in s))
    with open(out, "w", encoding="utf-8") as f:
        f.write("# 国境(旅の地図と国名の判定用)。出典: Natural Earth 1:50m Admin 0 – Countries\n")
        f.write("# (パブリックドメイン)。tools/world_geojson_to_txt.py で生成\n")
        f.write("# 形式: 「@<国コード> <日本語名>」と、1 行 = 1 リング「<国コード> <経度>,<緯度> ...」\n")
        f.write("\n".join(names + rings) + "\n")
    return len(names), len(rings)


def cities(path, out):
    with open(path, encoding="utf-8") as f:
        features = json.load(f)["features"]
    rows = []
    for feature in features:
        p = feature["properties"]
        name = p.get("NAME_JA")
        if not name or p.get("ADM0_A3") == "JPN":
            continue
        lon, lat = feature["geometry"]["coordinates"][:2]
        rows.append(f"{lon:.3f},{lat:.3f} {int(p.get('POP_MAX') or 0)} {name.replace(' ', '')}")
    with open(out, "w", encoding="utf-8") as f:
        f.write("# 日本以外の都市(旅の地名用)。出典: Natural Earth 1:10m Populated Places\n")
        f.write("# (パブリックドメイン)。tools/world_geojson_to_txt.py で生成\n")
        f.write("# 形式: 「<経度>,<緯度> <人口> <日本語名>」\n")
        f.write("\n".join(rows) + "\n")
    return len(rows)


def main():
    countries_path, cities_path, out_dir = sys.argv[1:4]
    n, r = countries(countries_path, f"{out_dir}/world_countries.txt")
    c = cities(cities_path, f"{out_dir}/world_cities.txt")
    print(f"countries={n} rings={r} cities={c}", file=sys.stderr)


if __name__ == "__main__":
    main()
