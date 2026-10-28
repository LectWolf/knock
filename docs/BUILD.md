# 本地跑起来

## 客户端（Windows 优先）

```bash
cd client
npm install
npm run dev            # 前端热更新，浏览器里看界面：http://localhost:5173
npm run tauri dev      # 带壳跑（要 vite 在 5173，Tauri 会自己起）
npx tauri build --no-bundle        # 只出可执行文件（快，不联网）：src-tauri/target/release/knock.exe
npm run tauri build               # 出安装包（NSIS/MSI，首次要联网拉工具链）

> 只用 `cargo build --release` 是不够的：Tauri 靠 `custom-protocol` feature 决定
> "内嵌前端"还是"连 dev 服务器"，`tauri build` 会帮你带这个 feature，裸 cargo 不会。
```

**国内构建先配镜像**，否则拉 crates.io 会慢到以为卡死（实测 5 分钟 2 个包）：

```bash
cat > ../.cargo/config.toml <<'CFG'
[source.crates-io]
replace-with = "rsproxy-sparse"
[source.rsproxy-sparse]
registry = "sparse+https://rsproxy.cn/index/"
[net]
retry = 5
CFG
```

换完之后同样的下载是 2 分钟 397 个包。`.cargo/` 已在 `.gitignore` 里，只影响本机。

## 节点（Linux）

见 `docs/NODE.md`。

## 托盘行为

- 关窗口 = 缩到托盘，门继续开着（`CloseRequested` 里 `prevent_close` + `hide`）
- 左键点托盘图标 = 把窗口叫回来；右键 = 打开主界面 / 关门 / 退出
- 真要退出只能走托盘菜单，避免手滑把人从服里踢下去
