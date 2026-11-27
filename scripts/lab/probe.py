"""Doze 期间采样：不依赖应用内 API（Doze 会掐掉连本地端口的请求），
直接读磁盘上的任务 json + 进程状态。

用法: python probe.py <标签> <采样次数> [间隔秒]
输出: 标准输出逐行 JSON + 同目录 <标签>.json
adb 路径: 环境变量 FELLDL_ADB，否则用 PATH 里的 adb
"""
import json
import os
import subprocess
import sys
import time

LABEL = sys.argv[1] if len(sys.argv) > 1 else "run"
ROUNDS = int(sys.argv[2]) if len(sys.argv) > 2 else 10
GAP = int(sys.argv[3]) if len(sys.argv) > 3 else 60
ADB = os.environ.get("FELLDL_ADB") or "adb"
PKG = "com.felldl.app"


def sh(cmd, timeout=20):
    try:
        return subprocess.check_output([ADB, "shell", cmd], timeout=timeout, stderr=subprocess.DEVNULL).decode(
            "utf-8", "ignore"
        )
    except Exception as e:
        return f"<err {e}>"


def task():
    """取最新的一个任务文件（按修改时间）"""
    listing = sh(f"run-as {PKG} ls -t /data/data/{PKG}/tasks")
    ids = [x.strip() for x in listing.splitlines() if x.strip().endswith(".json")]
    if not ids:
        return {}
    raw = sh(f"run-as {PKG} cat /data/data/{PKG}/tasks/{ids[0]}")
    try:
        return json.loads(raw)
    except Exception:
        return {"raw": raw[:200]}


def proc_state():
    out = sh(f"dumpsys activity processes | grep -A 40 'ProcessRecord.*{PKG}'")
    frozen = "isFrozen=?" if "isFrozen=" not in out else out.split("isFrozen=")[1].split()[0]
    adj = ""
    for line in out.splitlines():
        if "oom:" in line or "procState" in line:
            adj = line.strip()[:60]
            break
    return frozen, adj


def idle():
    out = sh("dumpsys deviceidle")
    state = light = charging = "?"
    for line in out.splitlines():
        for part in line.split():
            if part.startswith("mState="):
                state = part.split("=")[1]
            elif part.startswith("mLightState="):
                light = part.split("=")[1]
            elif part.startswith("mCharging="):
                charging = part.split("=")[1]
    return f"{state}/{light} charging={charging}"


rows = []
for i in range(ROUNDS):
    t = task()
    frozen, adj = proc_state()
    row = {
        "t": i,
        "clock": time.strftime("%H:%M:%S"),
        "state": t.get("state"),
        "downloaded": t.get("downloaded"),
        "error": (t.get("error") or "")[:60],
        "idle": idle(),
        "frozen": frozen,
    }
    rows.append(row)
    print(json.dumps(row, ensure_ascii=False), flush=True)
    if i != ROUNDS - 1:
        time.sleep(GAP)

with open(f"{LABEL}.json", "w", encoding="utf-8") as f:
    json.dump(rows, f, ensure_ascii=False, indent=1)
print("saved", LABEL)
