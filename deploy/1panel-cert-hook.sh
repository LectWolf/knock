#!/bin/bash
# 贴进 1Panel「申请证书之后执行脚本」里的内容。
#
# 1Panel 会在「证书推送目录」执行它，当前目录就是那里，证书文件名是
# fullchain.pem / privkey.pem（相对路径即可）。
# 作用：把证书推到节点，并让节点切到 TLS 中继（自动重载，不用重启）。
set -uo pipefail

DOMAIN="node-tw.mcii.cc"        # 证书要覆盖的名字
NODE_HOST="154.36.165.162"      # 节点公网地址
NODE_PORT=19508                 # 节点 SSH 端口
SSH_KEY=/root/.ssh/knock-node-tw
REMOTE_DIR=/etc/ssl/knock

log() { echo "[$(date '+%F %T')] $*"; }
die() { log "失败：$*" >&2; exit 1; }

# 证书文件：优先当前目录（1Panel 推送目录），否则找站点目录
CERT="./fullchain.pem"
PKEY="./privkey.pem"
if [ ! -f "$CERT" ]; then
  for d in /opt/1panel/www/sites/*/ssl /opt/1panel/tmp/ssl/*; do
    if [ -f "$d/fullchain.pem" ] && openssl x509 -in "$d/fullchain.pem" -noout -checkhost "$DOMAIN" 2>/dev/null | grep -q "does match"; then
      CERT="$d/fullchain.pem"; PKEY="$d/privkey.pem"; break
    fi
  done
fi
[ -f "$CERT" ] || die "找不到证书（当前目录 $(pwd)）"
[ -f "$PKEY" ] || die "找不到私钥（当前目录 $(pwd)）"
log "证书：$CERT"
log "到期：$(openssl x509 -in "$CERT" -noout -enddate | cut -d= -f2)"

openssl x509 -in "$CERT" -noout -checkhost "$DOMAIN" 2>/dev/null | grep -q "does match" \
  || die "这张证书不覆盖 $DOMAIN"

SSH_OPTS=(-i "$SSH_KEY" -o StrictHostKeyChecking=no -o BatchMode=yes -o ConnectTimeout=15)

log "推送到 $NODE_HOST:$REMOTE_DIR"
ssh -p "$NODE_PORT" "${SSH_OPTS[@]}" "root@$NODE_HOST" "mkdir -p $REMOTE_DIR" || die "连不上节点"
scp -P "$NODE_PORT" "${SSH_OPTS[@]}" -q "$CERT" "root@$NODE_HOST:$REMOTE_DIR/fullchain.pem" || die "证书推送失败"
scp -P "$NODE_PORT" "${SSH_OPTS[@]}" -q "$PKEY" "root@$NODE_HOST:$REMOTE_DIR/privkey.pem" || die "私钥推送失败"

log "让节点切到 TLS"
ssh -p "$NODE_PORT" "${SSH_OPTS[@]}" "root@$NODE_HOST" "knock-apply-cert $DOMAIN" || die "节点应用证书失败"

log "完成"
