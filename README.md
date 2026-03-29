# AI-HIL Embedded Dev Automation

**Version:** v2.0 · 2026-03-29

> Give a single engineer the development, debugging, and verification capacity of a 3–5 person hardware team — through AI-assisted closed-loop automation.

---

## What is AI-HIL?

**AI-HIL (AI-Hardware-in-the-Loop)** upgrades Claude Code from a "code generator" to a "system-level engineer." By connecting Claude Code to physical hardware through the [Model Context Protocol (MCP)](https://modelcontextprotocol.io), the AI gains:

- **Perception** — reading serial logs, JTAG call stacks, power waveforms, and camera frames
- **Action** — building/flashing firmware, resetting hardware, controlling power
- **Closed-Loop Validation** — automatically verifying fixes and recording bug patterns

---

## System Architecture

```
┌─────────────────────────────────────────────────────┐
│                   Decision Brain                     │
│          Claude Code CLI + CLAUDE.md Rules           │
└───────────────────────┬─────────────────────────────┘
                        │ MCP
┌───────────────────────▼─────────────────────────────┐
│             Nervous System — FastMCP                 │
└──────┬──────────────────────────────────┬────────────┘
       │                                  │
┌──────▼──────────┐              ┌────────▼───────────┐
│ Perception Plane│              │   Action Plane     │
│  Serial / SSH   │              │  Build / Flash     │
│  JTAG / SWD     │              │  Hard Reset        │
│  Webcam / CV    │              │  GPIO / Sim        │
│  PPK2 Power     │              └────────────────────┘
│  SDR / RF       │
│  Thermal / Mic  │              ┌────────────────────┐
└─────────────────┘              │  Context Plane     │
                                 │  Datasheets / PDFs │
                                 │  Golden Samples    │
                                 └────────────────────┘
```

---

## MCP Servers

Servers are built with **FastMCP (Python)** or **Rust** (`rmcp` + `probe-rs`). Each server encapsulates one hardware dimension and returns **semantic text** (diagnostic conclusions, not raw binary data).

| Server | Port | Library | Purpose |
|--------|------|---------|---------|
| `serial-mcp` | stdio | `pyserial` (Python) · `serialport` (Rust) | Read UART logs, detect anomalies; named board aliases via `devices.toml`; concurrent multi-port reads; pattern waiting |
| `jtag-mcp` | stdio | `pyocd` (Python) · `probe-rs` (Rust) | Multi-board probe selection via `devices.toml`; halt/resume/reset; registers/memory; Cortex-M fault diagnosis; architecture-aware guards for non-CM targets |
| `vision-mcp` | stdio | `opencv-python` · `pytesseract` · `pyobjc-AVFoundation` · `anthropic` | Frame capture, software PTZ, image adjustment, LED detection, display OCR, jumper detection, board presence, motion/reset detection, QR code reading |
| `ppk2-mcp` | stdio | `ppk2` (Rust) | Current measurement, power state profiling, pin-triggered capture, battery life estimate |
| `build-flash-mcp` | stdio | `subprocess` (Python) · `std::process::Command` (Rust) | Firmware build/flash; `devices.toml` project registry; `board` param on all tools; flash tool abstraction (openocd/esptool/idf/probe-rs); arch-aware size tool |
| `power-control-mcp` | :8006 | `pyusb` / `gpiozero` | Hard reset, power cycle via USB relay |
| `sdr-mcp` *(Phase 4)* | :8007 | `pyrtlsdr` | RF spectrum scan, noise floor, emission detection |
| `thermal-mic-mcp` *(Phase 4)* | :8008 | `pyaudio` + FLIR SDK | Thermal imaging, coil whine detection |

### Design Principles

- **One server = one hardware dimension** — independently startable/stoppable
- **Tools return semantic text** — e.g., `"Stack overflow in task foo"`, not `0xE000ED28 = 0x0400`
- **Resources** expose real-time state; **Tools** execute active operations
- **Errors return clear messages**, not Python tracebacks
- **Python and Rust implementations are interchangeable** — same tool names and return format; swap by editing `.mcp.json`

---

## The Closed Loop

```
Triage          Diagnosis           Remediation
 (anomaly)  →  (JTAG + PPK2   →   (Claude Code
  detected       + Vision)          fixes code)
                                        │
    ┌───────────────────────────────────┘
    ▼
Verification ── PASS ──► Record bug in CLAUDE.md ──► next Triage
    │
   FAIL
    │
    └──────────────────────► Diagnosis (retry)
```

1. **Triage** — Perception plane detects anomaly (high current, log error)
2. **Diagnosis** — AI simultaneously checks JTAG stack, thermal image, power waveform
3. **Remediation** — Claude Code modifies C/Zig/Rust source
4. **Build & Flash** — Auto-compile and flash to target board
5. **Verification** — Serial log + PPK2 confirm fix; updates Known Bug Record in `CLAUDE.md`

---

## Quick Start

**1. Clone this repo once** (anywhere on your machine):

```bash
git clone https://github.com/kuochenfu/ai-hil-mcp.git ~/ai-hil-mcp
```

**2. In each new firmware project**, run setup to register the MCP servers:

```bash
cd ~/my-firmware-project
bash ~/ai-hil-mcp/setup.sh
```

This creates a `.mcp.json` in your project — Claude Code will auto-connect to the MCP servers when you open that project.

**3. Verify:**

```bash
claude mcp list
# serial-mcp: ... ✓ Connected
```

### Dev mode (hot-reload for server development)

```bash
cd ~/ai-hil-mcp
.venv/bin/fastmcp dev serial-mcp/server.py
```

---

## Standard Diagnostic SOP

When Claude Code encounters a hardware issue:

```
1. list_boards()                        # confirm board aliases are correct
2. read_multi_log(ports="b1/log,b2/log")# check all boards at once
3. read_call_stack()                    # if HardFault or hang suspected
4. measure_current(5000)                # if power anomaly suspected
5. capture_frame()                      # if physical state unclear
6. build_firmware()
   → flash_firmware()
   → wait_for_pattern(pattern="boot|ready")  # confirm clean boot
```

### Verification Flow (after every code change)

```
1. build_firmware()            # confirm build passes
2. flash_firmware()            # flash to target
3. read_serial_log(lines=30)   # confirm clean boot
4. verify_deep_sleep(...)      # validate power (if applicable)
5. detect_led_state(...)       # confirm LED state (if applicable)
```

### Safety Constraints

- Never modify ISR handlers without reading call stack first
- Always `halt_cpu()` before flash operations
- Wait 2s after `power_cycle()` before serial reads
- Confirm PPK2 measurement range (uA vs mA) before measuring
- Watchdog timeout is typically 2s — feed periodically during long operations

---

## Hardware BOM

### Phase 1–2 (MVP) — $253–345 USD

| Device | Spec | Est. Price | Priority |
|--------|------|-----------|----------|
| USB-to-Serial | FTDI FT232RL | $8–15 | P0 |
| Debugger | ST-Link V3 SET or J-Link EDU Mini | $30–60 | P0 |
| Target Board | NUCLEO-WL55JC1 (STM32WL) | $35–45 | P0 |
| Target Board | ESP32-S3-DevKitC-1 | $10–15 | P1 |
| Power Profiler | Nordic PPK2 | $90–100 | P0 |
| Webcam | Logitech C920 / C922 (recommended) | $50–70 | P1 |
| USB Hub | Powered 7-Port USB 3.0 | $20–30 | P0 |

### Phase 3–4 (Advanced) — $910–1,020 USD (incl. workstation)

| Device | Spec | Est. Price | Priority |
|--------|------|-----------|----------|
| USB Relay | 2-Channel USB Relay Module | $15–25 | P1 |
| Programmable PSU | Riden RD6006 (60V/6A) | $50–80 | P2 |
| SDR | RTL-SDR V4 | $30–35 | P2 |
| Thermal Camera | FLIR Lepton 3.5 Module | $200–250 | P2 |
| USB Microphone | Any USB recording mic | $15–30 | P2 |
| HIL Workstation | Mac Mini M4 or Linux PC | $600+ | P1 |

**Recommended Suppliers:** Mouser/Digi-Key (STM32, ST-Link), Nordic Semiconductor (PPK2), RTL-SDR Blog (RTL-SDR V4), Amazon/PChome (webcam, relay, hub)

---

## Implementation Roadmap

> **Testing policy:** Tasks marked ❌ require physical hardware and are deferred until the equipment is available. Tasks marked ✅ or ⚠️ are implemented first.

### Phase 1 — Foundation (Weeks 1–2)
Goal: Claude Code reads serial log and flashes firmware

| # | Task | Testable | Acceptance Criteria |
|---|------|----------|---------------------|
| 1.1 | Create project, install FastMCP | ✅ | **Done** |
| 1.2 | Serial MCP Server + anomaly detection | ✅ | **Done** |
| 1.4 | Build & Flash MCP — build + flash | ✅ | **Done** — tested on STM32WL55 dual-core |
| 1.5 | Write initial `CLAUDE.md` | ✅ | **Done** |
| 1.6 | E2E verification | ✅ | **Done** — Build → Flash → Serial log captured (boot banner + radio events) |

### Phase 2 — Perception Expansion (Weeks 3–5)
Goal: AI "sees" hardware faults via JTAG + Power + Vision

| # | Task | Testable | Acceptance Criteria |
|---|------|----------|---------------------|
| 2.1 | JTAG/SWD MCP Server (pyocd) | ✅ | **Done** — registers, memory, call stack |
| 2.2 | HardFault semantic parser | ✅ | **Done** — fault injection test passed (PRECISERR @ 0x60000000) |
| 2.2b | JTAG MCP Server — Rust rewrite (`probe-rs` + `rmcp`) | ✅ | **Done** — all 12 tools hardware-verified on STM32WL55. Active debugging confirmed: DWT watchpoint halted CPU in `HAL_IncTick` writing `uwTick`; FPB breakpoint halted CPU at exact target address. `clear_breakpoint` uses raw FPB scan (probe-rs clears FPB on session open). v2.0: multi-board probe selection via `devices.toml`, optional `board` param on all tools, architecture-aware guards for non-Cortex-M targets — verified on 2× ESP32-S3 boards |
| 2.2c | Serial MCP Server — Rust rewrite (`serialport` + `rmcp`) | ✅ | **Done** — hardware-verified on STM32WL55 (LoRa PING traffic captured). v2.0: device registry (`devices.toml`), board aliases, `read_multi_log`, `wait_for_pattern`, timestamps, USB serial numbers — verified on 2× Espressif boards (4 ports) |
| 2.2d | Build & Flash MCP Server — Rust rewrite (`std::process::Command` + `rmcp`) | ✅ | **Done** — hardware-verified: build → flash → serial confirmed on STM32WL55. v2.0: `devices.toml` project registry, `board` param on all 5 tools (`list_projects` added), flash tool abstraction (openocd/esptool/idf/probe-rs), arch-aware size tool, ESP-IDF support (`build_tool="idf"`, `idf_path`). Binary verified; hardware test on ESP32-S3 boards pending. |
| 2.3 | PPK2 MCP Server (`ppk2-mcp-rs`) | ✅ | **Done** — 7 tools hardware-verified on STM32WL55: `measure_current`, `profile_power_states`, `measure_with_pin_trigger`, `estimate_battery_life`, `set_dut_power`, `find_ppk2`, `get_metadata`. Dual-port macOS issue resolved. |
| 2.4 | Vision MCP Server | ✅ | **Done** — 8 tools: `list_cameras`, `get_camera_info`, `set_resolution`, `set_ptz`, `adjust_image`, `set_focus`, `capture_frame`, `analyze_frame`, `detect_led_state`. Software ePTZ + image adjustments via OpenCV; LED detection OpenCV-first with Claude vision fallback. Verified on Logitech MX Brio Ultra 4K. |
| 2.5 | Multi-sense diagnosis test | ❌ Full hardware | Inject memory overflow bug, AI locates root cause |

### Phase 3 — Closed-Loop Automation (Weeks 6–8)
Goal: AI autonomously completes Triage → Diagnosis → Remediation → Verification

| # | Task | Testable | Acceptance Criteria |
|---|------|----------|---------------------|
| 3.1 | AI-HIL Orchestrator script | ⚠️ Partial | Auto-triggers full diagnosis cycle |
| 3.2 | Power Control MCP Server | ❌ USB Relay | `hard_reset()` reboots target board |
| 3.3 | Automated closed-loop verification | ❌ Full hardware | edit → Build → Flash → Reset → check → PASS/FAIL |
| 3.4 | CLAUDE.md auto-update | ✅ | Bug pattern appended to Known Bug Record after each fix |
| 3.5 | Regression test suite | ❌ Full hardware | Known bug → reproduce → auto-fix → verify PASS |

### Phase 4 — Advanced Perception + CI/CD (Weeks 9–12)
Goal: Expand sensing + integrate into continuous integration

| # | Task | Testable | Acceptance Criteria |
|---|------|----------|---------------------|
| 4.1 | SDR MCP Server | ❌ RTL-SDR V4 | Detect LoRa/Sub-GHz emission, return spectrum summary |
| 4.2 | Thermal/Mic MCP Server | ❌ FLIR + mic | Detect overheating and coil whine |
| 4.3 | CI/CD pipeline | ⚠️ Partial | GitHub Actions → SSH to HIL workstation → auto Build/Flash/Test |
| 4.4 | Multi-board support | ✅ | **Done** — `devices.toml` registry + aliases; `read_multi_log` concurrent N-board reads; `wait_for_pattern` cross-board event detection; verified on 2× Espressif boards |

---

## Success Metrics

| Metric | Traditional | AI-HIL Target |
|--------|------------|---------------|
| Bug fix cycle time | 2–8 hours | **< 15 minutes** |
| Flash-to-verify time | 5–10 min (manual) | **< 60 seconds** |
| HardFault diagnosis | 20–60 min | **< 30 seconds** |
| Power regression detection | Pre-shipment only | **Automatic after every flash** |
| Closed-loop fix success rate | — | **> 50%** |
| Known bug regression rate | — | **0%** |

---

## Example Target Hardware

| Target | AI-HIL Application |
|--------|-------------------|
| ESP32-S3 | LoRa communication validation, Deep Sleep optimization, RF TX confirmation, WiFi/BLE stability |
| STM32WL55JC | Sub-GHz spectrum validation, ultra-low-power verification |
| RPi CM4 | Edge gateway, dual-mode reception validation, Rule Engine testing |
| Zenoh gateway | Router stress testing, offline autonomy validation |

---

## Dev Logs

Progress is tracked in [`doc/`](doc/) with daily logs.

| Date | Milestone |
|------|-----------|
| [2026-03-19](doc/2026-03-19.md) | Phase 1 + 2.1/2.2 complete — Serial, Build & Flash, JTAG MCPs hardware-tested on STM32WL55 |
| [2026-03-21](doc/2026-03-21.md) | Phase 2.2b/c/d — All 3 MCP servers ported to Rust and hardware-verified on STM32WL55 |
| [2026-03-22](doc/2026-03-22.md) | `jtag-mcp-rs` expanded to full active debugger — all 12 tools hardware-verified; DWT watchpoint halt confirmed in SysTick ISR; FPB cross-session limitation documented |
| [2026-03-23](doc/2026-03-23.md) | Phase 2.3 complete — `ppk2-mcp-rs` implemented and all 7 tools fully hardware-verified; dual-port macOS issue resolved; active-low button confirmed on pin 0; battery estimate: 6.3 days on 2000 mAh @ 13 mA avg |
| [2026-03-25](doc/2026-03-25.md) | Phase 2.4 complete — `vision-mcp` implemented with 8 tools; software ePTZ, image adjustments, LED detection (OpenCV-first + Claude vision fallback); verified on Logitech MX Brio Ultra 4K |
| [2026-03-29](doc/2026-03-29.md) | `serial-mcp` v2.0 — multi-board device registry, named aliases, `read_multi_log`, `wait_for_pattern`, timestamps; `jtag-mcp` v2.0 — probe selection by serial, `board` param on all tools, architecture guards for ESP32; both verified on 2× Espressif boards; `build-flash-mcp` v2.0 — project registry, flash tool abstraction (openocd/esptool/idf/probe-rs), arch-aware size tool, ESP-IDF support |

---

## Documentation

| Document | Description |
|----------|-------------|
| [`doc/user-manual.md`](doc/user-manual.md) | Complete user manual — all MCP servers, SOPs, safety constraints, quick reference |
| [`doc/user-manual-ppk2.md`](doc/user-manual-ppk2.md) | PPK2 power profiling deep-dive — hardware setup, all tools, workflows, troubleshooting |
| [`doc/user-manual-vision-mcp.md`](doc/user-manual-vision-mcp.md) | vision-mcp deep-dive — camera setup, PTZ, image adjustments, LED detection, workflows |
| [`doc/user-manual-serial-mcp.md`](doc/user-manual-serial-mcp.md) | serial-mcp deep-dive — board config, aliases, multi-port reads, pattern waiting, all tools with examples |
| [`doc/user-manual-jtag-mcp.md`](doc/user-manual-jtag-mcp.md) | jtag-mcp deep-dive — multi-board probe selection, target config, architecture awareness, all 14 tools with examples |
| [`doc/AIHIL_embedded_dev_automation.md`](doc/AIHIL_embedded_dev_automation.md) | Full architectural specification with diagrams and design rationale |

---

*AI-HIL — Giving hardware the soul of AI, realizing automated closed-loop development in the physical world.*
