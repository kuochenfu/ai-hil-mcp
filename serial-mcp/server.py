from fastmcp import FastMCP
import serial
import serial.tools.list_ports

mcp = FastMCP("serial-mcp")

ANOMALY_KEYWORDS = ["panic", "hardfault", "assert", "watchdog", "stack overflow", "hard fault"]


def _flag_anomalies(lines: list[str]) -> str:
    anomalies = [l for l in lines if any(k in l.lower() for k in ANOMALY_KEYWORDS)]
    body = "\n".join(lines)
    if anomalies:
        warning = "WARNING: ANOMALY DETECTED:\n" + "\n".join(anomalies) + "\n---\n"
        return warning + body
    return body


@mcp.tool()
def list_serial_ports() -> str:
    """List all available serial ports on this machine."""
    ports = serial.tools.list_ports.comports()
    if not ports:
        return "No serial ports found."
    return "\n".join(f"{p.device} — {p.description}" for p in ports)


@mcp.tool()
def read_serial_log(port: str, baud: int = 115200, lines: int = 50, timeout_s: int = 8) -> str:
    """
    Read up to N lines from a serial port within a time window. Auto-flags anomalies
    like HardFault, Panic, Watchdog Reset. Reads for up to timeout_s seconds so it
    catches output that arrives in bursts (e.g. after boot or a radio event).

    Args:
        port: Serial port path (e.g. /dev/tty.usbmodem1303).
        baud: Baud rate. Default: 115200.
        lines: Maximum number of lines to collect. Default: 50.
        timeout_s: Total read window in seconds. Default: 8.
    """
    import time
    try:
        ser = serial.Serial(port, baud, timeout=1)
        buffer = []
        deadline = time.time() + timeout_s
        while time.time() < deadline and len(buffer) < lines:
            line = ser.readline().decode(errors="replace").strip()
            if line:
                buffer.append(line)
        ser.close()
        if not buffer:
            return "No data received. Check that the device is running and baud rate is correct."
        return _flag_anomalies(buffer)
    except serial.SerialException as e:
        return f"ERROR: Could not open {port}: {e}"


@mcp.tool()
def send_serial_command(port: str, command: str, baud: int = 115200, response_lines: int = 10) -> str:
    """Send a command over serial and return the response."""
    try:
        ser = serial.Serial(port, baud, timeout=2)
        ser.write((command + "\r\n").encode())
        buffer = []
        for _ in range(response_lines):
            line = ser.readline().decode(errors="replace").strip()
            if line:
                buffer.append(line)
        ser.close()
        if not buffer:
            return "Command sent. No response received."
        return _flag_anomalies(buffer)
    except serial.SerialException as e:
        return f"ERROR: Could not open {port}: {e}"


if __name__ == "__main__":
    mcp.run()
