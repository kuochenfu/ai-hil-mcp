# PPK2 MCP Server — User Manual

**Server:** `ppk2-mcp` · **Binary:** `ppk2-mcp-rs` · **Version:** 1.0 (2026-03-23)

> Measure, profile, and analyze the power consumption of embedded devices through Claude Code — no GUI required.

---

## Hardware Setup

### PPK2 Modes

| Mode | When to use | PPK2 connection |
|------|-------------|-----------------|
| **Source meter** | PPK2 is the sole power supply to your DUT | PPK2 V+ → DUT VDD, PPK2 GND → DUT GND |
| **Ampere meter** | DUT has its own power supply (e.g. battery, USB) | Wire DUT power line through PPK2 VIN/GND |

### NUCLEO-WL55JC1 Setup (source meter)

1. Remove **jumper JP1** — this isolates the MCU VDD from the ST-Link 3.3V rail
2. Connect PPK2 **V+** → NUCLEO **JP1 pin 1** (MCU VDD side)
3. Connect PPK2 **GND** → NUCLEO **GND**
4. Leave ST-Link USB connected for flashing/serial — it powers the ST-Link but not the MCU

> **Warning**: Never set source meter voltage above DUT rated VDD. STM32WL55 max is 3.6V — use 3300 mV.

---

## Quick Start

### Step 1 — Find your PPK2 port

```
find_ppk2()
```

Expected output:
```
PPK2 found. Control port: /dev/cu.usbmodemF4C7372644342 (data port: /dev/cu.usbmodemF4C7372644344)
Use: /dev/cu.usbmodemF4C7372644342
```

> Always use the **control port** (lower-numbered). The PPK2 exposes two CDC-ACM ports on macOS — `find_ppk2` picks the correct one automatically.

### Step 2 — Measure current

```
measure_current(
  port="/dev/cu.usbmodemF4C7372644342",
  voltage_mv=3300,
  duration_s=3
)
```

Example output:
```
Measurement complete (3.0s, 2996 samples)
Mode:       source_meter
Min:        -1.50 µA
Max:        22.28 mA
Avg:        11.01 mA
Std dev:    3.42 mA
p50:        10.80 mA
p95:        18.20 mA
p99:        21.50 mA
Peak/Avg:   2.0x
Voltage:    3300 mV
Energy:     108953.98 µJ  (0.0303 mWh)
```

---

## Prompt Examples

Just describe what you want in plain English — Claude Code will call the right tools automatically.

---

**Example 1 — Quick power check after flashing**

> "Measure the current on the STM32 for 5 seconds and tell me if it looks healthy."

Claude will run `find_ppk2` → `measure_current(duration_s=5)` and interpret the result:
```
Measurement complete (5.0s, 4995 samples)
Avg: 12.75 mA  p95: 18.77 mA  Peak/Avg: 1.7x
→ Consistent with active LoRa PING/PONG loop. No spike detected.
```

---

**Example 2 — Power state breakdown**

> "Profile the power states of the board — I want to know how much time it spends in sleep vs TX."

Claude will run `profile_power_states(duration_s=10)` and highlight the dominant state:
```
Power State Profile (10.0s)
< 1 µA    (deep sleep)   4.2%
1–10 mA   (active)       7.1%
> 10 mA   (TX / peak)   88.7%  ← dominant
→ Board is mostly transmitting. Sleep state is nearly absent — check low-power mode config.
```

---

**Example 3 — Battery life estimate before shipping**

> "How long will this device last on a 2000 mAh battery?"

Claude will run `estimate_battery_life(battery_capacity_mah=2000, duration_s=10)` and summarize:
```
Avg current: 13.21 mA @ 3300 mV → 43.6 mW average power
Estimated runtime: 151 hours (6.3 days)
→ Below the 10-day target. Consider enabling deep sleep between LoRa cycles.
```

---

## Tools Reference

### `find_ppk2`

No parameters required.

Scans USB for Nordic PPK2 (VID=0x1915, PID=0xC00A), sorts all matches, and returns the lowest port name (control interface). Reports both ports when two are found.

---

### `measure_current`

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `port` | string | required | From `find_ppk2` |
| `mode` | string | `source_meter` | `source_meter` or `ampere_meter` |
| `voltage_mv` | integer | `3300` | Supply voltage in mV (800–5000). Source meter only. |
| `duration_s` | float | `3.0` | How long to measure, in seconds |

**What it does:** enables DUT power → measures for `duration_s` → disables DUT power (resets PPK2).

**Output fields:**
- `Min / Max / Avg` — current extremes and mean
- `Std dev` — spread; high std dev = variable workload
- `p50 / p95 / p99` — percentiles; p99 ≈ worst-case burst current
- `Energy` — total energy consumed (µJ and mWh)
- `Peak/Avg ratio` — > 10× triggers spike warning
- `WARNING: CURRENT SPIKE DETECTED` — prepended if peak > 10× average

**Negative current readings** (e.g. -1.5 µA) are noise floor — normal when measuring µA-level sleep currents on a mA range.

---

### `profile_power_states`

Same parameters as `measure_current`.

Bins all samples into log-scale current bands and identifies the dominant operating state.

```
Power State Profile (3.0s, 2996 samples)
──────────────────────────────────────────────────────────
Range                        Samples   Time%      Avg (µA)
──────────────────────────────────────────────────────────
< 1 µA    (deep sleep)             0    0.0%             —
1–10 µA   (sleep)                  0    0.0%             —
10–100 µA (idle)                   0    0.0%             —
100µA–1mA (light load)             0    0.0%             —
1–10 mA   (active)              2801   93.5%      9823.12 µA  ← dominant
> 10 mA   (TX / peak)            195    6.5%      17341.00 µA
──────────────────────────────────────────────────────────
Overall avg: 10.11 mA  |  std dev: 2.78 mA
Dominant state: 1–10 mA   (active) (93.5%)
Est. active current: 17.34 mA
```

