# 欠账清单

> 2026-10-28 全量走查。只记"明明该有、现在没有"的东西，不记还没开始做的功能想法。

## 会阻碍「点开就有窗口」的

1. ~~没有 Tauri 壳~~ **已补**：`main.rs` + `tauri.conf.json` + `build.rs` + 图标 + 托盘。
   窗口能起来，关窗口缩托盘实测有效（`IsWindowVisible` 为 False，进程仍在）。
2. ~~界面没接 Rust~~ **已接**：`load_settings / save_settings / open_door / knock / close_door / door_state / guests / kick` 八个命令，
   状态走 `knock://state` 事件、名册每秒拉一次。**但只在浏览器外验证了"读设置"这一段**，开门/敲门全靠网络，没端到端跑通。
3. **名册是轮询不是推送**：`guests` 每秒拉一次。人多了没问题，但应该改成 transport 在 join/kick 时主动 emit。

4. ~~没有托盘~~ **已补**：左键唤回窗口，右键菜单（打开主界面 / 关门 / 退出）。
   仍未接的是"关门"菜单项：现在只发 `knock://tray` 事件，界面还没监听。
5. **关门只是界面动作**：`关门` 会回到 idle，但还没真的调节点 DELETE、也没断服里的人。

5. **没出过安装包**：`npm run tauri build`（release + NSIS/MSI）没跑过，只验证了 debug 壳。
6. ~~壳里没有单实例~~ **已补**：`tauri-plugin-single-instance`，二次启动只把窗口叫出来。

## 协议与节点

6. **心跳没实现**：节点侧已按"最后一次 announce 续命"，但客户端还没有每 60 秒重发 announce 的循环。
7. **没有兼容期**：`knock/1` 直接被拒。升级后旧节点/旧房客只会看到"版本对不上"，没有过渡提示。
8. **节点 API 是明文 HTTP**：公网部署要 TLS；relay 的 TLS（ACME）要一个指向节点的域名。
9. **节点列表只到文件**：`nodes/public.json` 有了，但客户端没有拉取、缓存、过期标记，也没有校验 CI。
10. **控制帧没有金标向量**：`spec/invite-v1.tsv` 那套跨语言向量只覆盖邀请码，控制帧只有 Rust 单测。

## 工程

11. **没有 CI**：Go / Rust / 前端三套测试全手跑。
12. **e2e 脚本写死**：`scripts/transport.sh` 里固定了香港节点 IP 和门牌号，换节点要改脚本。
13. **没有发布流程**：Tauri bundle、节点二进制交叉编译、版本号统一，都还没有。
14. **没有远端仓库**：本轮才 `git init`，远端（GitHub 或自建）要定。

## 小的

15. **GLOSSARY 缺词**：昵称、名册、控制帧、闸门都没收录（AGENTS 要求改术语先进词表）。
16. **`docs/NODE.md` 状态表没写 relay 服务**：实际已经跑起来了。
17. **UI 的"服务号"没接线**：输入框还在界面上，没传给 `Target::Open { port }`。
