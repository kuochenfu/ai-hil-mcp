# AI-HIL User Manual

**Version:** 2.0 · 2026-03-29

> Complete guide to using Claude Code as an AI-hardware-in-the-loop embedded systems engineer.

---

## What Claude Code Can Do With AI-HIL

Once the MCP servers are running, Claude Code can:

| Capability | MCP Server | Example |
|------------|------------|---------|
| Read UART serial logs and detect faults | `serial-mcp` | "HardFault detected in RX callback" |
| Monitor multiple boards simultaneously | `serial-mcp` | Interleaved timestamped output from all ports in one call |
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
| `list_serial_ports` | List all connected serial ports with USB serial numbers |
| `list_boards` | Show boards configured in `devices.toml` with their aliases |
| `read_serial_log(port, lines, timeout_s, timestamps)` | Read N lines with anomaly detection; accepts alias or raw path |
| `send_serial_command(port, command)` | Send a command and read response; accepts alias or raw path |
| `read_multi_log(ports, lines, timeout_s)` | Read multiple ports concurrently; interleaved timestamped output |
| `wait_for_pattern(ports, pattern, timeout_s)` | Monitor ports and return on first pattern match; supports `\|` OR |

Anomaly detection automatically flags: `HardFault`, `panic`, `assert`, `watchdog`, `stack overflow`.

**Board aliases:** configure `devices.toml` (path set via `SERIAL_MCP_CONFIG` in `.mcp.json`), then use `board1/log`, `board1/shell` etc. instead of raw port paths. See [user-manual-serial-mcp.md](user-manual-serial-mcp.md) for full setup and examples.

---

### `jtag-mcp` — CPU Debugging via JTAG/SWD

| Tool | Category | Description |
|------|----------|-------------|
| `list_probes` | Discovery | Enumerate all connected debug probes with serial numbers |
| `list_boards` | Discovery | Show boards configured in `devices.toml` with probe and target info |
| `halt_cpu(board)` | Control | Halt CPU, return PC; accepts optional board alias |
| `resume_cpu(board)` | Control | Resume execution; accepts optional board alias |
| `reset_target(board)` | Control | Soft reset, reads post-reset PC; reliable on all architectures |
| `read_registers(board)` | Perception | PC/SP/LR on all targets; full Cortex-M set on CM targets |
| `read_memory(address, length, board)` | Perception | Read bytes from any address |
| `read_call_stack(board)` | Perception | Cortex-M exception frame at SP (CM only) |
| `diagnose_hardfault(board)` | Perception | Decode HFSR/CFSR/BFAR/MMFAR (Cortex-M only) |
| `write_memory(address, data, board)` | Action | Write to any address (fault injection, live patches) |
| `set_breakpoint(address, board)` | Action | Hardware breakpoint via FPB |
| `clear_breakpoint(address, board)` | Action | Remove FPB breakpoint (Cortex-M only) |
| `set_watchpoint(address, kind, board)` | Action | DWT data watchpoint: `"read"`, `"write"`, `"read_write"` (CM only) |
| `clear_watchpoint(address, board)` | Action | Remove DWT watchpoint (Cortex-M only) |

**Board aliases:** configure `probe_serial` and `target` in `devices.toml` (path set via `JTAG_MCP_CONFIG` in `.mcp.json`), then pass `board="board1"` to any tool. Without a `board` param, connects to first available probe with default target.

**Architecture awareness:** Cortex-M-only tools (`diagnose_hardfault`, `read_call_stack`, DWT watchpoints, FPB `clear_breakpoint`) return a clear guidance message when called on non-Cortex-M targets (e.g. ESP32). Basic tools work on all architectures.

**Notes:**
- ESP32: `resume_cpu` may return Xtensa error after halt — use `reset_target` instead
- Watchdog timeout is typically 2s — do not halt CPU for more than 1.5s during live diagnosis
- DWT watchpoints persist across sessions (raw register writes); FPB breakpoints are cleared on session open

See [user-manual-jtag-mcp.md](user-manual-jtag-mcp.md) for full setup, multi-board SOP, and examples.

---

### `build-flash-mcp` — Firmware Build & Flash

| Tool | Description |
|------|-------------|
| `list_projects` | Show boards configured in `devices.toml` with project paths, presets, flash tools |
| `build_firmware(board)` | Run CMake + Ninja build; accepts optional `board` alias or explicit `project_path`+`preset` |
| `clean_build(board)` | Delete build directory; accepts optional `board` alias |
| `get_build_size(board)` | Report text/data/bss sizes; arch-aware (ARM/Xtensa/RISC-V); accepts optional `board` alias |
| `flash_firmware(board)` | Flash via auto-selected tool (openocd/esptool/idf/probe-rs); accepts optional `board` alias |

**Always:** call `halt_cpu` before flashing. Never flash if `build_firmware` returned errors. Wait 3s after flash before reading serial.

**Board aliases:** configure `project_path`, `preset`, `flash_tool`, `flash_port`, `flash_baud`, `openocd_cfg`, `target` in `devices.toml` (path set via `BUILD_FLASH_MCP_CONFIG` in `.mcp.json`), then pass `board="stm32"` to any tool. Without a `board` param, explicit `project_path` is required.

**Flash tools:**
- `openocd` — STM32/ARM targets via OpenOCD + ST-Link. OpenOCD scripts path detected dynamically via `brew --prefix open-ocd`.
- `esptool` — ESP32 via `esptool.py` using `flasher_args.json` generated by ESP-IDF CMake.
- `idf` — ESP32 via `idf.py flash` (handles bootloader, partition table, app automatically).
- `probe-rs` — any probe-rs supported target via `probe-rs download`.

**Architecture-aware size tool:**

| Target | Size binary |
|--------|-------------|
| STM32, nRF, ARM | `arm-none-eabi-size` |
| esp32s3, esp32s2 | `xtensa-esp32s3-elf-size` |
| esp32c3, RISC-V | `riscv32-esp-elf-size` |
| Fallback | system `size` |

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
  list_serial_ports()                  → all serial ports + USB serial numbers
  list_boards()                        → configured board aliases

OBSERVE
  read_serial_log(port, lines)         → UART log + anomaly flags (alias or raw path)
  read_multi_log(ports, lines)         → concurrent multi-board log, interleaved + timestamped
  wait_for_pattern(ports, pattern)     → block until pattern matches on any port
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
  list_projects()                      → configured boards with project paths + flash tools
  build_firmware(board)                → compile (alias or explicit project_path+preset)
  flash_firmware(board)                → program target (openocd/esptool/idf/probe-rs)
  get_build_size(board)                → text/data/bss (arch-aware: ARM/Xtensa/RISC-V)
  clean_build(board)                   → delete build dir

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
