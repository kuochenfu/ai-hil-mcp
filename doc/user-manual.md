# AI-HIL User Manual

**Version:** 1.7 · 2026-03-25

> Complete guide to using Claude Code as an AI-hardware-in-the-loop embedded systems engineer.

---

## What Claude Code Can Do With AI-HIL

Once the MCP servers are running, Claude Code can:

| Capability | MCP Server | Example |
|------------|------------|---------|
| Read UART serial logs and detect faults | `serial-mcp` | "HardFault detected in RX callback" |
| Halt CPU, read registers, diagnose crashes | `jtag-mcp` | "PRECISERR at 0x60000000 — invalid memory read" |
| Set hardware breakpoints and watchpoints | `jtag-mcp` | Halt CPU exactly when `uwTick` is written |
| Build and flash firmware | `build-flash-mcp` | CMake + OpenOCD, dual-core support |
| Measure current draw | `ppk2-mcp` | "Avg 11 mA, p99 22 mA — consistent with LoRa TX" |
| Profile power states | `ppk2-mcp` | "93% active, 7% TX burst — sleep state missing" |
| Estimate battery life | `ppk2-mcp` | "181 hours on 2000 mAh at measured draw" |
| Capture camera frame | `vision-mcp` | Frame with software PTZ and image adjustments applied |
| Detect LED states | `vision-mcp` | "GREEN ON (middle-center), BLUE ON (top-right)" |
| Visual board analysis | `vision-mcp` | "Board is powered, no fault LED visible, LCD shows PING" |

---

## Setup

### One-time setup

```bash
# Clone the repo once anywhere on your machine
git clone https://github.com/kuochenfu/ai-hil-mcp.git ~/ai-hil-mcp
```

### Per-project setup

```bash
cd ~/my-firmware-project
bash ~/ai-hil-mcp/setup.sh
```

This builds the Rust binaries and registers all MCP servers in `.mcp.json` for your project. Claude Code auto-connects when you open the project.

### Verify

```bash
claude mcp list
```

Expected:
```
serial-mcp      /path/to/serial-mcp-rs      ✓ Connected
build-flash-mcp /path/to/build-flash-mcp-rs ✓ Connected
jtag-mcp        /path/to/jtag-mcp-rs        ✓ Connected
ppk2-mcp        /path/to/ppk2-mcp-rs        ✓ Connected
```

---

## MCP Servers

### `serial-mcp` — UART Serial

| Tool | Description |
|------|-------------|
| `list_serial_ports` | List all connected serial ports |
| `read_serial_log(port, baud, lines, timeout_s)` | Read N lines with anomaly detection |
| `send_serial_command(port, baud, command)` | Send a command and read response |

Anomaly detection automatically flags: `HardFault`, `panic`, `assert`, `watchdog`, `stack overflow`.

**Defaults for STM32WL55:** `port="/dev/cu.usbmodem1303"`, `baud=115200`

---

### `jtag-mcp` — CPU Debugging via JTAG/SWD

| Tool | Category | Description |
|------|----------|-------------|
| `halt_cpu` | Control | Halt CPU, return PC |
| `resume_cpu` | Control | Resume execution |
| `reset_target` | Control | Soft reset, reads post-reset PC |
| `read_registers` | Perception | All core registers + xPSR + CONTROL/FAULTMASK |
| `read_memory(address, length)` | Perception | Read bytes from any address |
| `read_call_stack` | Perception | Exception frame at SP |
| `diagnose_hardfault` | Perception | Decode HFSR/CFSR/BFAR/MMFAR — semantic fault report |
| `write_memory(address, hex_bytes)` | Action | Write to any address (fault injection, live patches) |
| `set_breakpoint(address)` | Action | Hardware breakpoint via FPB |
| `clear_breakpoint(address)` | Action | Remove FPB breakpoint |
| `set_watchpoint(address, kind)` | Action | DWT data watchpoint: `"read"`, `"write"`, `"read_write"` |
| `clear_watchpoint(address)` | Action | Remove DWT watchpoint |

**Notes:**
- FPB breakpoints are cleared on each new JTAG session — set and use in the same conversation turn
- DWT watchpoints persist across sessions (raw register writes, not probe-rs managed)
- Watchdog timeout is 2s — do not halt CPU for more than 1.5s during live diagnosis

---

### `build-flash-mcp` — Firmware Build & Flash

