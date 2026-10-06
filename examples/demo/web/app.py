# 依存なしで redis の INCR を呼ぶ小さな HTTP サーバー
import os
import socket
from http.server import BaseHTTPRequestHandler, HTTPServer

REDIS = (os.environ.get("REDIS_HOST", "localhost"), 6379)


def incr(key: str) -> int:
    with socket.create_connection(REDIS, timeout=3) as s:
        s.sendall(f"*2\r\n$4\r\nINCR\r\n${len(key)}\r\n{key}\r\n".encode())
        return int(s.recv(64).decode().strip().lstrip(":"))


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        body = f"{os.environ.get('GREETING', '')}: {incr('hits')}\n".encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.end_headers()
        self.wfile.write(body)


print("listening on :8000", flush=True)
HTTPServer(("0.0.0.0", 8000), Handler).serve_forever()
