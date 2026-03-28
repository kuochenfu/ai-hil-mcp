# serial-mcp User Manual

**Version:** 2.0 · 2026-03-29

> Multi-board serial layer for AI-HIL — named board aliases, concurrent multi-port reads, anomaly detection, and pattern-based event waiting across up to N boards.

---

## Overview

`serial-mcp` gives Claude Code the ability to read and write to serial ports on physical embedded hardware. Version 2.0 adds a **device registry** (`devices.toml`) that maps human-readable board names to port paths, plus two new tools for multi-board workflows: `read_multi_log` and `wait_for_pattern`.

### Tool Summary

| Tool | Description |
|------|-------------|
| `list_serial_ports` | Enumerate all serial ports with USB vendor, product, and serial number |
| `list_boards` | Show all boards configured in `devices.toml` with their aliases |
| `read_serial_log` | Read up to N lines from a port; accepts alias or raw path; optional timestamps |
| `send_serial_command` | Send a command and collect the response |
| `read_multi_log` | Read from multiple ports **concurrently**, return interleaved timestamped output |
| `wait_for_pattern` | Monitor multiple ports and return on first pattern match |

---

## Board Configuration

### Option A — Manual configuration in `devices.toml` (recommended)

The device registry lives at the path pointed to by the `SERIAL_MCP_CONFIG` environment variable (already set in `.mcp.json`). Default location: `/Users/chenfu/Labs/ai-hil-mcp/devices.toml`.

**Step 1 — discover your port paths:**

```
list_serial_ports()
```

This returns every port with its USB manufacturer, product name, and hardware serial number. The serial number is stable across reboots and reconnects — use it to identify which physical board is which.

Example output:
```
/dev/cu.usbmodem11201 — USB — Espressif USB JTAG/serial debug unit [serial: 90:E5:B1:CB:CE:BC]
/dev/cu.usbserial-1110 — USB — Silicon Labs CP2102N USB to UART Bridge Controller [serial: 1ab9a031...]
/dev/cu.usbmodem11401 — USB — Espressif USB JTAG/serial debug unit [serial: 90:E5:B1:CB:CD:84]
/dev/cu.usbserial-1130 — USB — Silicon Labs CP2102N USB to UART Bridge Controller [serial: ccc65336...]
```

**Step 2 — edit `devices.toml`:**

```toml
[board.board1]
description = "Espressif board 1"
log_port    = "/dev/cu.usbmodem11201"   # primary console (USB JTAG/serial)
shell_port  = "/dev/cu.usbserial-1110"  # secondary UART (CP2102N bridge)
baud        = 115200

[board.board2]
description = "Espressif board 2"
log_port    = "/dev/cu.usbmodem11401"
shell_port  = "/dev/cu.usbserial-1130"
baud        = 115200
```

Fields:
| Field | Required | Default | Description |
|-------|----------|---------|-------------|
| `log_port` | yes | — | Primary console port — use alias `<name>/log` |
| `shell_port` | no | — | Secondary UART — use alias `<name>/shell` |
| `baud` | no | 115200 | Baud rate for both ports on this board |
| `description` | no | — | Free-text label shown in `list_boards` |

Changes to `devices.toml` take effect immediately — no rebuild or restart needed.

### Option B — Use raw port paths (no config needed)

Every tool also accepts a raw path like `/dev/cu.usbmodem11201` directly. This is fine for one-off commands but becomes unwieldy with multiple boards. Baud rate defaults to 115200; override with the `baud` parameter.

### Per-project configuration

The binary is shared across all projects — only the config path changes. Each project can have its own `devices.toml` describing the boards connected to that project.

**How the server resolves the config file (in order):**

1. `SERIAL_MCP_CONFIG` environment variable — set per-project in `.mcp.json`
2. Fallback — `devices.toml` in the same directory as the binary (acts as a global default)

To give a project its own board config, add `SERIAL_MCP_CONFIG` to that project's `.mcp.json`:

```json
"serial-mcp": {
  "type": "stdio",
  "command": "/Users/chenfu/Labs/ai-hil-mcp/serial-mcp-rs/target/release/serial-mcp-rs",
  "args": [],
  "env": {
    "SERIAL_MCP_CONFIG": "/Users/chenfu/Labs/<your-project>/devices.toml"
  }
}
```

