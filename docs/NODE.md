# 跑一台节点

节点是「别人家的那台机器」：它替房间换地址（信令），可选地替人转发流量（中继）。
**它不是我们的服务**，也不是 VPN——只做「两个拿着门牌号的人之间的一条通道」。

## 它现在能干什么、还不能干什么

| 能力 | 状态 |
|---|---|
| 房间登记（`POST /rooms` 发 6 位码、查码、关门、过期清理） | ✅ 能 |
| 节点身份 + 节点码（粘贴即可添加这个节点） | ✅ 能 |
| 启动横幅、可达性警告、`-print-code`、`-inspect` | ✅ 能 |
| 房间上限、查码限速（防枚举门牌号） | ✅ 能 |
| 房主发布身份与候选地址（`POST /rooms/{code}/announce`，一次性令牌） | ✅ 能 |
| 数据中转（`iroh-relay`） | 🚧 下一步 |
| TLS / 证书 | 🚧 下一步，目前是明文 HTTP（只适合本机或内网先验） |

**所以现在别把节点开到公网上**：接口是明文的，房间上限和限速也还没做。

## 装

```sh
curl -fsSL https://raw.githubusercontent.com/OWNER/knock/main/deploy/install.sh \
  | sudo sh -s -- --domain relay.example.com
```

或者用本地编译的二进制（还没发 release 时就这样）：

```sh
cd server && go build -o bin/knock-hub ./cmd/hub
sudo sh deploy/install.sh --binary ./bin/knock-hub --ip 203.0.113.7
```

装完最后会打印节点码。再要一次：

```sh
sudo /usr/local/bin/knock-hub -data-dir /var/lib/knock -print-code
```

| 参数 | 作用 |
|---|---|
| `--domain` | 对外域名。**推荐**：IP 变了不用重发节点码 |
| `--ip` | 对外 IPv4/IPv6。没有域名时用 |
| `--port` | 对外端口，默认 `8443` |
| `--no-relay` | 只做信令，不替别人转发流量 |
| `--binary` | 用本地二进制，不联网下载 |
| `--data-dir` | 数据目录，默认 `/var/lib/knock` |

## 配置（`/var/lib/knock/node.json`）

```json
{
  "label": "",
  "public_port": 8443,
  "domain": "relay.example.com",
  "ipv4": "",
  "ipv6": "",
  "relay_allowed": true,
  "listen": ":8443",
  "room_ttl": "10m",
  "max_rooms": 200
}
```

| 字段 | 说明 |
|---|---|
| `label` | 短邀请里的 4 个字符。留空则由密钥派生，保证稳定 |
| `public_port` | **别人连你用的端口**，会写进节点码 |
| `domain` / `ipv4` / `ipv6` | 至少填一个。域名优先 |
| `relay_allowed` | 是否允许数据中转。关掉仍可当信令点 |
| `relay_url` | 这个节点的中继地址（`https://…`），留空 = 只做信令。公网必须 https |
| `listen` | 本机监听地址 |
| `room_ttl` | 房间闲置寿命（房主每次心跳都会续期，不是开门后的固定时长），Go duration 写法（`10m`、`90s`） |
| `max_rooms` | 同时开着的房间上限，`0` 为不限 |

改完配置要 `systemctl restart knock-node`。

## 节点码

```
34T15JR0E43VP08024H36H2NCSVPG
```

- 这是**自描述**的：地址、端口、标签、指纹都在里面，不需要任何列表。
- 里面**没有房间号**。房间号是别人在你这里开门时临时拿到的 6 位码。
- 别人粘这一串 = 添加你这个节点；要开门时把「节点码 + `.` + 6 位门牌号」一起发。
- 如果对方已经加过你，只需要 `04HM.门牌号`（`04HM` 是你的标签）。

**别把它当秘密，也别公开乱贴**：它含你的 IP 和端口。安全不靠它保密——身份靠里面的指纹钉扎，
拿到码的人也没法冒充你的节点。

## 密钥

