#!/usr/bin/env python3
"""
AI-HIL Fault Injector — injects a HardFault message into the serial port
to simulate a real firmware crash and trigger the orchestrator SOP.

Usage:
    python orchestrator/inject_fault.py
    python orchestrator/inject_fault.py --port /dev/cu.usbmodem1303 --fault hardfault
    python orchestrator/inject_fault.py --fault watchdog
    python orchestrator/inject_fault.py --fault panic

Available fault types:
    hardfault   PRECISERR bus fault (default)
    watchdog    Watchdog reset
    panic       Assert / panic
"""

import argparse
import time
import serial

FAULT_MESSAGES = {
    "hardfault": (
        "HardFault detected\r\n"
        "CFSR=0x00000200 HFSR=0x40000000\r\n"  # PRECISERR + FORCED
        "PC=0x08004ABC LR=0x080040CD SP=0x20003F40\r\n"
    ),
    "watchdog": (
        "IWDG watchdog reset triggered\r\n"
        "Last task: RadioTx — possible blocking loop\r\n"
    ),
    "panic": (
        "ASSERT failed: radio_buf != NULL\r\n"
        "file: subghz_phy_app.c line: 187\r\n"
    ),
}


def inject(port: str, baud: int, fault: str, delay_s: float) -> None:
    msg = FAULT_MESSAGES.get(fault)
    if not msg:
        print(f"Unknown fault type '{fault}'. Choose: {list(FAULT_MESSAGES)}")
        return

    print(f"[inject] Opening {port} @ {baud}...")
    with serial.Serial(port, baud, timeout=1) as ser:
        print(f"[inject] Waiting {delay_s}s before injecting '{fault}'...")
        time.sleep(delay_s)
        print(f"[inject] Injecting fault message:")
        for line in msg.strip().splitlines():
            print(f"[inject]   {line}")
        ser.write(msg.encode())
        ser.flush()
        print("[inject] Done. Watcher should now detect the anomaly.")


def main() -> None:
    parser = argparse.ArgumentParser(description="AI-HIL fault injector")
    parser.add_argument("--port", default="/dev/cu.usbmodem1303")
    parser.add_argument("--baud", type=int, default=115200)
    parser.add_argument("--fault", default="hardfault",
                        choices=list(FAULT_MESSAGES))
    parser.add_argument("--delay", type=float, default=2.0,
                        help="Seconds to wait before injecting (default: 2)")
    args = parser.parse_args()

    inject(args.port, args.baud, args.fault, args.delay)


if __name__ == "__main__":
    main()
