"""限速文件服务器：安卓后台下载 / 前台服务的对照实验用。

- 支持 Range（引擎是多线程分片，不支持 Range 会退化成单连接，测不出真实行为）
- 每个连接限速，总时长可控，方便在锁屏期间采样进度

配套 probe.py：
    python slow_server.py 8800 400 512   # 400MB，每连接 512KB/s
    adb reverse tcp:8800 tcp:8800        # 设备侧 localhost:8800 → 本机
    python probe.py base3 12 60          # 每 60s 采一次，共 12 次

测「真实网络切换」时不能用 adb reverse（它走 USB，关 Wi-Fi 也照样通），
要让设备走局域网访问本机：
    python slow_server.py 8800 400 256 0.0.0.0   # 第 4 个参数是监听地址
"""
import sys
import time
import re
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 8800
TOTAL = int(sys.argv[2]) * 1024 * 1024 if len(sys.argv) > 2 else 400 * 1024 * 1024
PER_CONN = (int(sys.argv[3]) if len(sys.argv) > 3 else 512) * 1024  # 字节/秒

# 默认只听回环；传 0.0.0.0 才会暴露到局域网（仅用于跨网络实验，注意本机防火墙）
HOST = sys.argv[4] if len(sys.argv) > 4 else "127.0.0.1"
CHUNK = 64 * 1024

# 内容无所谓，用可预测的字节，便于以后核对哈希
BLOB = bytes(range(256)) * (CHUNK // 256)


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        sys.stderr.write("[srv] " + fmt % args + "\n")

    def _range(self):
        rng = self.headers.get("Range")
        if not rng:
            return None
        m = re.match(r"bytes=(\d+)-(\d*)", rng)
        if not m:
            return None
        start = int(m.group(1))
        end = int(m.group(2)) if m.group(2) else TOTAL - 1
        end = min(end, TOTAL - 1)
        if start > end:
            return "bad"
        return (start, end)

    def do_HEAD(self):
        self.send_response(200)
        self.send_header("Content-Length", str(TOTAL))
        self.send_header("Accept-Ranges", "bytes")
        self.send_header("Content-Type", "application/octet-stream")
        self.end_headers()

    def do_GET(self):
        r = self._range()
        if r == "bad":
            self.send_response(416)
            self.send_header("Content-Range", f"bytes */{TOTAL}")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        start, end = r if r else (0, TOTAL - 1)
        length = end - start + 1
        self.send_response(206 if r else 200)
        if r:
            self.send_header("Content-Range", f"bytes {start}-{end}/{TOTAL}")
        self.send_header("Content-Length", str(length))
        self.send_header("Accept-Ranges", "bytes")
        self.send_header("Content-Type", "application/octet-stream")
        self.end_headers()
        sent = 0
        t0 = time.time()
        try:
            while sent < length:
                n = min(CHUNK, length - sent)
                self.wfile.write(BLOB[:n])
                sent += n
                # 按每连接限速睡眠
                target = sent / PER_CONN
                drift = target - (time.time() - t0)
                if drift > 0:
                    time.sleep(drift)
        except (BrokenPipeError, ConnectionResetError):
            self.log_message("client gone after %d bytes", sent)


if __name__ == "__main__":
    print(f"serving {TOTAL/1024/1024:.0f} MB on :{PORT}, {PER_CONN/1024:.0f} KB/s per connection", flush=True)
    ThreadingHTTPServer((HOST, PORT), H).serve_forever()
