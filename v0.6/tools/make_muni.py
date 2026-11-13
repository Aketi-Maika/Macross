"""国土地理院の市区町村コード表（muni.js）から server/assets/muni.json を作る

使い方: python tools/make_muni.py
（地図で住所を出すとき、逆ジオコーダが返す市区町村コードを名前にするために使う。
  市区町村の合併などで表が古くなったら実行し直す）
"""
import json
import pathlib
import re
import urllib.request

URL = "https://maps.gsi.go.jp/js/muni.js"
OUT = pathlib.Path(__file__).resolve().parent.parent / "server" / "assets" / "muni.json"

text = urllib.request.urlopen(URL, timeout=30).read().decode("utf-8")
table = {}
for code, value in re.findall(r"MUNI_ARRAY\[\"(\d+)\"\]\s*=\s*'([^']*)'", text):
    parts = value.split(",")
    if len(parts) < 4:
        continue
    pref, muni = parts[1], parts[3].replace("　", "")
    table[str(int(code))] = pref + muni
OUT.write_text(json.dumps(table, ensure_ascii=False, separators=(",", ":"), sort_keys=True), encoding="utf-8")
print(f"{len(table)} 件 -> {OUT}")
