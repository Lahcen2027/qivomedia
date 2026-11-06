#!/usr/bin/env bash
# Turns a fresh Debian 13 install into a dedicated Qivomedia signage player.
# Made for small boxes such as the HP t430 thin client (after replacing ThinPro).
#
#   sudo ./setup-kiosk.sh --server https://signage.univ.ma --deb ./qivomedia-player_0.1.0_amd64.deb
#   options: --user signage   --rotate left|right|inverted   (portrait screens)
#
# It installs a minimal graphical session (X11 + Openbox) and video decoding for Intel
# graphics. A "signage" account signs in automatically at boot and runs only the player.
# The script also turns off blanking, allows remote restart from the dashboard, keeps logs
# small (eMMC storage), and installs security updates automatically with a restart at 03:30.
# Safe to run again (for example to change the server or install a newer .deb).
set -euo pipefail

SERVER="" DEB="" KIOSK_USER="signage" ROTATE=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --server) SERVER="$2"; shift 2 ;;
    --deb) DEB="$2"; shift 2 ;;
    --user) KIOSK_USER="$2"; shift 2 ;;
    --rotate) ROTATE="$2"; shift 2 ;;
    -h|--help) sed -n '2,13p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 1 ;;
  esac
done
step() { printf '\n\033[1;36m==> %s\033[0m\n' "$*"; }
[[ $EUID -eq 0 ]] || { echo "Run as root: sudo $0 ..." >&2; exit 1; }
[[ -n "$SERVER" ]] || { echo "--server is required (for example --server https://signage.univ.ma)" >&2; exit 1; }
[[ "$SERVER" =~ ^https?:// ]] || SERVER="https://$SERVER"
SERVER="${SERVER%/}"
if [[ -n "$ROTATE" && ! "$ROTATE" =~ ^(left|right|inverted|normal)$ ]]; then
  echo "--rotate must be left, right, inverted or normal" >&2; exit 1
fi

# gstreamer1.0-gl lets WebKit hand decoded video straight to the GPU ("zero-copy"):
# without it a small Celeron spends its CPU copying 4K frames.
step "Packages: X11, Openbox, Intel video decoding, sound, fonts (Latin, Arabic, emoji)"
export DEBIAN_FRONTEND=noninteractive
apt-get update -q
apt-get install -y -q --no-install-recommends \
  xserver-xorg-core xserver-xorg-input-libinput xinit x11-xserver-utils x11-utils openbox \
  gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad gstreamer1.0-libav \
  gstreamer1.0-gl intel-media-va-driver i965-va-driver vainfo \
  dbus-x11 pipewire-audio gstreamer1.0-pipewire \
  fonts-noto-core fonts-noto-color-emoji fonts-dejavu-core \
  polkitd unattended-upgrades ca-certificates
# Older hardware-decoding plugin, used by some WebKit versions; fine if missing.
apt-get install -y -q --no-install-recommends gstreamer1.0-vaapi 2>/dev/null || true

if [[ -n "$DEB" ]]; then
  step "Qivomedia Player from $DEB"
  apt-get install -y -q "$(realpath "$DEB")"
fi
PLAYER="$(command -v qivo-player || true)"
[[ -n "$PLAYER" ]] || { echo "qivo-player is not installed: pass --deb <file>.deb" >&2; exit 1; }

step "Server address: $SERVER"
install -d /etc/qivomedia-player
cat > /etc/qivomedia-player/defaults.json <<EOF
{ "server_url": "$SERVER", "restart_at": "04:00", "autostart": false }
EOF

step "Account '$KIOSK_USER' with automatic sign-in on the screen"
id "$KIOSK_USER" &>/dev/null || useradd --create-home --shell /bin/bash --comment "Qivomedia signage" "$KIOSK_USER"
usermod -aG video,render,audio "$KIOSK_USER"
install -d /etc/systemd/system/getty@tty1.service.d
cat > /etc/systemd/system/getty@tty1.service.d/autologin.conf <<EOF
[Service]
ExecStart=
ExecStart=-/sbin/agetty --autologin $KIOSK_USER --noclear %I \$TERM
EOF
HOME_DIR="$(getent passwd "$KIOSK_USER" | cut -d: -f6)"
# Start the graphical session on the physical screen only (not over SSH). No mouse cursor.
PROFILE_LINE='[ -z "$DISPLAY" ] && [ "$(tty)" = /dev/tty1 ] && exec startx -- -nocursor -keeptty >"$HOME/.xsession.log" 2>&1'
grep -qF "startx -- -nocursor" "$HOME_DIR/.profile" 2>/dev/null || echo "$PROFILE_LINE" >> "$HOME_DIR/.profile"
echo "exec openbox-session" > "$HOME_DIR/.xinitrc"
install -d "$HOME_DIR/.config/openbox"
{
  echo "# Written by setup-kiosk.sh"
  echo "xset s off s noblank -dpms"
  if [[ -n "$ROTATE" ]]; then
    echo "xrandr --output \"\$(xrandr | awk '/ connected/{print \$1; exit}')\" --rotate $ROTATE"
  fi
  echo "\"$PLAYER\" &"
} > "$HOME_DIR/.config/openbox/autostart"
chown -R "$KIOSK_USER:$KIOSK_USER" "$HOME_DIR/.profile" "$HOME_DIR/.xinitrc" "$HOME_DIR/.config"

step "Allow '$KIOSK_USER' to restart the computer (dashboard → Restart computer)"
install -d /etc/polkit-1/rules.d
cat > /etc/polkit-1/rules.d/50-qivomedia-reboot.rules <<EOF
// Qivomedia: the signage account may restart the computer when the dashboard asks.
polkit.addRule(function (action, subject) {
  if ((action.id == "org.freedesktop.login1.reboot" ||
       action.id == "org.freedesktop.login1.reboot-multiple-sessions") &&
      subject.user == "$KIOSK_USER") {
    return polkit.Result.YES;
  }
});
EOF

step "Quiet, fast boot"
if [[ -f /etc/default/grub ]]; then
  sed -i 's/^GRUB_TIMEOUT=.*/GRUB_TIMEOUT=1/' /etc/default/grub
  sed -i 's/^GRUB_CMDLINE_LINUX_DEFAULT=.*/GRUB_CMDLINE_LINUX_DEFAULT="quiet loglevel=3"/' /etc/default/grub
  update-grub 2>/dev/null || true
fi

step "Small logs (protects the eMMC storage)"
install -d /etc/systemd/journald.conf.d
printf '[Journal]\nSystemMaxUse=50M\n' > /etc/systemd/journald.conf.d/qivomedia.conf

step "Automatic security updates, restarting at 03:30 when needed"
cat > /etc/apt/apt.conf.d/52qivomedia <<'EOF'
APT::Periodic::Update-Package-Lists "1";
APT::Periodic::Unattended-Upgrade "1";
Unattended-Upgrade::Automatic-Reboot "true";
Unattended-Upgrade::Automatic-Reboot-Time "03:30";
EOF

echo
echo "Video decoding check (look for H264 / HEVC / VP9 'VLD' lines):"
vainfo 2>/dev/null | grep -E "VAProfile(H264|HEVC|VP9|AV1).*VLD" || echo "  vainfo found no hardware decoder: this runs before X starts, so check again after reboot over SSH: vainfo --display drm"
printf '\n\033[1;32mDone.\033[0m Restart now (sudo reboot): the box signs in as %s and shows the pairing code.\n' "$KIOSK_USER"
echo "Maintenance: Ctrl+Alt+F2 for a text console (sign in with your admin account), or SSH."