When you open Claude Code in a different project directory, it reads that project's `.mcp.json` and loads the right `devices.toml` automatically. No rebuild or restart of the binary needed — just update the TOML.

The fallback (file next to binary) is useful as a personal default that applies to any project that doesn't set `SERIAL_MCP_CONFIG`.

### Port path stability on macOS

macOS assigns `/dev/cu.usbmodemXXXX` numbers based on enumeration order — they can change if you replug or reboot. To keep your `devices.toml` stable:

- Note the USB **serial number** from `list_serial_ports` for each board
- If a port path changes after a replug, run `list_serial_ports` again, find your board by serial number, and update `devices.toml`

---

## Tools

---

### `list_serial_ports`

Enumerate every serial port on the machine. Shows USB vendor, product, and hardware serial number.

**Parameters:** none

**Examples:**

1. Discover ports when setting up a new board:
```
list_serial_ports()
```

2. Find the hardware serial number to identify a specific board after replug:
```
list_serial_ports()
→ /dev/cu.usbmodem11401 — USB — Espressif USB JTAG/serial debug unit [serial: 90:E5:B1:CB:CD:84]
```

3. Verify a board is detected before flashing:
```
list_serial_ports()
→ confirm /dev/cu.usbmodem11201 is present before proceeding
```

---

### `list_boards`

Show all boards configured in `devices.toml` with their resolved port paths and aliases.

**Parameters:** none

**Examples:**

1. Check what boards are available after editing `devices.toml`:
```
list_boards()
```

2. Confirm aliases before a multi-board test session:
```
list_boards()
→ board1: log_port /dev/cu.usbmodem11201 → alias 'board1/log'
→ board2: log_port /dev/cu.usbmodem11401 → alias 'board2/log'
```

3. Use the output to orient yourself in an unfamiliar setup:
```
list_boards()
→ understand board layout before starting triage
```

---

### `read_serial_log`

Read up to N lines from a serial port within a time window. Accepts a raw port path or a board alias (`board1/log`, `board1/shell`). Automatically flags anomalies (watchdog, HardFault, panic, assert, stack overflow).

**Parameters:**
| Parameter | Required | Default | Description |
|-----------|----------|---------|-------------|
| `port` | yes | — | Raw path (`/dev/cu.usbmodem11201`) or alias (`board1/log`) |
| `baud` | no | 115200 | Baud rate (ignored when using an alias — config baud is used) |
| `lines` | no | 50 | Max lines to collect |
| `timeout_s` | no | 8 | Read window in seconds |
| `timestamps` | no | false | Prefix each line with `[HH:MM:SS.mmm]` wall-clock time |

**Examples:**

1. Quick health check on board1's primary console:
```
read_serial_log(port="board1/log", lines=20, timeout_s=5)
```

2. Triage after a reported crash — capture full boot with timestamps:
```
read_serial_log(port="board1/log", lines=100, timeout_s=15, timestamps=true)
→ [21:28:40.218]  E (1267523) task_wdt: Task watchdog got triggered ...
```

3. Read the secondary UART (shell/debug port) on board2 using a raw path:
```
read_serial_log(port="/dev/cu.usbserial-1130", baud=115200, lines=30)
```

---

### `send_serial_command`

Send a command string (terminated with `\r\n`) and collect the response. Accepts a raw port path or board alias.

**Parameters:**
| Parameter | Required | Default | Description |
|-----------|----------|---------|-------------|
| `port` | yes | — | Raw path or alias (`board1/shell`) |
| `command` | yes | — | Command to send |
| `baud` | no | 115200 | Baud rate |
| `response_lines` | no | 10 | Lines to collect after sending |

**Examples:**

1. Query firmware version on board1's shell port:
```
send_serial_command(port="board1/shell", command="version")
```

2. Trigger a specific test case via debug CLI:
```
send_serial_command(port="board2/shell", command="test run sensor_cal", response_lines=20)
```

3. Send a reset command using a raw port path:
```
send_serial_command(port="/dev/cu.usbserial-1110", command="reset", response_lines=5)
```

---

### `read_multi_log`

