# Qivomedia Player for Windows and Linux

A small native app (about 4 MB to download; Tauri with Rust, using WebView2 on Windows and WebKitGTK on Linux) that shows the Qivomedia web player full screen and adds what a browser can't do:

| | |
|---|---|
| **Kiosk** | Full screen, always on top, no cursor, no right-click or zoom; videos with sound start on their own |
| **Watchdog** | Restarts the player if it crashes, freezes (no heartbeat for 2 min) or is closed, plus a daily restart (default 04:00). Repeated failures back off up to 1 min. |
| **Always on** | The computer never sleeps or blanks the screen while the player runs |
| **Remote control** (dashboard → Displays) | Screenshot, Restart player, Restart computer, Screen off / on |
| **Device health** (dashboard) | Computer name, system version, CPU, memory, disk, temperature (where reported), uptime, resolution, refresh rate and scaling of each screen |
| **Offline start** | Once a screen has loaded content, it boots and plays without a network |
| **Updates** | The content and player logic come from your server, so a server update reaches every screen with no reinstall |

## Windows: install on one PC

1. Download the installer from the **Desktop player** workflow on GitHub (artifact `qivomedia-player-windows`): `Qivomedia Player_x.y.z_x64-setup.exe` (or the `.msi` for Intune/GPO).
2. Run it. It installs WebView2 if the PC doesn't have it.
3. Start **Qivomedia Player**, type the server address (for example `signage.univ.ma` or `10.10.0.5:8080`) and choose **Save and start**. Without `https://` or `http://`, it tries HTTPS first, then HTTP.
4. The screen shows a 6-character code. In the dashboard: **Displays → Add screen**.

## Windows: turn a PC into a dedicated player (recommended)

After installing, run in an administrator PowerShell:

```powershell
.\setup-kiosk.ps1 -Server https://signage.univ.ma
```

It creates a `signage` account that signs in at boot and goes straight to the player, with no desktop or taskbar (Shell Launcher). It also turns off sleep and the lock screen and keeps Windows Update restarts outside 06:00–23:00. Administrators keep the normal desktop: Ctrl+Alt+Del → Sign out.

Shell Launcher needs Windows 10/11 **Enterprise, Education or IoT Enterprise**. We recommend **Windows 11 IoT Enterprise LTSC 2024**, supported until 2034, with no feature updates and no surprise changes. On Pro, the script starts the player at sign-in instead.

## Linux: HP t430 thin client (or any small Intel box)

HP ThinPro is too locked down to install apps on, so replace it with **Debian 13**. It's free and light, with security updates until 2028 (2030 with Debian LTS). The box then works like a dedicated player: it boots straight to the screen with no desktop, no cursor and nothing to click.

**You need:** a USB stick (4 GB or more), a keyboard, a wired network, and the two files from the **Desktop player** workflow on GitHub (artifact `qivomedia-player-linux`): `qivomedia-player_amd64.deb` and `setup-kiosk.sh`.

1. **Make the USB stick:** download the Debian 13 "netinst" image (amd64) from debian.org and write it with Rufus or balenaEtcher.
2. **Boot the t430 from USB:** power on and press **F9** (boot menu), then choose the USB stick. If it doesn't appear, press **F10** for the BIOS settings and allow USB boot.
3. **Install Debian** (about 15 minutes) and accept the defaults, except:
   - Set a password for **root** or for your admin user, and note it.
   - At *Partition disks*: *Guided, use entire disk* (the 32 GB eMMC). **This erases ThinPro.**
   - At *Software selection*: untick **everything** except **SSH server** and **standard system utilities**. No desktop environment.
4. **Copy the two files** onto the box (on a USB stick, or with `scp` from your computer), then run:
   ```bash
   sudo bash setup-kiosk.sh --server https://signage.univ.ma --deb qivomedia-player_amd64.deb
   sudo reboot
   ```
   For a portrait screen, add `--rotate left` (or `right`).
5. After the restart, the screen shows the pairing code. In the dashboard: **Displays → Add screen**.

What the script sets up:
- automatic sign-in to a `signage` account on the screen, running only the player;
- Intel hardware video decoding;
- Arabic and emoji fonts;
- remote **Restart computer** from the dashboard;
- small logs, to spare the eMMC;
- automatic security updates, with a restart at 03:30 when one is needed.

You can run it again at any time, for example to change the server or install a newer `.deb`.

