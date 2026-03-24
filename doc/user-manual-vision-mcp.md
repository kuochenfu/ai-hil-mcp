# vision-mcp User Manual

**Version:** 1.1 · 2026-03-25

> Visual perception layer for AI-HIL — frame capture, software PTZ, image adjustment, and LED state detection via any UVC USB camera.

---

## Overview

`vision-mcp` gives Claude Code the ability to see the physical hardware setup. It captures frames from the camera, applies software pan/tilt/zoom and image adjustments, and detects LED states on the board — either with local OpenCV processing or via Claude vision API as a fallback.

### Tool Summary

| Tool | Category | Description |
|------|----------|-------------|
| `list_cameras` | Discovery | Enumerate camera indices and resolutions |
| `get_camera_info` | Discovery | Show all current settings (PTZ, adjustments, resolution) |
| `set_resolution(width, height)` | Configuration | Switch capture resolution |
| `set_ptz(pan, tilt, zoom)` | Configuration | Software pan/tilt/zoom — persistent |
| `adjust_image(brightness, contrast, saturation, sharpness)` | Configuration | Post-capture image transforms — persistent |
| `set_focus(mode)` | Configuration | `"auto"` or `"manual"` focus via AVFoundation |
| `capture_frame` | Perception | Capture a JPEG with current settings applied |
| `analyze_frame(prompt)` | Perception | Claude vision analysis with current settings applied |
| `detect_led_state` | Perception | LED color/on-off detection — OpenCV first, Claude fallback |

---

## Camera Selection

### macOS UVC limitations

Before choosing a camera, understand what macOS actually allows `vision-mcp` to control:

macOS kernel driver (`AppleUSBVideoSupport`) claims the UVC VideoControl interface while the camera is active. This means:

- **Direct UVC control transfers via `pyusb` are blocked** — the kernel intercepts them
- **OpenCV's AVFoundation backend returns 0 (read-only) for all camera properties** — brightness, contrast, pan, tilt, focus, exposure, white balance, zoom — only resolution and FPS are writable
- **All PTZ and image adjustments in `vision-mcp` are software workarounds** — post-capture crop/resize and OpenCV transforms applied to the captured frame

Practically, **any 1080p UVC webcam delivers the same functional result as a 4K camera** for AI-HIL use cases. The extra sensor resolution and hardware ePTZ of premium cameras like the MX Brio are inaccessible from Claude Code on macOS.

### Recommended cameras

| Camera | Price | Resolution | Notes |
|--------|-------|-----------|-------|
| **Logitech C920 / C922** | $50–70 | 1080p | ✅ Recommended — proven macOS UVC compatibility, widely available |
| Logitech C505 | $30–40 | 720p | Budget option — sufficient for LED detection |
| Razer Kiyo | $50–60 | 1080p | Built-in ring light — useful for consistent LED illumination |
| Elgato Facecam | $80–100 | 1080p | Fixed focus, sharp at close range — good for board inspection |
| Generic 1080p UVC webcam | $15–30 | 1080p | Minimum viable option |
| Logitech MX Brio Ultra 4K | $199 | 1080p* | ⚠️ Overkill — 4K sensor and hardware ePTZ are inaccessible on macOS |

\* Maximum usable resolution via AVFoundation/OpenCV is 1080p regardless of sensor size.

**Bottom line:** Buy a **Logitech C920** unless you already own a different camera. The C920 costs $130+ less than the MX Brio and delivers identical results in this setup.

---

## Hardware Setup

### Camera

- **Supported:** Any UVC USB camera. Verified on Logitech MX Brio Ultra 4K (index 0)
- **macOS camera permission:** Grant access to your terminal app in
  `System Settings → Privacy & Security → Camera`
- **Camera index:** Run `list_cameras` to find the correct index

### Mounting Recommendation

For board inspection, mount the camera directly above the NUCLEO board:

```
        [ Camera — pointing down ]
                  |
             ~20–40 cm
                  |
        [ NUCLEO-WL55JC1 board ]
```

A closer distance improves LED detection confidence. Use `set_ptz` to reframe without physically moving the camera.

---

## Quick Start

### 1. Confirm camera is visible

```
list_cameras()
```

Expected:
```
[0] 1920x1080    ← MX Brio
[1] 1920x1080    ← MacBook Pro built-in
[2] 1920x1080    ← iPhone Continuity Camera (if connected)
```

### 2. Check current settings

```
get_camera_info()
```

Expected (defaults):
```
Camera index : 0
Resolution   : 1920x1080
Pan          : +0.00  (-1=full left, +1=full right)
Tilt         : +0.00  (-1=full up,   +1=full down)
Zoom         : 1.00x
Brightness   : +0    (-100…+100)
Contrast     : 1.00  (1.0=normal)
Saturation   : 1.00  (1.0=normal)
Sharpness    : 0     (0…10)
```

