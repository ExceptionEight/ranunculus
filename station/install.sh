#!/usr/bin/env bash
# Puts Ranunculus on a rooted Yandex Station 2, or takes it off again. Run it on the Linux machine the server runs
# on (the Compose project of this repository), with SSH access to the station as root.
#
#   station/install.sh [options] STATION              STATION: the station's address in the home network
#   station/install.sh --uninstall [options] STATION
#
#   -p PORT      the station's SSH port (2222)
#   -i KEY       an SSH private key (default: whatever ssh uses)
#   -s ADDRESS   this server's address as the station reaches it (default: this machine's address on the way to it)
#   -d NAME      the station's name on the server (default: the one it has; a new station: station-<end of its MAC>)
#   -y           reboot the station without asking
#
# It checks the station, copies the server's client for stations and its settings to /data/ranunculus and runs
# `ranunculus-station install` there (station/src/setup.rs: Alice switched off, autostart), then reboots the
# station. Nothing in the firmware is changed but one autostart file; --uninstall puts the station back.
#
# COMPOSE names the server's Compose project if it is not the one here (COMPOSE="docker compose -p name -f file");
# SSH_OPTS adds options to ssh.
set -euo pipefail

die() {
    echo "install.sh: $*" >&2
    exit 1
}

port=2222 key='' address='' name='' yes='' uninstall=''
while [ $# -gt 0 ]; do
    case "$1" in
        -p) port=${2:?-p PORT}; shift 2 ;;
        -i) key=${2:?-i KEY}; shift 2 ;;
        -s) address=${2:?-s ADDRESS}; shift 2 ;;
        -d) name=${2:?-d NAME}; shift 2 ;;
        -y) yes=1; shift ;;
        --uninstall) uninstall=1; shift ;;
        -h | --help) sed -n '2,19p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        -*) die "unknown option $1 (see --help)" ;;
        *) break ;;
    esac
done
[ $# -eq 1 ] || die "one station's address is needed (see --help)"
station=$1
home=/data/ranunculus

read -r -a ssh_opts <<<"${SSH_OPTS:-}"
on_station() {
    ssh -p "$port" ${key:+-i "$key"} -o BatchMode=yes -o ConnectTimeout=8 "${ssh_opts[@]}" "root@$station" "$@"
}

reboot_station() {
    if [ -z "$yes" ]; then
        # no terminal to ask (a script, a pipe): the same as «no»
        read -r -p "Reboot the station now? [y/N] " answer || answer=
        case "$answer" in y | Y | yes) ;; *) echo "Not rebooted: it takes effect after the next reboot."; exit 0 ;; esac
    fi
    echo "Rebooting the station…"
    on_station 'reboot' >/dev/null 2>&1 || true
    sleep 15
    for _ in $(seq 1 40); do
        on_station true >/dev/null 2>&1 && return 0
        sleep 3
    done
    die "the station did not come back in two minutes: look at it"
}

command -v ssh >/dev/null || die "ssh is not installed here"
on_station true 2>/dev/null || die "no SSH access to root@$station (port $port): root the station first and check the address and the key"
model=$(on_station 'tr -d "\0" < /proc/device-tree/model 2>/dev/null || cat /etc/hostname' || true)
case "$model" in
    "Yandex Station Midi" | Yandex-Station-gen2) ;;
    *) die "this is «$model», not a Yandex Station 2: nothing done" ;;
esac

if [ -n "$uninstall" ]; then
    on_station "[ -x $home/ranunculus-station ]" || die "Ranunculus is not on this station"
    on_station "$home/ranunculus-station uninstall"
    reboot_station
    echo "Done: the station is Alice's again."
    exit 0
fi

free=$(on_station "df -k /data | awk 'NR==2 {print \$4}'")
[ "${free:-0}" -ge 20000 ] || die "less than 20 MB free in the station's /data"

# the server: its client for stations and what a station needs to find and know it
cd "$(dirname "$0")/.."
read -r -a compose <<<"${COMPOSE:-docker compose}"
command -v "${compose[0]}" >/dev/null || die "${compose[0]} is not installed here"
"${compose[@]}" exec -T server true 2>/dev/null || die "the server is not running here (${compose[*]} up -d)"
if [ -z "$address" ]; then
    target=$(getent ahostsv4 "$station" | awk '{print $1; exit}')
    address=$(ip -4 route get "${target:-$station}" 2>/dev/null | sed -n 's/.* src \([0-9.]*\).*/\1/p')
    [ -n "$address" ] || die "this machine's address on the way to the station is not known: give it with -s"
fi
settings=$("${compose[@]}" exec -T server ranunculus-server station-config "$address") || die "the server gave no settings for a station"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
"${compose[@]}" exec -T server cat station/ranunculus-station >"$tmp/client" || die "the server has no client for stations"
[ -s "$tmp/client" ] || die "the server's client for stations is empty"
sum=$(sha256sum "$tmp/client" | cut -d' ' -f1)

if [ -z "$name" ]; then
    name=$(on_station "sed -n 's/^device *= *\"\\(.*\\)\"/\\1/p' $home/station.toml 2>/dev/null" || true)
fi
if [ -z "$name" ]; then
    mac=$(on_station 'cat /sys/class/net/wlan0/address 2>/dev/null' | tr -d ':' || true)
    [ -n "$mac" ] || die "the station's MAC address is not readable: give it a name with -d"
    name=station-${mac: -4}
fi
case "$name" in *[!a-zA-Z0-9_-]* | '') die "the station's name «$name»: Latin letters, digits, - and _ only" ;; esac

echo "Station $station: $model. The server for it: $address. Its name: $name."
on_station "mkdir -p $home && cat > $home/ranunculus-station.new && chmod 755 $home/ranunculus-station.new" <"$tmp/client"
[ "$(on_station "sha256sum $home/ranunculus-station.new | cut -d' ' -f1")" = "$sum" ] || die "the client arrived damaged: try again"
on_station "$home/ranunculus-station.new version" >/dev/null || die "the station cannot run the client: is it a Station 2?"
on_station "[ ! -x $home/ranunculus-station ] || $home/ranunculus-station stop; mv -f $home/ranunculus-station.new $home/ranunculus-station"
printf '%s\ndevice = "%s"\n' "$settings" "$name" | on_station "umask 077; cat > $home/station.toml"
on_station "$home/ranunculus-station install"
reboot_station

on_station "$home/ranunculus-station status" || die "not everything is in place on the station (above)"
for _ in $(seq 1 20); do
    if "${compose[@]}" logs --since 3m server 2>/dev/null | grep -q "\[$name\] connected"; then
        echo "Done: the station is connected to the server. It is in the web UI now."
        exit 0
    fi
    sleep 3
done
die "the station is set up but has not connected to the server at $address: can it reach port 52700 there? Its log: ssh … 'cat /tmp/ranunculus/station.log'"
