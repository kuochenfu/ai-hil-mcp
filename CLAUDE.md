# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**AI-HIL (AI-Hardware-in-the-Loop) Embedded Dev Automation**.

This repository is transitioning from architectural specification to implementation. The spec lives in `doc/AIHIL_embedded_dev_automation.md`. The goal is to give Claude Code the ability to **perceive, act on, and validate** physical embedded hardware via MCP servers.

## Architecture

The system has 5 planes connected through FastMCP:

- **Brain:** Claude Code CLI + CLAUDE.md hardware constraints
- **Nervous System:** FastMCP (Python) — the MCP layer bridging AI to hardware
- **Perception Plane:** Serial/SSH, JTAG/SWD, Webcam/OpenCV, PPK2 power profiler, SDR, Thermal/Mic
- **Action Plane:** Build/Flash/Erase firmware, hard reset/power cycle, GPIO/virtual sensor simulation
- **Context Plane:** Datasheets, schematics, golden sample measurements

### MCP Server Layout (to be implemented)

Each server encapsulates one hardware dimension, runs independently, and returns **semantic text** (not raw binary):

| Server | Port | Library | Purpose |
|--------|------|---------|---------|
| `serial-mcp` | :8001 | `pyserial` | UART log reading, anomaly detection |
| `jtag-mcp` | :8002 | `pyocd` | Call stack, register/memory read, HardFault diagnosis |
| `vision-mcp` | :8003 | `opencv-python` | LED state, LCD OCR, frame capture |
| `ppk2-mcp` | :8004 | `ppk2-api` | Current measurement, deep sleep verification |
| `build-flash-mcp` | :8005 | `subprocess` → `pio`/`west`/`cargo` | Firmware build, flash, erase |
| `power-control-mcp` | :8006 | `pyusb`/`gpiozero` | Hard reset, power cycle via USB relay |

SDR (`:8007`, `pyrtlsdr`) and Thermal/Mic (`:8008`, `pyaudio`+FLIR) are Phase 4.

## Development Commands

*To be filled in as implementation progresses (Phase 1 starts 2026-03-24).*

For each MCP server, the expected workflow will be:

```bash
# Install FastMCP and server dependencies
pip install fastmcp pyserial pyocd opencv-python ppk2-api

# Launch a server in dev mode (auto-reload)
fastmcp dev serial-mcp/server.py

# Run server standalone
python serial-mcp/server.py
```

## MCP Design Principles

1. **One server = one hardware dimension** — independent start/stop
2. **Tools return semantic text** — e.g., `"WARNING: HardFault detected: Stack overflow in task foo"`, not raw register hex
3. **Resources expose real-time state**; Tools execute active operations
4. **Error handling returns clear messages**, not Python tracebacks — the AI needs to understand the error

## Standard Diagnostic SOP (for embedded CLAUDE.md templates)

```
1. read_serial_log()          -- check for obvious errors first
2. read_call_stack()          -- if HardFault or hang suspected
3. measure_current(5000)      -- if power anomaly suspected
4. capture_frame()            -- if physical state unclear
5. build_firmware() -> flash_firmware() -> repeat step 1
```

## Closed-Loop Automation Flow

```
Triage (detect anomaly) → Diagnosis (JTAG + PPK2 + Vision in parallel)
→ Remediation (Claude fixes code) → Build & Flash → Verification
→ Record bug pattern in Known Bug Record → next Triage
```

## Target Hardware

- **ESP32-S3**: LoRa mote, Deep Sleep validation, RF verification, WiFi/BLE sensor nodes
- **STM32WL55JC**: Sub-GHz, ultra-low-power embedded targets
- **RPi CM4**: Edge gateway, dual-mode receiver
- **Zenoh-based mesh network gateway**

## Safety Constraints

- Never modify ISR handlers without reading call stack first
- Always `halt_cpu()` before flash operations
- Wait 2s after `power_cycle()` before serial reads
- Confirm PPK2 measurement range (uA vs mA mode) before measuring
- Watchdog timeout is typically 2s — feed periodically during long operations
