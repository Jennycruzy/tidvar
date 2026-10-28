#!/bin/sh
set -eu
PATH=/usr/sbin:/usr/bin:/sbin:/bin
# One VPN per network: the testnet VPN uses the defaults; the mainnet VPN sets these in its unit.
dev=${TIDVAR_VPN_INTERFACE:-tidvar}
net=${TIDVAR_VPN_NET:-77}
port=${TIDVAR_VPN_PORT:-51820}
ip address replace 10.$net.0.1/16 dev "$dev"
ip link set dev "$dev" mtu 1420 up
sysctl -q -w net.ipv4.ip_forward=1
tc qdisc replace dev "$dev" root handle 1: htb default ffff
tc class replace dev "$dev" parent 1: classid 1:1 htb rate 1000mbit ceil 1000mbit quantum 1514
tc class replace dev "$dev" parent 1:1 classid 1:ffff htb rate 20mbit ceil 20mbit burst 128k cburst 128k quantum 1514
tc qdisc replace dev "$dev" handle ffff: ingress
if nft list table inet "${dev}_vpn" >/dev/null 2>&1; then nft delete table inet "${dev}_vpn"; fi
if nft list table ip "${dev}_nat" >/dev/null 2>&1; then nft delete table ip "${dev}_nat"; fi
sed -e "s/tidvar_vpn/${dev}_vpn/; s/tidvar_nat/${dev}_nat/; s/define vpn = \"tidvar\"/define vpn = \"$dev\"/; s/10\.77\./10.$net./g" \
  /root/tidvar/deploy/vpn.nft | nft -f -
ufw allow "$port"/udp comment tidvar-vpn >/dev/null
ufw allow in on "$dev" to 10.$net.0.1 port 53 comment tidvar-dns >/dev/null
ufw route allow in on "$dev" out on eth0 comment tidvar-vpn >/dev/null
