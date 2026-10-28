"""按窗口句柄给 Knock 拍照。

屏幕截图会被别的窗口挡住（浏览器、聊天窗），所以主路径是 `PrintWindow`：
让窗口自己把内容画到一块内存位图上，挡不挡得着都拍得到。
拍出来是黑的再退回屏幕抓取。
"""

import ctypes
import ctypes.wintypes as wt
import subprocess
import sys
import time

from PIL import Image, ImageGrab

user32 = ctypes.windll.user32
gdi32 = ctypes.windll.gdi32
user32.SetProcessDPIAware()


class BITMAPINFOHEADER(ctypes.Structure):
    _fields_ = [
        ("biSize", wt.DWORD),
        ("biWidth", wt.LONG),
        ("biHeight", wt.LONG),
        ("biPlanes", wt.WORD),
        ("biBitCount", wt.WORD),
        ("biCompression", wt.DWORD),
        ("biSizeImage", wt.DWORD),
        ("biXPelsPerMeter", wt.LONG),
        ("biYPelsPerMeter", wt.LONG),
        ("biClrUsed", wt.DWORD),
        ("biClrImportant", wt.DWORD),
    ]


class BITMAPINFO(ctypes.Structure):
    _fields_ = [("bmiHeader", BITMAPINFOHEADER), ("bmiColors", wt.DWORD * 3)]


def pids_of(name):
    out = subprocess.run(
        ["tasklist", "/FI", "IMAGENAME eq " + name, "/FO", "CSV", "/NH"],
        capture_output=True, text=True, encoding="gbk", errors="ignore",
    ).stdout
    pids = []
    for line in out.splitlines():
        parts = [p.strip('"') for p in line.split('","')]
        if len(parts) >= 2 and parts[0].lower() == name.lower():
            try:
                pids.append(int(parts[1]))
            except ValueError:
                pass
    return pids


def windows_of(pid):
    found = []

    @ctypes.WINFUNCTYPE(wt.BOOL, wt.HWND, wt.LPARAM)
    def cb(hwnd, _):
        if not user32.IsWindowVisible(hwnd):
            return True
        wpid = wt.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(wpid))
        if wpid.value != pid:
            return True
        rect = wt.RECT()
        user32.GetWindowRect(hwnd, ctypes.byref(rect))
        w, h = rect.right - rect.left, rect.bottom - rect.top
        if w > 200 and h > 200:
            found.append((hwnd, (rect.left, rect.top, rect.right, rect.bottom)))
        return True

    user32.EnumWindows(cb, 0)
    return found


def print_window(hwnd, w, h):
    hdc = user32.GetWindowDC(hwnd)
    if not hdc:
        return None
    mem = gdi32.CreateCompatibleDC(hdc)
    bmp = gdi32.CreateCompatibleBitmap(hdc, w, h)
    gdi32.SelectObject(mem, bmp)
    ok = user32.PrintWindow(hwnd, mem, 2)

    info = BITMAPINFO()
    info.bmiHeader.biSize = ctypes.sizeof(BITMAPINFOHEADER)
    info.bmiHeader.biWidth = w
    info.bmiHeader.biHeight = -h
    info.bmiHeader.biPlanes = 1
    info.bmiHeader.biBitCount = 32
    info.bmiHeader.biCompression = 0

    buf = ctypes.create_string_buffer(w * h * 4)
    got = gdi32.GetDIBits(mem, bmp, 0, h, buf, ctypes.byref(info), 0)

    gdi32.DeleteObject(bmp)
    gdi32.DeleteDC(mem)
    user32.ReleaseDC(hwnd, hdc)

    if not ok or not got:
        return None
    return Image.frombuffer("RGBA", (w, h), buf, "raw", "BGRA", 0, 1).convert("RGB")


def main():
    out_path = sys.argv[1] if len(sys.argv) > 1 else "shots/window.png"
    pids = pids_of("knock.exe")
    if not pids:
        print("knock.exe 没在跑")
        return 1

    for pid in pids:
        wins = windows_of(pid)
        if not wins:
            continue
        hwnd, box = wins[0]
        w, h = box[2] - box[0], box[3] - box[1]

        img = print_window(hwnd, w, h)
        if img is None or img.getextrema() == ((0, 0), (0, 0), (0, 0)):
            user32.SetWindowPos(hwnd, -1, 0, 0, 0, 0, 0x0001 | 0x0002 | 0x0010)
            time.sleep(0.9)
            rect = wt.RECT()
            user32.GetWindowRect(hwnd, ctypes.byref(rect))
            img = ImageGrab.grab(
                bbox=(rect.left, rect.top, rect.right, rect.bottom), all_screens=True
            )
            user32.SetWindowPos(hwnd, -2, 0, 0, 0, 0, 0x0001 | 0x0002 | 0x0010)

        img.save(out_path)
        print(str(w) + "x" + str(h) + " -> " + out_path)
        return 0

    print("没找到可见窗口")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