| Tool | Description |
|------|-------------|
| `build_firmware(project_path, preset)` | Run CMake + Ninja build |
| `clean_build(project_path, preset)` | Delete build directory |
| `get_build_size(project_path)` | Report text/data/bss sizes for all ELFs |
| `flash_firmware(project_path)` | Flash all ELFs via OpenOCD |

**Always:** call `halt_cpu` before flashing. Never flash if `build_firmware` returned errors. Wait 3s after flash before reading serial.

**Target:** `project_path="/Users/chenfu/Labs/stm_projects/synapse-lora/CM4"`, `preset="Debug"`

---

### `ppk2-mcp` — Power Profiling

| Tool | Description |
|------|-------------|
| `find_ppk2` | Auto-detect correct PPK2 control port |
| `measure_current(port, mode, voltage_mv, duration_s)` | Full stats: min/max/avg/std-dev/percentiles/energy |
| `profile_power_states(...)` | Histogram across sleep/idle/active/TX current bands |
| `measure_with_pin_trigger(..., pin, trigger_level)` | Current stats filtered by logic input pin state |
| `estimate_battery_life(..., battery_capacity_mah)` | Runtime estimate from measured avg current |
| `set_dut_power(port, mode, voltage_mv, enabled)` | Manual DUT power on/off |
| `get_metadata(port)` | Read calibration, VDD, hardware version |

See [user-manual-ppk2.md](user-manual-ppk2.md) for full PPK2 setup and usage details.

---

### `vision-mcp` — Camera & Visual Inspection

| Tool | Description |
|------|-------------|
| `list_cameras` | Enumerate camera indices and resolutions |
| `get_camera_info` | Show all current settings (PTZ, adjustments, resolution) |
| `set_resolution(width, height)` | Switch capture resolution |
| `set_ptz(pan, tilt, zoom)` | Software ePTZ — crop-based, persistent across captures |
| `adjust_image(brightness, contrast, saturation, sharpness)` | Post-capture transforms — persistent |
| `set_focus(mode)` | `"auto"` or `"manual"` focus via AVFoundation |
| `capture_frame` | Raw JPEG with current PTZ and adjustments applied |
| `analyze_frame(prompt)` | Claude vision API analysis |
| `detect_led_state(region_hint)` | OpenCV LED detection (+ Claude vision fallback if confidence < 60%) |

**Defaults:** camera index 0, 1920×1080, no PTZ, no image adjustments.

**Notes:**
- All PTZ and image adjustment settings are persistent — set once, applied to all subsequent captures
- `detect_led_state` never requires an API key when LEDs are well-lit (OpenCV confidence ≥ 60%)
- `analyze_frame` requires `ANTHROPIC_API_KEY` in the environment
- macOS camera permission must be granted to the terminal app

See [user-manual-vision-mcp.md](user-manual-vision-mcp.md) for full setup, workflows, and troubleshooting.

---

## Standard Diagnostic SOP

When Claude Code encounters a hardware issue, execute this loop:

### Step 1 — Triage

```
read_serial_log(port="/dev/cu.usbmodem1303", baud=115200, lines=50, timeout_s=8)
```

| Log pattern | Go to |
|-------------|-------|
| `HardFault`, `hard fault` | Step 2A |
| `panic`, `assert` | Step 2B |
| `watchdog`, `IWDG` | Step 2C |
| `stack overflow` | Step 2A |
| No output | Step 2D |
| Clean — no anomaly | Stop ✓ |

### Step 2A — HardFault / Stack Overflow

Run in parallel:
```
diagnose_hardfault()
read_call_stack()
read_registers()
```

- `PRECISERR + BFARVALID` → bus fault; check BFAR address
- `IACCVIOL` → jumped to invalid address; check PC/LR
- `DIVBYZERO` → divide by zero; check PC
- `STKERR` → stack overflow; check SP vs stack bottom
- `FORCED` in HFSR → escalated fault; CFSR holds real cause

### Step 2B — Panic / Assert

```
read_serial_log(lines=100)
read_registers()
```

Extract file, line number, and condition from the panic message.

### Step 2C — Watchdog Reset

```
read_serial_log(lines=100)
read_registers()
```

Look for long-running loops, blocking waits, or missing `HAL_IWDG_Refresh()` calls.

### Step 2D — Board Hang (No Serial Output)

```
halt_cpu()
read_registers()
read_call_stack()
```

Check if CPU is spinning in a tight loop, blocking on semaphore, or in infinite fault loop.

### Step 3 — Remediation

1. Identify source file and function (from PC in exception frame)
2. Read the source file before making changes
3. Apply minimal fix — do not refactor unrelated code
4. If touching an ISR: re-read call stack first