**Reading the histogram:**
- A device in deep sleep should show > 90% in `< 1 µA`
- A LoRa device in PING/PONG should show dominant `1–10 mA` with bursts in `> 10 mA`
- Unexpected time in a high band after a code change = power regression

---

### `measure_with_pin_trigger`

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `port` | string | required | From `find_ppk2` |
| `mode` | string | `source_meter` | `source_meter` or `ampere_meter` |
| `voltage_mv` | integer | `3300` | Supply voltage in mV |
| `duration_s` | float | `3.0` | Total measurement window |
| `pin` | integer | required | Logic input pin 0–7 |
| `trigger_level` | string | required | `"high"` or `"low"` |

Connect a GPIO from your DUT to the PPK2 logic input header. The tool captures current **only when the pin matches** the trigger level.

**Use cases:**
- Measure current during LoRa TX: toggle a GPIO HIGH at TX start, LOW at TX end → `pin=0, trigger_level="high"`
- Measure sleep current: trigger LOW on the active indicator GPIO
- Profile DMA burst power: trigger on DMA active signal

> **NUCLEO-WL55JC1 note**: The on-board user button (B1) is **active-low** — it pulls the logic pin LOW when pressed. Use `trigger_level="low"` when testing with this button. Using `trigger_level="high"` will return no matched samples.

**Example output (NUCLEO button held, pin 0, active-low):**
```
Pin-triggered Measurement (pin 0 = low, 10.0s)
Matched samples:  9995 / 9995 (100.0% of time pin was low)
────────────────────────────────────────
Min:     -1.48 µA
Max:     51.47 mA
Avg:     18.31 mA
Std dev: 4.47 mA
p50:     18.50 mA  p95: 23.19 mA  p99: 23.24 mA
```

---

### `estimate_battery_life`

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `port` | string | required | From `find_ppk2` |
| `mode` | string | `source_meter` | `source_meter` or `ampere_meter` |
| `voltage_mv` | integer | `3300` | Supply voltage in mV |
| `duration_s` | float | `5.0` | Measurement window (longer = more accurate) |
| `battery_capacity_mah` | float | required | Battery capacity, e.g. `2000` for 2000 mAh |

**Example output:**
```
Battery Life Estimate
──────────────────────────────────────────────────
Measured avg current: 11.01 mA  (p95: 18.20 mA)
Supply voltage:       3300 mV
Average power:        36.33 mW
Battery capacity:     2000 mAh  (6600.0 mWh)
──────────────────────────────────────────────────
Estimated runtime:    181.7 hours  (7.6 days)
Energy per second:    36.33 µJ
──────────────────────────────────────────────────
```

> This estimate assumes the measured duty cycle is representative of real-world usage. Use a longer `duration_s` (10–30s) to capture multiple TX/sleep cycles for accuracy.

---

### `set_dut_power`

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `port` | string | required | From `find_ppk2` |
| `mode` | string | `source_meter` | `source_meter` or `ampere_meter` |
| `voltage_mv` | integer | `3300` | Supply voltage in mV |
| `enabled` | boolean | required | `true` = power on, `false` = power off |

Power the DUT on or off without running a measurement. Use for:
- Power cycling the DUT between test cases
- Holding DUT in powered-off state while flashing (though `build-flash-mcp` handles this automatically)
- Manual control during debugging

---

### `get_metadata`

| Parameter | Type | Description |
|-----------|------|-------------|
| `port` | string | From `find_ppk2` |

Returns PPK2 device info:
```
PPK2 Metadata
Calibrated: true
VDD:        3300 mV
HW version: 9173
Mode:       Source
IA:         56
```

---

## Common Workflows

### Verify power budget after flash

```
1. flash_firmware(...)           # build-flash-mcp
2. (wait 3s)
3. measure_current(duration_s=5) # baseline current check
4. profile_power_states(...)     # confirm sleep/active distribution
```

### Detect power regression

Run `measure_current` before and after a code change. Compare:
- `Avg` — any increase in average current
- `p99` — any increase in peak current
- `profile_power_states` — any shift from lower to higher current bands

### Measure LoRa TX current

Wire GPIO to PPK2 logic pin 0. Toggle high during TX in firmware.

```
measure_with_pin_trigger(
  port="...",
  pin=0,
  trigger_level="high",
  duration_s=10
)
```

### Estimate battery life for a coin cell

```
estimate_battery_life(
  port="...",
  voltage_mv=3000,        # CR2032 nominal
  duration_s=30,          # capture multiple sleep/TX cycles
  battery_capacity_mah=220
)
```

---

## Troubleshooting

| Error | Cause | Fix |
|-------|-------|-----|
| `PPK2 not found` | USB not connected or driver issue | Check USB cable, try unplugging/replugging |
| `Parse error in "Metadata"` | Wrong port or device not ready | Always use `find_ppk2` to get the correct port |
| All readings ~0 µA | DUT not connected or JP1 not removed | Check wiring; confirm JP1 is removed on NUCLEO |
| Negative current (< -5 µA) | DUT powered by another source in parallel | Remove all other power sources; check JP1 |
| `No samples collected` | `duration_s` too short or connection lost | Increase `duration_s`, recheck port |

---

## Safety Limits (STM32WL55)

| Limit | Value |
|-------|-------|
| Max VDD | 3.6 V → use `voltage_mv=3300` |
| Max current (source meter) | 600 mA — well above any STM32WL55 operating point |
| Expected active current | 8–15 mA (CPU + LoRa RX) |
| Expected TX current | 15–25 mA (SX126x PA) |
| Expected deep sleep | < 10 µA (with RTC running) |