`/var/lib/knock/node.key`（`0600`，PKCS8 PEM）。它是节点的长期身份，`pin` 就是它的指纹。

- **丢了它，之前发出去的节点码全部作废**——对方会发现指纹对不上。
- 想换身份就删掉它再启动，旧码自然失效。
- 备份它是唯一需要备份的东西（`node.json` 不算）。

## 已部署的节点

| 节点 | 地址 | 节点码 | 备注 |
|---|---|---|---|
| 香港·荃湾 | `http://154.36.165.162:30990`（SSH 19508） | `377PN6H4MPH0WYEFD838Q42C674TR` | 项目测试节点，中继同在机上（`127.0.0.1:30991`，不对外） |
| 枣庄·14900K | `http://202.189.15.149:19600`（SSH 19508） | `36YS5JNX1YAS0K5XJAJ6MBSEJ12WW` | 国内节点，`/data/knock`，只做信令（未配中继） |

升级二进制的方式（就地替换，留 `.old` 可回滚）：

```sh
GOOS=linux GOARCH=amd64 CGO_ENABLED=0 go build -trimpath -ldflags "-s -w" -o knock-hub ./cmd/hub
scp -P 19508 knock-hub root@<主机>:/usr/local/bin/knock-hub.new
ssh -p 19508 root@<主机> 'chmod +x /usr/local/bin/knock-hub.new   && cp /usr/local/bin/knock-hub /usr/local/bin/knock-hub.old   && mv /usr/local/bin/knock-hub.new /usr/local/bin/knock-hub   && systemctl restart knock-node && curl -s localhost:30990/healthz'
```

`healthz` 里必须能看到 `"proto":{"api":1,"transport":2}`，客户端靠它拒绝旧节点。

## 排查

| 现象 | 先看什么 |
|---|---|
| `节点码还没生成成功` | 配置里没填地址。`journalctl -u knock-node -n 40` |
| 启动横幅里有 ⚠ | 填的地址不是公网可达（私网、环回、CGNAT）。别人连不上，不是软件的锅 |
| 别人说「门牌号不对」 | 对方用的节点码是不是你这台的？房间是不是已经过期？ |
| 装了但没起来 | `systemctl status knock-node`；端口被占会直接报错退出 |

## 它不做的事（刻意）

- **不主动外连任何目标**。只处理「两端都持有有效凭据」的转发请求，所以它不是开放代理。
- 不接 TUN、不做网段、不代理任意 UDP。
- 不收集账号，不记聊天内容，也没有计费。

## 中继对外开放（2026-10-30 上线）

节点对外通告的中继地址写在 `/var/lib/knock/node.json`：

```json
"relay_url": "http://154.36.165.162:30991",
"allow_plain_relay": true
```

- `iroh-relay` 必须监听 `0.0.0.0:30991`（`/etc/knock-relay.toml` 的 `http_bind_addr`），
  且云厂商安全组要放行 **TCP 30991**——只开 30990 的话，客户端拿到的中继地址是不可达的。
- 明文 http 中继要 `allow_plain_relay: true` 才会被接受；**正式部署请上 TLS**（443 + 证书），
  iroh 流量本身端到端加密，中继只看到密文，但明文端口不合规也容易被中间设备干扰。
- 验证：从外网 `curl http://<节点IP>:30991/` 应返回 200；端到端测试
  `KNOCK_RELAY_URL=http://<节点IP>:30991 cargo test --test transport_e2e a_failed_punch -- --ignored` 应绿。

## 单端口 + 安装时选中继（2026-10-30）

节点现在**只对外开一个端口**：房间登记和中继共用它，中继进程只监听 `127.0.0.1:30991`，
`knock-hub` 按路径把 `/rooms`、`/healthz` 之外的请求转给它（`relay_local` 字段）。

安装：`sudo ./install.sh`，四个问题（名字 / 对外地址 / 端口 / 要不要中继），装完自启动。
管理：`knock`（菜单）、`knock status`、`knock code`、`knock relay on|off`、`knock uninstall`。
演示：`docs/install-demo.html`（模拟安装过程）。
