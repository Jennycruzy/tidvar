#!/bin/sh
set -eu
PATH=/usr/sbin:/usr/bin:/sbin:/bin
ip address replace 10.77.0.1/16 dev tidvar
ip link set dev tidvar mtu 1420 up
sysctl -q -w net.ipv4.ip_forward=1
tc qdisc replace dev tidvar root handle 1: htb default ffff
tc class replace dev tidvar parent 1: classid 1:1 htb rate 1000mbit ceil 1000mbit quantum 1514
tc class replace dev tidvar parent 1:1 classid 1:ffff htb rate 20mbit ceil 20mbit burst 128k cburst 128k quantum 1514
tc qdisc replace dev tidvar handle ffff: ingress
if nft list table inet tidvar_vpn >/dev/null 2>&1; then nft delete table inet tidvar_vpn; fi
if nft list table ip tidvar_nat >/dev/null 2>&1; then nft delete table ip tidvar_nat; fi
nft -f /root/tidvar/deploy/vpn.nft
ufw allow 51820/udp comment tidvar-vpn >/dev/null
ufw allow in on tidvar to 10.77.0.1 port 53 comment tidvar-dns >/dev/null
ufw route allow in on tidvar out on eth0 comment tidvar-vpn >/dev/null