### 3. Capture a frame

```
capture_frame()
```

Returns a base64-encoded JPEG. The raw image data can be saved locally:

```bash
! python3 -c "
import base64, re
data = '<paste FRAME_BASE64 value here>'
open('/tmp/frame.jpg','wb').write(base64.b64decode(data))
"
```

### 4. Detect LED states

```
detect_led_state()
```

Example output (OpenCV):
```
OpenCV detected 2 LED(s) [confidence: 78%]:
  1. GREEN — ON — middle-center (px:962,540  area:124px²  conf:82%)
  2. BLUE  — ON — top-right     (px:1480,210  area:88px²   conf:74%)
```

### 5. Ask a question about the board

```
analyze_frame(prompt="Is the NUCLEO board powered on? Are any LEDs lit?")
```

Requires `ANTHROPIC_API_KEY` to be set in the environment.

---

## Tool Reference

### `list_cameras`

Scans indices 0–7 and returns those that open successfully.

```
list_cameras()
```

No parameters. Returns one line per camera: `[index] widthxheight`.

---

### `get_camera_info`

Returns the full current state including all PTZ values and image adjustments.

```
get_camera_info()
```

No parameters. Useful for confirming settings before a capture.

---

### `set_resolution(width, height)`

Switches the capture resolution. Applies to the next `capture_frame` or `detect_led_state` call.

```
set_resolution(width=1920, height=1080)   # default — Full HD
set_resolution(width=1280, height=720)    # HD — faster capture
set_resolution(width=848,  height=480)    # low-res — very fast
```

**Supported resolutions (MX Brio):**

| Resolution | Use case |
|-----------|----------|
| 1920×1080 | Default — best detail for LED detection |
| 1280×720 | Faster, good for general analysis |
| 848×480 | Fast triage |
| 640×480 | Legacy / maximum FPS |
| 320×240 | Minimal latency |

If the exact resolution is not supported, the camera snaps to the nearest available mode. The tool reports the actual resolution set.

---

### `set_ptz(pan, tilt, zoom)`

Sets software pan/tilt/zoom. Applied by cropping the captured frame and resizing back to the original dimensions.

**Settings are persistent** — they apply to every subsequent `capture_frame`, `analyze_frame`, and `detect_led_state` call until changed.

```
set_ptz(pan=0.0, tilt=0.0, zoom=1.0)    # center, no zoom (default)
set_ptz(pan=-0.5, tilt=0.0, zoom=2.0)   # 2× zoom, panned left
set_ptz(pan=0.3, tilt=-0.3, zoom=1.5)   # upper-right area, 1.5× zoom
```

**Parameters:**

| Parameter | Range | Description |
|-----------|-------|-------------|
| `pan` | `-1.0` … `+1.0` | Horizontal position. `-1`=full left, `0`=center, `+1`=full right |
| `tilt` | `-1.0` … `+1.0` | Vertical position. `-1`=full up, `0`=center, `+1`=full down |
| `zoom` | `1.0` … `4.0` | Zoom factor. `1.0`=no zoom, `2.0`=2× crop, max `4.0` |

**Note:** Software ePTZ works by cropping the 1080p frame. At zoom=4.0 the effective resolution of the cropped area is 480×270 — quality degrades. For best results keep zoom ≤ 2.0.

To reset to defaults:
```
set_ptz(pan=0.0, tilt=0.0, zoom=1.0)
```

---

### `adjust_image(brightness, contrast, saturation, sharpness)`

Sets post-capture image adjustments. Applied to every frame after PTZ cropping.

**Settings are persistent** — apply to all subsequent captures until changed.

```
adjust_image(brightness=0, contrast=1.0, saturation=1.0, sharpness=0)   # defaults
adjust_image(brightness=30, contrast=1.2)                                # brighter + more contrast
adjust_image(saturation=0.0)                                              # grayscale
adjust_image(sharpness=5)                                                 # sharpen edges
```

**Parameters:**

| Parameter | Range | Default | Description |
|-----------|-------|---------|-------------|
| `brightness` | `-100` … `+100` | `0` | Additive pixel offset. Negative = darker, positive = brighter |
| `contrast` | `0.0` … `3.0` | `1.0` | Multiplicative scale. `< 1.0` = low contrast, `> 1.0` = high contrast |
| `saturation` | `0.0` … `3.0` | `1.0` | Color intensity. `0.0` = grayscale, `1.0` = normal, `> 1.0` = vivid |
| `sharpness` | `0` … `10` | `0` | Unsharp mask strength. `0` = off, `10` = maximum |

