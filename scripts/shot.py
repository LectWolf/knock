"""按窗口句柄截图：把 Knock 窗口提到最前，再用 PIL 抓那一块。

用 Python 而不是 PowerShell+C#，因为这段 P/Invoke 在 PS 里编译太挑食。
"""

import ctypes
import ctypes.wintypes as wt
import subprocess
import sys
import time

from PIL import ImageGrab

user32 = ctypes.windll.user32
user32.SetProcessDPIAware()


def pids_of(name: str) -> list[int]:
    out = subprocess.run(
        ["tasklist", "/FI", f"IMAGENAME eq {name}", "/FO", "CSV", "/NH"],
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


def windows_of(pid: int) -> list[tuple[int, tuple[int, int, int, int]]]:
    found: list[tuple[int, tuple[int, int, int, int]]] = []

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


def main() -> int:
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
        user32.ShowWindow(hwnd, 9)  # SW_RESTORE
        # 置顶：SetForegroundWindow 会被系统拒绝，置顶最稳
        HWND_TOPMOST, HWND_NOTOPMOST = -1, -2
        SWP = 0x0001 | 0x0002 | 0x0010  # NOSIZE | NOMOVE | NOACTIVATE
        user32.SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP)
        user32.SetForegroundWindow(hwnd)
        time.sleep(1.2)
        # 提权之后窗口可能移位，重新取一次
        rect = wt.RECT()
        user32.GetWindowRect(hwnd, ctypes.byref(rect))
        box = (rect.left, rect.top, rect.right, rect.bottom)
        img = ImageGrab.grab(bbox=box, all_screens=True)
        img.save(out_path)
        user32.SetWindowPos(hwnd, HWND_NOTOPMOST, 0, 0, 0, 0, SWP)
        print(f"抓到 {box[2]-box[0]}x{box[3]-box[1]} → {out_path}")
        return 0

    print("没找到可见窗口")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