**Maintenance:** connect over SSH, or press **Ctrl+Alt+F2** on the box for a text console.
- `vainfo --display drm` shows which video formats the box decodes in hardware.
- **No sound on the TV** (it goes to the headset jack instead): `sudo -u signage XDG_RUNTIME_DIR=/run/user/$(id -u signage) wpctl status` lists the outputs; choose the HDMI/DisplayPort one with `wpctl set-default <id>`.

**Expectations for the t430** (Celeron N4020, 4 GB RAM):
- Very good for Full HD screens.
- 4K images and text: fine through the **DisplayPort** output.
- 4K video: use HEVC at 30 fps, or play 1080p video and let the TV upscale it. There is no AV1 hardware decoding.
- Keep about 10 GB free for content. The dashboard warns below 5 GB.

## Deploying many PCs

- **Server address for everyone:** `%ProgramData%\Qivomedia\Player\defaults.json` (Linux: `/etc/qivomedia-player/defaults.json`)
  ```json
  { "server_url": "https://signage.univ.ma", "restart_at": "04:00", "autostart": true }
  ```
  Each user's own settings (`%LOCALAPPDATA%\Qivomedia\Player\config.json`) take priority once saved.
- **Command line:** `qivo-player.exe --server https://signage.univ.ma` saves the address and starts.
- **Silent install:** `msiexec /i "Qivomedia Player_x.y.z_x64_en-US.msi" /qn`, or the setup `.exe` with `/S`.

## On the screen

- **Ctrl+Shift+F10** opens the setup screen (change server, daily restart time, exit).
- **Exit player** on that screen closes it for good, until the next sign-in. Any other way of closing it gets it restarted by the watchdog.

## Files

Everything for a user lives in `%LOCALAPPDATA%\Qivomedia\Player` (Linux: `~/.config/qivomedia-player`):

| File | |
|---|---|
| `config.json` | settings |
| `player.log` | starts, restarts and why, remote commands (kept to about 2 MB) |
| `webview\` | pairing key and the offline copy of content |

**Delete the folder to reset the screen completely** (it will show a new pairing code).

## Crystal-clear 4K: checklist

1. **Windows display settings:** 3840 × 2160 at 60 Hz. Scale 100 %, 150 % or 200 % (other values blur). The dashboard warns about unusual scaling.
2. **TV, sharp text:** turn on full colour detail (4:4:4) for that HDMI input. Samsung: *Input Signal Plus*. LG: rename the input to **PC**. Sony: *HDMI signal format → Enhanced*.
3. **TV, picture:** *Just Scan* / *Screen Fit* (overscan off), motion smoothing off, extra sharpening off.
4. **Colour range:** set full RGB on both the PC (Intel Graphics Command Center → Display → Color → Quantization Range: Full) and the TV (HDMI black level: Normal/High). A mismatch makes blacks look grey.
5. **Video:** export at 30 or 60 fps. 25 fps material judders on a 60 Hz output. Use H.264 or HEVC; AV1 needs an Intel N100/N150 or newer.
6. **Cable:** a certified *Premium High Speed* HDMI cable (18 Gbit/s) for 4K at 60 Hz.

## Recommended hardware

A fanless mini PC with an **Intel N150** (hardware decoding of 4K H.264, HEVC, VP9 and AV1), **8 GB RAM**, a **256 GB SSD**, **HDMI 2.0+**, wired Ethernet and TPM 2.0. In the BIOS, turn on **power on after AC loss**. Video walls or very heavy animation: Core i3/i5 class.

## Development

```bash
cd players/desktop/src-tauri
cargo run -- --no-supervisor --windowed      # the player directly, in a window
cargo run -- --windowed                      # with the watchdog
QIVO_PLAYER_HOME=/tmp/qp cargo run -- --server http://127.0.0.1:8080 --windowed
npx @tauri-apps/cli@2 build                  # installers (on Windows)
```

On Linux it uses WebKitGTK. **Restart computer** uses systemd; **Screen off/on** needs an X11 display with power management. The app reports exactly which commands each machine supports, and the dashboard only shows those.

**Security:** only pages from the configured server can call the native functions, and only these: device info, screenshot, heartbeat, restart player, restart computer, screen power and open setup. The setup functions, which change which server is trusted, are available only to the app's own setup screen. On the server, restarting a computer, restarting the player and screen power are admin-only and recorded in the audit log.
