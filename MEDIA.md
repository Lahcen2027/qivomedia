# Media formats and live streams

## Supported formats

Upload anything below in **Media library**. Files that screens can't play directly are **converted automatically** on the server. The tile shows "Converting… 45%", and the item goes live by itself when it's ready. Screens never receive a file they can't play.

| Format | On screens | What happens on upload |
|---|---|---|
| **JPEG, PNG, GIF** (animated too), WebP, SVG | ✅ | Used as they are |
| **TIFF** | ✅ | Converted to PNG |
| **H.264** (MP4, MOV) | ✅ | Used as it is. Converted only if it's 10-bit, interlaced, larger than 4K or has unsupported audio |
| **AVI** (DivX, Xvid, MJPEG…) | ✅ | Converted to H.264 MP4 |
| **MPEG-2** (.mpg, .mpeg) | ✅ | Converted to H.264 MP4 and **deinterlaced** |
| **VOB** (DVD) | ✅ | Converted to H.264 MP4, deinterlaced, with AC-3 audio converted to AAC |
| **MPEG-TS** (.ts, .m2ts: broadcast and IPTV recordings) | ✅ | Converted to H.264 MP4 |
| **H.265 / HEVC**, WMV, FLV, MKV | ✅ | Converted to H.264 MP4 |
| WebM (VP8, VP9, AV1) | ✅ | Used as it is |
| **MP3**, AAC/M4A, Ogg, WAV, FLAC | ✅ | Used as they are |
| WMA, AMR, AC-3… | ✅ | Converted to AAC |
| **PDF** | ✅ | Every page becomes a sharp image (long side 3840 px), shown as a slideshow. Set the seconds per page in the playlist. Up to 200 pages. |
| HTML | ✅ | Shown in a sandbox |

**Conversion settings:** H.264 High profile, 8-bit 4:2:0, quality CRF 20, at most 3840 px on the long side, AAC 192 kb/s stereo, and "fast start" so playback begins before the download ends. H.264 video that's already compatible is copied without re-encoding, so there's no quality loss.

**Speed:** on 2 CPU cores, a 40-second 1080p DVD clip converts in about 25 seconds. Conversions run one at a time (set `QIVO_TRANSCODE_JOBS` for more), continue in the background if you close the page, and resume after a server restart.

## Live streams (IP cameras, encoders, IPTV)

**Media library → Live streams → + Add stream**, then add it to any zone of a layout with **● Live stream** in the playlist editor.

| Source | Example address | Notes |
|---|---|---|
| IP camera (**RTSP**) | `rtsp://user:password@192.168.1.20:554/stream1` | Most cameras. TCP transport is used for reliability. |
| Hikvision | `rtsp://user:password@IP:554/Streaming/Channels/101` | `102` is the lighter sub-stream |
| Dahua | `rtsp://user:password@IP:554/cam/realmonitor?channel=1&subtype=0` | `subtype=1` is the sub-stream |
| Axis | `rtsp://user:password@IP/axis-media/media.amp` | |
| IPTV **multicast** | `udp://@239.10.10.1:1234` | MPEG-TS over UDP (see the network note below) |
| Encoder (**SRT**) | `srt://10.0.0.8:9000` | |
| Encoder (**RTMP**) | `rtmp://10.0.0.8/live/channel1` | The server pulls the stream |
| **HLS** channel | `https://example.com/live/index.m3u8` | Can be relayed, or played by screens directly |

**How it works**
- The server **relays** the stream and screens play it as HLS. H.264 sources are passed through as they are, with almost no CPU use. **H.265** cameras and **MPEG-2** IPTV are converted to H.264 live.
- A relay **starts when a screen shows the stream and stops 45 s after the last screen stops**, so cameras aren't loaded for nothing.
- If a camera reboots or the network drops, the relay **reconnects by itself**, and the screen resumes. A picture that freezes for 20 seconds is restarted.
- Delay is about **4–8 seconds** behind live. That's normal for HLS and fine for signage, but not suited to surveillance where every second counts.
- **Test** checks the connection and shows the codec and resolution. **Preview** shows exactly what screens receive.
- **Capacity:** passing H.264 through costs very little, and one server can relay dozens of streams. Converting H.265 1080p costs about one CPU core per stream. Limit: `QIVO_MAX_RELAYS` (default 16).

**Network notes**
- The **server** must be able to reach the cameras. For a hosted server outside the campus, that means a VPN or a site-to-site link, because cameras should never be exposed to the internet.
- **Multicast** reaches a Docker container only with host networking. Add `network_mode: host` to the `qivomedia` service, or relay the channel through an encoder that sends unicast.
- To keep the main stream for recording, use the camera's **sub-stream** (lower resolution) for screens. It's lighter on the network.

**Security**
- Camera passwords are stored **encrypted** and are never shown again: the dashboard shows `****`.
- Only streaming protocols are accepted (`rtsp`, `rtmp`, `srt`, `udp`, `rtp`, `http(s)`), never `file:` or similar.
- The server itself and cloud-metadata addresses are refused. On a hosted multi-customer server, set `QIVO_STREAM_ALLOW_PRIVATE=0` to refuse private networks too.
- Screens get a stream address containing a key unique to that stream (derived from the server's secret), so it can't be guessed. Deleting the stream revokes it.
- Only admins can add or change streams. Editors can use them in playlists.
