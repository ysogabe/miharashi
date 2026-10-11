#!/usr/bin/env python3
"""長時間試験の集計。`journalctl -t miharashi -o short-iso` の stats 行から、RSS・fd・フレーム時間・
更新停止(ticks が増えない区間)を要約する。

使い方: sudo journalctl -t miharashi -o short-iso --since "2026-10-12 02:00" | python3 tools/soak_summary.py
"""
import re
import sys
from datetime import datetime

LINE = re.compile(
    r"^(?P<ts>\S+) .*stats: t=(?P<t>\d+)s rss=(?P<rss>\d+)MB fds=(?P<fds>\d+) frames=(?P<frames>\d+) "
    r"worst_frame=(?P<worst>\d+)ms ticks=(?P<ticks>\d+) keys=(?P<keys>\d+) screen=(?P<screen>\w+) "
    r"photo=(?P<photo>\d+) feed=(?P<feed>.*?) cpu="
)


def main() -> int:
    rows, events = [], {"seat_disabled": 0, "seat_enabled": 0, "restored": 0, "errors": 0, "starts": 0}
    for line in sys.stdin:
        m = LINE.match(line)
        if m:
            d = m.groupdict()
            rows.append({k: (int(v) if v.isdigit() else v) for k, v in d.items()})
            continue
        if "seat: 無効化" in line:
            events["seat_disabled"] += 1
        elif "seat: 有効化" in line:
            events["seat_enabled"] += 1
        elif "seat: 表示を復元" in line:
            events["restored"] += 1
        elif "config:" in line:
            events["starts"] += 1
        elif "Error" in line or "エラー" in line or "再試行" in line:
            events["errors"] += 1
    if not rows:
        print("stats 行がない")
        return 1

    first, last = rows[0], rows[-1]
    span = (datetime.fromisoformat(last["ts"]) - datetime.fromisoformat(first["ts"])).total_seconds()
    print(f"期間: {first['ts']} 〜 {last['ts']}({span / 3600:.2f} 時間、stats {len(rows)} 行、プロセス起動 {events['starts']} 回)")

    rss = [r["rss"] for r in rows]
    # RSS の持続的な増加は、前半と後半の中央値で見る(スワップで一時的に減ることがあるため最大値も併記)。
    half = len(rss) // 2
    med = lambda xs: sorted(xs)[len(xs) // 2]
    print(f"RSS(MB): 最小 {min(rss)} / 最大 {max(rss)} / 前半の中央値 {med(rss[:half] or rss)} / 後半の中央値 {med(rss[half:] or rss)}")
    fds = [r["fds"] for r in rows]
    print(f"fd 数: 最小 {min(fds)} / 最大 {max(fds)}(開始 {first['fds']} → 終了 {last['fds']})")
    worst = [r["worst"] for r in rows]
    print(f"フレーム時間(区間最大, ms): 中央値 {med(worst)} / 最大 {max(worst)} / 100ms 超の区間 {sum(w > 100 for w in worst)}")

    stalls, prev = [], rows[0]
    for r in rows[1:]:
        if r["t"] > prev["t"] and r["ticks"] == prev["ticks"] and r["t"] - prev["t"] < 3600:
            stalls.append((prev["ts"], r["ts"]))
        prev = r
    print(f"更新停止(ticks が増えない区間): {len(stalls)} 件" + ("" if not stalls else f" 例: {stalls[0]}"))
    offline = sum(1 for r in rows if r["feed"] != "online")
    print(f"GEOFRAME 非接続だった区間: {offline} / {len(rows)}")
    print(f"キー入力: {last['keys']} 件")
    print(f"VT 無効化 {events['seat_disabled']} / 有効化 {events['seat_enabled']} / 表示復元 {events['restored']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
