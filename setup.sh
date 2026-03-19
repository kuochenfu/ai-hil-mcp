#!/usr/bin/env bash
set -e

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "==> Setting up AI-HIL MCP environment..."

# Create venv if it doesn't exist
if [ ! -d "$REPO_DIR/.venv" ]; then
  echo "==> Creating Python virtual environment..."
  python3 -m venv "$REPO_DIR/.venv"
fi

# Install dependencies
echo "==> Installing dependencies..."
"$REPO_DIR/.venv/bin/pip" install --quiet --upgrade pip
"$REPO_DIR/.venv/bin/pip" install --quiet fastmcp pyserial pyocd

echo "==> Installing pyocd device pack for STM32WL55..."
"$REPO_DIR/.venv/bin/pyocd" pack install stm32wl55jcix --no-check-for-updates 2>/dev/null || true

echo "==> Registering MCP servers in your current project..."

claude mcp add serial-mcp -s project -- \
  "$REPO_DIR/.venv/bin/python" "$REPO_DIR/serial-mcp/server.py"

claude mcp add build-flash-mcp -s project -- \
  "$REPO_DIR/.venv/bin/python" "$REPO_DIR/build-flash-mcp/server.py"

claude mcp add jtag-mcp -s project -- \
  "$REPO_DIR/.venv/bin/python" "$REPO_DIR/jtag-mcp/server.py"

echo ""
echo "Done! MCP servers registered:"
claude mcp list | grep -E "serial-mcp|build-flash-mcp|jtag-mcp"
echo ""
echo "To verify, run: claude mcp list"
