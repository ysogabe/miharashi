#!/usr/bin/env python3
"""都道府県ポリゴンの GeoJSON を、アラートの地図が読むテキスト形式に変換する。

入力は GEOFRAME の display-final タグの app/vendor/pref.geojson(Natural Earth 1:10m
admin-1 から日本の 47 件を抜き出して簡略化したもの。パブリックドメイン)。
出力は 1 行 = 1 リングで「<都道府県コード 2 桁> <経度>,<緯度> ...」。

使い方: python3 tools/pref_geojson_to_txt.py pref.geojson > crates/miharashi-render/assets/japan_prefs.txt
"""
import json
import sys

HEADER = """\
# 都道府県ポリゴン(地震アラートの地図用)
# 出典: Natural Earth 1:10m Admin 1 – States, Provinces(パブリックドメイン)。日本の 47 件を
#       抜き出し、Douglas-Peucker(0.005 度)と小数 4 桁への丸めで簡略化したもの
#       (GEOFRAME の display-final タグの app/vendor/pref.geojson)。
# 形式: 1 行 = 1 リング「<都道府県コード 2 桁> <経度>,<緯度> ...」。tools/pref_geojson_to_txt.py で生成。"""


def main():
    with open(sys.argv[1], encoding="utf-8") as f:
        features = json.load(f)["features"]
    print(HEADER)
    for feature in features:
        code = feature["properties"]["code"]
        geometry = feature["geometry"]
        polygons = (geometry["coordinates"] if geometry["type"] == "MultiPolygon"
                    else [geometry["coordinates"]])
        for polygon in polygons:
            for ring in polygon:
                points = " ".join(f"{lon:g},{lat:g}" for lon, lat in ring)
                print(f"{code} {points}")


if __name__ == "__main__":
    main()
