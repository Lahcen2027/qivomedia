# Qivomedia Player for Windows

A small native app (about 10 MB, Tauri with Rust and WebView2) that shows the Qivomedia web player full screen and adds what a browser can't do:

| | |
|---|---|
| **Kiosk** | Full screen, always on top, no cursor, no right-click or zoom; videos with sound start on their own |
| **Watchdog** | Restarts the player if it crashes, freezes (no heartbeat for 2 min) or is closed, plus a daily restart (default 04:00). Repeated failures back off up to 1 min. |
| **Always on** | Windows never sleeps or blanks the screen while the player runs |
| **Remote control** (dashboard → Displays) | Screenshot, Restart player, Restart computer, Screen off / on |
| **Device health** (dashboard) | Computer name, Windows version, CPU, memory, disk, temperature (where reported), uptime, resolution, refresh rate and scaling of each screen |
| **Offline start** | Once a screen has loaded content, it boots and plays without a network |
| **Updates** | The content and player logic come from your server, so a server update reaches every screen with no reinstall |

## Install on one PC

1. Download the installer from the **Windows player** workflow on GitHub (artifact `qivomedia-player-windows`): `Qivomedia Player_x.y.z_x64-setup.exe` (or the `.msi` for Intune/GPO).
2. Run it. It installs WebView2 if the PC doesn't have it.
3. Start **Qivomedia Player**, type the server address (for example `signage.univ.ma` or `10.10.0.5:8080`) and choose **Save and start**. Without `https://` or `http://`, it tries HTTPS first, then HTTP.
4. The screen shows a 6-character code. In the dashboard: **Displays → Add screen**.

## Turn a PC into a dedicated player (recommended)

After installing, run in an administrator PowerShell:

```powershell
.\setup-kiosk.ps1 -Server https://signage.univ.ma
```

It creates a `signage` account that signs in at boot and goes straight to the player, with no desktop or taskbar (Shell Launcher). It also turns off sleep and the lock screen and keeps Windows Update restarts outside 06:00–23:00. Administrators keep the normal desktop: Ctrl+Alt+Del → Sign out.

Shell Launcher needs Windows 10/11 **Enterprise, Education or IoT Enterprise**. We recommend **Windows 11 IoT Enterprise LTSC 2024**, supported until 2034, with no feature updates and no surprise changes. On Pro, the script starts the player at sign-in instead.

## Deploying many PCs

- **Server address for everyone:** `%ProgramData%\Qivomedia\Player\defaults.json`
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

Everything for a Windows user lives in `%LOCALAPPDATA%\Qivomedia\Player`:

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
cd players/windows/src-tauri
cargo run -- --no-supervisor --windowed      # the player directly, in a window
cargo run -- --windowed                      # with the watchdog
QIVO_PLAYER_HOME=/tmp/qp cargo run -- --server http://127.0.0.1:8080 --windowed
npx @tauri-apps/cli@2 build                  # installers (on Windows)
```

On Linux it runs with WebKitGTK for testing. Reboot and screen power are Windows-only, and the app reports exactly which commands it supports.

**Security:** only pages from the configured server can call the native functions, and only these: device info, screenshot, heartbeat, restart player, restart computer, screen power and open setup. The setup functions, which change which server is trusted, are available only to the app's own setup screen. On the server, restarting a computer, restarting the player and screen power are admin-only and recorded in the audit log.