### Step 4 — Build & Flash

```
build_firmware(project_path="...", preset="Debug")
```

If build fails: fix errors, do not flash.

```
flash_firmware(project_path="...")
```

Wait 3 seconds.

### Step 5 — Verification

```
read_serial_log(port="...", lines=30, timeout_s=10)
measure_current(port="...", duration_s=5)
```

Pass criteria:
- No anomaly keywords in serial output
- Boot banner present
- Current draw within expected range

If FAIL: return to Step 2 with new output as context.

---

## Power Profiling Workflow

### After every flash: power budget check

```
1. flash_firmware(...)
2. (wait 3s)
3. measure_current(port=..., voltage_mv=3300, duration_s=5)
4. profile_power_states(...)
```

Compare average current and state distribution to baseline. Any increase in average or unexpected time in high-current bands = regression.

### Deep sleep verification

For firmware with deep sleep / STOP modes:

```
profile_power_states(port=..., voltage_mv=3300, duration_s=10)
```

Expected (STOP mode with RTC):
```
< 1 µA    (deep sleep)    85%+
1–10 µA   (sleep)         0-5%
1–10 mA   (active)        10-15%  ← boot + radio
```

If `< 1 µA` bucket is empty, the MCU is not entering low-power mode.

---

## Safety Constraints

| Constraint | Why |
|------------|-----|
| Never modify ISR handlers without `read_call_stack` first | Interrupt context differs from thread context; stack layout changes |
| Always `halt_cpu` before flashing | Prevents flash corruption from concurrent execution |
| Never flash if `build_firmware` returned errors | Flashing a corrupt ELF bricks the target |
| Wait 3s after flash before serial read | Boot sequence takes ~1–2s; need margin |
| Watchdog = 2s — halt ≤ 1.5s max | IWDG resets the target if CPU is halted too long |
| PPK2 source voltage ≤ DUT rated VDD | STM32WL55: max 3.6V → use 3300 mV |
| JP1 must be removed (NUCLEO) | Without removal PPK2 fights onboard regulator |

---

## Known Hardware Configurations

### STM32WL55 (NUCLEO-WL55JC1)

| Item | Value |
|------|-------|
| Serial port | `/dev/cu.usbmodem1303` @ 115200 |
| JTAG target | `STM32WL55JCIx` |
| Firmware project | `/Users/chenfu/Labs/stm_projects/synapse-lora/CM4` |
| CMake preset | `Debug` |
| PPK2 port | `/dev/cu.usbmodemF4C7372644342` (use `find_ppk2`) |
| PPK2 voltage | `3300 mV` |
| JP1 | Remove before PPK2 source meter measurement |
| Expected active current | 8–15 mA |
| Expected TX current | 15–25 mA (SX126x PA) |
| Expected deep sleep | < 10 µA (RTC running) |

---

## Quick Reference Card

```
DISCOVER
  find_ppk2()                          → PPK2 control port
  list_serial_ports()                  → all serial ports

OBSERVE
  read_serial_log(port, baud, lines)   → UART log + anomaly flags
  read_registers()                     → CPU register snapshot
  read_memory(address, length)         → raw memory bytes
  read_call_stack()                    → exception frame
  diagnose_hardfault()                 → fault bits decoded

DEBUG
  halt_cpu()                           → stop CPU, get PC
  resume_cpu()                         → resume execution
  reset_target()                       → soft reset
  set_breakpoint(address)              → halt at address
  set_watchpoint(address, kind)        → halt on memory access
  write_memory(address, hex)           → live patch or fault inject

BUILD & FLASH
  build_firmware(path, preset)         → compile
  flash_firmware(path)                 → program target
  get_build_size(path)                 → text/data/bss

POWER
  measure_current(port, mv, secs)      → full stats + spike detect
  profile_power_states(port, mv, secs) → histogram by power mode
  measure_with_pin_trigger(port, pin)  → event-gated power
  estimate_battery_life(port, mah)     → runtime estimate

VISION
  list_cameras()                       → available camera indices
  get_camera_info()                    → all current settings
  set_resolution(w, h)                 → switch capture resolution
  set_ptz(pan, tilt, zoom)            → software ePTZ (persistent)
  adjust_image(br, co, sat, sh)       → image transforms (persistent)
  set_focus("auto"|"manual")          → AVFoundation focus mode
  capture_frame()                      → raw JPEG, settings applied
  analyze_frame(prompt)               → Claude vision Q&A
  detect_led_state(region_hint)       → OpenCV LED detection + fallback
```