**Tips for board inspection:**
- If LEDs are hard to detect under bright ambient light: `adjust_image(contrast=1.5, saturation=1.5)`
- In a dark environment: `adjust_image(brightness=40)`
- If LED blob edges are blurry: `adjust_image(sharpness=4)`

To reset to defaults:
```
adjust_image(brightness=0, contrast=1.0, saturation=1.0, sharpness=0)
```

---

### `set_focus(mode)`

Sets the camera focus mode via AVFoundation.

```
set_focus(mode="auto")    # continuous autofocus (default camera behavior)
set_focus(mode="manual")  # lock focus at current position
```

**When to use manual focus:** If the camera keeps refocusing on background objects instead of the board, set to `"manual"` once the board is in focus.

**Note:** Requires `pyobjc-framework-AVFoundation` (installed in the project venv). May not take effect if OpenCV is simultaneously capturing from the same camera.

---

### `capture_frame`

Captures a still frame and returns it as a base64-encoded JPEG. All current PTZ and image adjustments are applied.

```
capture_frame()                              # use current settings
capture_frame(camera_index=1)               # override camera index for this call
capture_frame(width=1280, height=720)       # override resolution for this call
```

**Parameters:**

| Parameter | Default | Description |
|-----------|---------|-------------|
| `camera_index` | `-1` (use current) | Camera index. `-1` = use state setting |
| `width` | `-1` (use current) | Capture width. `-1` = use state setting |
| `height` | `-1` (use current) | Capture height. `-1` = use state setting |

Returns:
```
FRAME_BASE64:<base64-encoded JPEG>
SIZE:142KB  RES:1920x1080
```

---

### `analyze_frame(prompt)`

Captures a frame and sends it to `claude-opus-4-6` for vision analysis. All current PTZ and image adjustments are applied before sending.

```
analyze_frame(prompt="Describe what you see.")
analyze_frame(prompt="Is the green LED on or off?")
analyze_frame(prompt="Are there any visible error indicators on the board?")
analyze_frame(prompt="What is displayed on the LCD screen?")
```

**Parameters:**

| Parameter | Default | Description |
|-----------|---------|-------------|
| `prompt` | `"Describe what you see."` | Natural language question or instruction |
| `camera_index` | `-1` (use current) | Override camera for this call |

**Requires:** `ANTHROPIC_API_KEY` environment variable. Returns an error message if not set.

---

### `detect_led_state`

Detects LED states on the hardware board. Uses a two-stage strategy:

**Stage 1 — OpenCV HSV detection (always runs, no API key needed)**
- Converts frame to HSV color space
- Applies color masks for: red, green, blue, yellow, orange, white
- Filters blobs by area (8–4000 px²) to exclude noise and non-LED objects
- Computes per-LED confidence score from saturation and blob size
- Returns results immediately if overall confidence ≥ 60%

**Stage 2 — Claude vision fallback (runs if confidence < 60% AND API key is set)**
- Sends the same frame to `claude-opus-4-6` with a board inspection prompt
- Returns both the OpenCV result and the vision model interpretation

```
detect_led_state()                          # full frame, camera index from state
detect_led_state(region_hint="top-right")  # crop to top-right quadrant first
detect_led_state(camera_index=0)           # explicit camera index
```

**Parameters:**

| Parameter | Default | Description |
|-----------|---------|-------------|
| `camera_index` | `-1` (use current) | Camera index |
| `region_hint` | `""` | Crop hint: `"top-left"`, `"top-right"`, `"bottom-left"`, `"bottom-right"`, `"top"`, `"bottom"`, `"left"`, `"right"` |

**Example — good confidence:**
```
OpenCV detected 2 LED(s) [confidence: 78%]:
  1. GREEN — ON — middle-center (px:962,540  area:124px²  conf:82%)
  2. BLUE  — ON — top-right     (px:1480,210  area:88px²   conf:74%)
```

**Example — low confidence, fallback triggered:**
```
OpenCV detected 1 LED(s) [confidence: 42%]:
  1. RED — ON — bottom-left (px:230,820  area:15px²  conf:42%)
[Low confidence (42%) — Claude vision fallback:]
I can see two LEDs on the board. The green LED near the USB connector
is ON. There is also a dim red LED in the bottom-left corner that
appears to be blinking.
```

**Improving detection confidence:**
1. Ensure the board is well-lit (desk lamp angled toward the LEDs)
2. Use `set_ptz(zoom=2.0)` to enlarge the board in the frame
3. Use `adjust_image(contrast=1.3, saturation=1.3)` to boost LED visibility
4. Use `region_hint` to focus on the area where LEDs are located

---

## Workflows

### Workflow 1 — Initial board setup

Run once when first pointing the camera at the board:

```
list_cameras()                              # confirm MX Brio is index 0
capture_frame()                             # check framing
set_ptz(zoom=1.5, tilt=0.1)               # zoom in slightly, adjust framing
adjust_image(sharpness=3)                  # sharpen for LED detail
detect_led_state()                          # confirm LEDs are detected correctly
```

