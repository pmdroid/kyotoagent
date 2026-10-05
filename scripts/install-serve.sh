#!/bin/sh
set -eu

print=0
uninstall=0
listen=
while [ $# -gt 0 ]; do
  case $1 in
    --print)
      print=1
      ;;
    --uninstall)
      uninstall=1
      ;;
    --listen)
      if [ $# -lt 2 ] || [ -z "${2:-}" ]; then
        echo "--listen needs an address" >&2
        exit 2
      fi
      listen=$2
      shift
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
  shift
done

euid=${KYOTOAGENT_INSTALL_EUID:-$(id -u)}
if [ "$euid" -eq 0 ]; then
  echo "kyotoagent serve refuses to run as root" >&2
  exit 1
fi

os=${KYOTOAGENT_INSTALL_OS:-$(uname -s)}
home=${HOME:?}
config_home=${XDG_CONFIG_HOME:-$home/.config}

case $os in
  Linux)
    unit=$config_home/systemd/user/kyotoagent.service
    template_name=kyotoagent.service.in
    ;;
  Darwin)
    unit=$home/Library/LaunchAgents/ai.kyotoagent.serve.plist
    template_name=ai.kyotoagent.serve.plist.in
    ;;
  *)
    echo "install-serve.sh supports Linux and macOS" >&2
    exit 1
    ;;
esac

if [ -n "${DESTDIR:-}" ]; then
  case $DESTDIR in
    */) unit=$DESTDIR${unit#/} ;;
    *) unit=$DESTDIR$unit ;;
  esac
  print=1
fi

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
template=$here/$template_name

sed_escape() {
  printf '%s' "$1" | sed 's/[&|\\]/\\&/g'
}

xml_escape() {
  printf '%s' "$1" | sed 's/&/\&amp;/g; s/</\&lt;/g; s/>/\&gt;/g'
}

fill() {
  binary=$1
  home_value=$2
  if [ "$os" = Darwin ]; then
    binary=$(xml_escape "$binary")
    home_value=$(xml_escape "$home_value")
  fi
  binary=$(sed_escape "$binary")
  home_value=$(sed_escape "$home_value")
  sed "s|@KYOTOAGENT@|$binary|g; s|@HOME@|$home_value|g" "$template"
}

add_listen() {
  unit_path=$1
  if [ -z "$listen" ]; then
    return 0
  fi
  revised=$(mktemp "$unit_path.XXXXXX")
  if [ "$os" = Darwin ]; then
    LISTEN_ADDR=$(xml_escape "$listen") awk '
      BEGIN { addr = ENVIRON["LISTEN_ADDR"] }
      {
        print
        if (!inserted && index($0, "<string>serve</string>") > 0) {
          print "    <string>--listen</string>"
          print "    <string>" addr "</string>"
          inserted = 1
        }
      }
      END {
        if (!inserted) {
          print "install-serve.sh: could not add --listen" > "/dev/stderr"
          exit 1
        }
      }
    ' "$unit_path" > "$revised"
  else
    LISTEN_ADDR=$listen awk '
      BEGIN { addr = ENVIRON["LISTEN_ADDR"] }
      {
        if (!inserted && $0 ~ /^ExecStart=/ && $0 ~ / serve$/) {
          print $0 " --listen " addr
          inserted = 1
        } else {
          print
        }
      }
      END {
        if (!inserted) {
          print "install-serve.sh: could not add --listen" > "/dev/stderr"
          exit 1
        }
      }
    ' "$unit_path" > "$revised"
  fi
  mv "$revised" "$unit_path"
}

apply_stop() {
  uid=$(id -u)
  case $os in
    Linux)
      systemctl --user disable --now kyotoagent.service || true
      ;;
    Darwin)
      launchctl bootout "gui/$uid" "$unit" 2>/dev/null || launchctl unload -w "$unit" 2>/dev/null || true
      ;;
  esac
}

apply_start() {
  uid=$(id -u)
  case $os in
    Linux)
      systemctl --user daemon-reload
      systemctl --user enable --now kyotoagent.service
      user=${USER:-$(id -un)}
      loginctl enable-linger "$user"
      ;;
    Darwin)
      launchctl bootout "gui/$uid" "$unit" 2>/dev/null || launchctl unload "$unit" 2>/dev/null || true
      if ! launchctl bootstrap "gui/$uid" "$unit"; then
        launchctl load -w "$unit"
      fi
      ;;
  esac
}

if [ "$uninstall" -eq 1 ]; then
  if [ "$print" -eq 0 ]; then
    apply_stop
    if [ "$os" = Linux ]; then
      rm -f "$unit"
      systemctl --user daemon-reload
      exit 0
    fi
  fi
  rm -f "$unit"
  exit 0
fi

command_path() {
  command -v "$1" 2>/dev/null || true
}

kyotoagent_path=$(command_path kyotoagent)
kyoto_path=$(command_path kyoto)
missing_name=
if [ -n "$kyotoagent_path" ]; then
  found=$kyotoagent_path
  if [ -z "$kyoto_path" ]; then
    missing_name=kyoto
  fi
elif [ -n "$kyoto_path" ]; then
  found=$kyoto_path
  missing_name=kyotoagent
else
  echo "kyotoagent is not on PATH" >&2
  exit 1
fi

case $found in
  /*) found_abs=$found ;;
  *) found_abs=$PWD/$found ;;
esac
link_dir=$(CDPATH= cd -- "$(dirname -- "$found_abs")" && pwd)
if [ -n "$missing_name" ]; then
  link=$link_dir/$missing_name
  if [ ! -e "$link" ]; then
    ln -s "$(basename -- "$found_abs")" "$link"
  fi
fi
bin=$found_abs
if command -v realpath >/dev/null 2>&1; then
  bin=$(realpath "$bin")
fi

mkdir -p "$(dirname -- "$unit")"
tmp=$(mktemp "$unit.XXXXXX")
fill "$bin" "$home" > "$tmp"
add_listen "$tmp"
mv "$tmp" "$unit"
tmp=

if [ "$print" -eq 1 ]; then
  printf '%s\n' "$unit"
  exit 0
fi

apply_start
