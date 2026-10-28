"""把本地 HTTP 代理当成 SSH 的通道。

这台机器出站 22 口被封，但 7890 的代理是通的，所以用 `ProxyCommand` 绕过去：

    ssh -o ProxyCommand="python scripts/proxycmd.py %h %p" root@主机
"""

import os
import socket
import sys

PROXY = ("127.0.0.1", 7890)


def main() -> int:
    if len(sys.argv) < 3:
        print("用法: proxycmd.py <host> <port>", file=sys.stderr)
        return 2
    host, port = sys.argv[1], sys.argv[2]

    sock = socket.create_connection(PROXY, timeout=10)
    request = f"CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n\r\n"
    sock.sendall(request.encode())

    # 读到空行为止；不 200 就直说，别让 ssh 拿着半截连接去猜
    head = b""
    while b"\r\n\r\n" not in head:
        chunk = sock.recv(4096)
        if not chunk:
            print("代理没有回应", file=sys.stderr)
            return 1
        head += chunk
    status = head.split(b"\r\n", 1)[0].decode(errors="replace")
    if " 200" not in status:
        print(f"代理拒绝：{status}", file=sys.stderr)
        return 1

    # 代理可能在同一包里多回了一点数据，先把已经读到的部分吐出来
    leftover = head.split(b"\r\n\r\n", 1)[1]
    out = sys.stdout.buffer
    if leftover:
        out.write(leftover)
        out.flush()

    # 双向泵：stdio <-> socket
    import threading

    def pump_in() -> None:
        # 用 os.read/os.write 而不是 sys.stdin.buffer：
        # Python 关闭时会去抢缓冲区锁，和守护线程撞上就是那个
        # "_enter_buffered_busy" 崩溃，ssh 会看到连接无端断开
        try:
            while True:
                data = os.read(0, 4096)
                if not data:
                    break
                sock.sendall(data)
        finally:
            try:
                sock.shutdown(socket.SHUT_WR)
            except OSError:
                pass

    threading.Thread(target=pump_in, daemon=True).start()
    try:
        while True:
            data = sock.recv(4096)
            if not data:
                break
            os.write(1, data)
    except OSError:
        pass
    # 直接走，别触发解释器清理（守护线程还挂在 fd 0 上）
    os._exit(0)


if __name__ == "__main__":
    raise SystemExit(main())
