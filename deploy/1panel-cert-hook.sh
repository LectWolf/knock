#!/bin/bash
# 贴进 1Panel「申请证书之后执行脚本」。
#
# 前提：面板和节点是同一台机器（香港·荃湾那种），证书由这台机的 1Panel 签发。
# 1Panel 执行脚本时工作目录 = 证书推送目录，证书文件名是 fullchain.pem / privkey.pem，
# 所以用相对路径 ./ 就能取到；取不到再回退去站点目录里按域名找。
#
# 干什么：把证书放到 /etc/ssl/knock/，然后让中继切到 TLS（Reloading，换文件自动生效）。
set -uo pipefail

DOMAIN="node-tw.mcii.cc"     # 证书要覆盖的名字（改节点就改这里）
DST=/etc/ssl/knock

log() { echo "[$(date '+%F %T')] $*"; }
die() { log "失败：$*" >&2; exit 1; }

CERT="./fullchain.pem"
PKEY="./privkey.pem"
if [ ! -f "$CERT" ]; then
  for d in /opt/1panel/www/sites/*/ssl /opt/1panel/tmp/ssl/*; do
    if [ -f "$d/fullchain.pem" ] && \
       openssl x509 -in "$d/fullchain.pem" -noout -checkhost "$DOMAIN" 2>/dev/null | grep -q "does match"; then
      CERT="$d/fullchain.pem"; PKEY="$d/privkey.pem"; break
    fi
  done
fi
[ -f "$CERT" ] && [ -f "$PKEY" ] || die "找不到证书（当前目录 $(pwd)）"

openssl x509 -in "$CERT" -noout -checkhost "$DOMAIN" 2>/dev/null | grep -q "does match" \
  || die "这张证书不覆盖 $DOMAIN"
log "证书 $CERT"
log "到期 $(openssl x509 -in "$CERT" -noout -enddate | cut -d= -f2)"

install -d -m755 "$DST"
install -m644 "$CERT" "$DST/fullchain.pem"
install -m600 "$PKEY" "$DST/privkey.pem"
log "已放到 $DST（fullchain.pem / privkey.pem）"

command -v knock-apply-cert >/dev/null || die "这台机没装 knock-apply-cert"
knock-apply-cert "$DOMAIN" || die "应用证书失败"

log "完成"