### Workflow 2 — Post-flash verification

After flashing firmware, confirm the board booted and LEDs show the expected state:

```
flash_firmware(project_path="...")
# wait 3 seconds
read_serial_log(port="/dev/cu.usbmodem1303", lines=30)
detect_led_state(region_hint="top-right")   # check status LEDs
```

Pass criteria:
- Serial log shows boot banner (`System Initialization`)
- Expected LEDs are ON (green = healthy, no red fault LED)

### Workflow 3 — Diagnose board hang (no serial output)

When the board is unresponsive:

```
analyze_frame(prompt="Is the board powered? Are any LEDs lit? Is anything unusual visible?")
halt_cpu()
read_registers()
read_call_stack()
```

Vision confirms physical state (is power LED on?) while JTAG provides the software state.

### Workflow 4 — Remote framing adjustment

If the board shifts position or you want to inspect a different area:

```
set_ptz(pan=0.2, tilt=-0.1, zoom=2.0)     # focus upper-right area
detect_led_state(region_hint="top-right")
set_ptz(pan=0.0, tilt=0.0, zoom=1.0)      # reset to full frame
```

---

## LED Detection Tips

### NUCLEO-WL55JC1 LED locations

| LED | Color | Location on board | Expected state |
|-----|-------|------------------|----------------|
| LD1 | Green | Top-right area | ON = MCU running |
| LD2 | Blue  | Top-right area | ON = LoRa TX active |
| LD3 | Red   | Top-right area | ON = fault / error |

Use `region_hint="top-right"` to improve detection confidence on this board.

### Camera positioning for best results

- Mount **directly above** the board at **20–40 cm** distance
- Use a **diffused light source** (avoid direct spotlight which creates glare on LEDs)
- Avoid **backlit setups** (window behind the board) — use `adjust_image(brightness=20)` if unavoidable

### When OpenCV confidence is consistently low

| Symptom | Fix |
|---------|-----|
| LEDs detected but wrong color | Check lighting — colored shadows or reflections cause hue shift |
| No LEDs detected, they are clearly on | Increase zoom (`set_ptz(zoom=2.0)`) and contrast (`adjust_image(contrast=1.4, saturation=1.4)`) |
| Too many false positives | Add `region_hint` to restrict the detection area |
| Detection flickers between calls | LEDs may be blinking — capture twice and compare |

---

## Safety Notes

- **Never** use `analyze_frame` in a tight loop — each call makes an API request and incurs cost and latency
- `capture_frame` is free (local only) — use it for framing/positioning checks
- `detect_led_state` is free when OpenCV confidence ≥ 60% — fallback to API only when needed
- Camera warm-up discards 3 frames per capture to let auto-exposure settle — each call takes ~0.5–1s

---

## Troubleshooting

### `ERROR: Cannot open camera index 0`

macOS camera permission not granted. Go to:
`System Settings → Privacy & Security → Camera`
Enable access for your terminal application, then relaunch Claude Code.

### `ERROR: ANTHROPIC_API_KEY not set`

`analyze_frame` and the `detect_led_state` fallback require the API key. Set it before launching Claude Code:

```bash
export ANTHROPIC_API_KEY=sk-ant-...
claude
```

Or add it to the `env` block in `.mcp.json` under `vision-mcp`.

### OpenCV always returns 0.0 for camera properties

This is expected on macOS — the AVFoundation backend does not expose UVC control properties via `cap.get()`. All image adjustments are applied post-capture via OpenCV transforms instead.

### `set_focus` returns "cannot lock device"

OpenCV may have the camera locked in a capture session. Try calling `set_focus` before any `capture_frame` call, or restart Claude Code so the MCP server reinitializes.

### Poor 4K / high-resolution captures

AVFoundation on macOS does not support the `3840×2160` preset. The maximum reliable resolution via OpenCV/AVFoundation is **1920×1080** regardless of camera sensor size. See [Camera Selection](#camera-selection) for why a 4K camera provides no advantage in this setup.

---

## Quick Reference

```
DISCOVER
  list_cameras()                              → available camera indices
  get_camera_info()                           → all current settings

CONFIGURE
  set_resolution(w, h)                        → switch capture resolution
  set_ptz(pan, tilt, zoom)                   → software ePTZ (persistent)
  adjust_image(br, co, sat, sh)              → image transforms (persistent)
  set_focus("auto" | "manual")               → AVFoundation focus mode

CAPTURE
  capture_frame()                             → raw JPEG, settings applied
  analyze_frame(prompt)                       → Claude vision Q&A
  detect_led_state(region_hint)              → OpenCV LED detection + fallback
```