Read from **multiple ports concurrently** and return the output interleaved in timestamp order. Each line is prefixed with `[<board/port>  HH:MM:SS.mmm]`. Anomalies across any port trigger the `WARNING:` banner. This is the primary tool for observing inter-board communication (e.g. LoRa, Zenoh, BLE).

**Parameters:**
| Parameter | Required | Default | Description |
|-----------|----------|---------|-------------|
| `ports` | yes | — | Comma-separated list of raw paths or aliases |
| `lines` | no | 30 | Max lines per port |
| `timeout_s` | no | 8 | Read window in seconds (all ports read in parallel for this duration) |

**Examples:**

1. Watch both boards' primary consoles simultaneously during a protocol test:
```
read_multi_log(ports="board1/log,board2/log", lines=20, timeout_s=8)
→ [board1/log  21:29:15.218]  E (1302523) task_wdt: Task watchdog triggered
→ [board2/log  21:29:14.303]  I synapse_s3gw: Heartbeat published OK
```

2. Monitor all 4 ports at once — useful for initial bringup of a new board pair:
```
read_multi_log(ports="board1/log,board1/shell,board2/log,board2/shell", lines=10, timeout_s=6)
```

3. Correlate LoRa TX on one board with RX on the other — check timing gap:
```
read_multi_log(ports="board1/log,board2/log", lines=50, timeout_s=15)
→ compare timestamps on "Tx PING" and "Rx PING" lines to measure air latency
```

---

### `wait_for_pattern`

Monitor one or more ports and **return immediately** when any line matches the pattern. Uses `|` as a case-insensitive OR separator. Useful in automated test loops — run a test, then call `wait_for_pattern` to detect the first failure across any board without repeatedly polling.

**Parameters:**
| Parameter | Required | Default | Description |
|-----------|----------|---------|-------------|
| `ports` | yes | — | Comma-separated list of raw paths or aliases |
| `pattern` | yes | — | Substring pattern; `|` means OR (e.g. `"HardFault\|panic\|assert"`) |
| `timeout_s` | no | 30 | Give up after this many seconds |

**Examples:**

1. Wait for either board to fault during a stress test:
```
wait_for_pattern(ports="board1/log,board2/log", pattern="HardFault|panic|assert|watchdog", timeout_s=60)
→ MATCH on 'board1/log' at 21:28:40.218: E task_wdt: Task watchdog got triggered
```

2. Wait for a successful boot banner after flashing:
```
wait_for_pattern(ports="board1/log", pattern="System Initialization|app_main", timeout_s=15)
→ MATCH on 'board1/log' at 21:30:01.004: I app_main: System Initialization complete
```

3. Confirm both boards reach the ready state before starting a test (run twice, once per board):
```
wait_for_pattern(ports="board1/log", pattern="ready|initialized", timeout_s=20)
wait_for_pattern(ports="board2/log", pattern="ready|initialized", timeout_s=20)
```

---

## Alias Reference

Given the `devices.toml` from the setup section, the full alias table is:

| Alias | Resolves to | Board |
|-------|-------------|-------|
| `board1/log` | `/dev/cu.usbmodem11201` @ 115200 | Espressif board 1 — USB JTAG console |
| `board1/shell` | `/dev/cu.usbserial-1110` @ 115200 | Espressif board 1 — CP2102N UART |
| `board2/log` | `/dev/cu.usbmodem11401` @ 115200 | Espressif board 2 — USB JTAG console |
| `board2/shell` | `/dev/cu.usbserial-1130` @ 115200 | Espressif board 2 — CP2102N UART |

To add a third board, add a new `[board.board3]` section to `devices.toml`. No rebuild required.

---

## Anomaly Detection

All read tools automatically scan output for the following keywords (case-insensitive):

| Keyword | Typical cause |
|---------|--------------|
| `hardfault` / `hard fault` | CPU memory/bus/usage fault |
| `panic` | Firmware assertion or unhandled exception |
| `assert` | Failed assertion |
| `watchdog` | IWDG/task watchdog timeout |
| `stack overflow` | FreeRTOS task stack exhausted |

When a match is found, output is prepended with:
```
WARNING: ANOMALY DETECTED:
<matching lines>
---
<full log>
```

This allows Claude to immediately classify the failure type from a single tool call.
