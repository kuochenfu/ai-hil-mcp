from fastmcp import FastMCP
import subprocess
import shutil
import os
import glob

mcp = FastMCP("build-flash-mcp")


def _run(cmd: list[str], cwd: str | None = None, timeout: int = 120) -> tuple[int, str, str]:
    result = subprocess.run(cmd, capture_output=True, text=True, cwd=cwd, timeout=timeout)
    return result.returncode, result.stdout, result.stderr


def _extract_errors(text: str, keyword: str = "error:") -> list[str]:
    return [l.strip() for l in text.splitlines() if keyword.lower() in l.lower()]


def _find_elf(build_dir: str) -> str | None:
    matches = glob.glob(os.path.join(build_dir, "**", "*.elf"), recursive=True)
    return matches[0] if matches else None


@mcp.tool()
def build_firmware(
    project_path: str,
    toolchain_file: str,
    build_dir: str = "build",
    build_type: str = "Debug",
) -> str:
    """
    Configure (CMake + Ninja) and build a firmware project.
    Handles both fresh configure and incremental builds.
    Returns a semantic summary of success or failure with key error lines.

    Args:
        project_path: Absolute path to the CMake project root (where CMakeLists.txt lives).
        toolchain_file: Absolute path to the CMake toolchain file (e.g. arm-none-eabi toolchain).
        build_dir: Build output directory name, relative to project_path. Default: "build".
        build_type: CMake build type — Debug or Release. Default: Debug.
    """
    build_path = os.path.join(project_path, build_dir)
    cmake_cache = os.path.join(build_path, "CMakeCache.txt")

    # Configure if no cache exists yet
    if not os.path.exists(cmake_cache):
        code, out, err = _run([
            "cmake",
            "-B", build_path,
            "-G", "Ninja",
            f"-DCMAKE_TOOLCHAIN_FILE={toolchain_file}",
            f"-DCMAKE_BUILD_TYPE={build_type}",
            project_path,
        ])
        if code != 0:
            errors = _extract_errors(err) or _extract_errors(out) or err.splitlines()[-5:]
            return "Configure FAILED. Key errors:\n" + "\n".join(errors[:10])

    # Build
    code, out, err = _run(["cmake", "--build", build_path, "--", "-j4"], timeout=180)
    if code != 0:
        errors = _extract_errors(err) or _extract_errors(out) or err.splitlines()[-10:]
        return "Build FAILED. Key errors:\n" + "\n".join(errors[:15])

    elf = _find_elf(build_path)
    size_info = ""
    if elf:
        scode, sout, _ = _run(["arm-none-eabi-size", elf])
        if scode == 0:
            size_info = f"\nBinary size:\n{sout.strip()}"

    return f"Build SUCCESS.{size_info}"


@mcp.tool()
def clean_build(project_path: str, build_dir: str = "build") -> str:
    """
    Delete the build directory to force a full reconfigure on next build.

    Args:
        project_path: Absolute path to the CMake project root.
        build_dir: Build directory name to remove. Default: "build".
    """
    build_path = os.path.join(project_path, build_dir)
    if not os.path.exists(build_path):
        return f"Nothing to clean — {build_path} does not exist."
    shutil.rmtree(build_path)
    return f"Cleaned {build_path}. Next build will reconfigure."


@mcp.tool()
def get_build_size(project_path: str, build_dir: str = "build") -> str:
    """
    Report firmware binary size (.text / .data / .bss) from the built ELF.

    Args:
        project_path: Absolute path to the CMake project root.
        build_dir: Build directory name. Default: "build".
    """
    build_path = os.path.join(project_path, build_dir)
    elf = _find_elf(build_path)
    if not elf:
        return f"No .elf file found in {build_path}. Run build_firmware first."
    code, out, err = _run(["arm-none-eabi-size", elf])
    if code != 0:
        return f"ERROR: arm-none-eabi-size failed: {err.strip()}"
    return f"ELF: {os.path.basename(elf)}\n{out.strip()}"


@mcp.tool()
def flash_firmware(
    project_path: str,
    openocd_config: str,
    build_dir: str = "build",
) -> str:
    """
    Flash the built ELF to the target board via OpenOCD and ST-Link.
    NOTE: Requires a connected ST-Link V3 and target board.

    Args:
        project_path: Absolute path to the CMake project root.
        openocd_config: Absolute path to the OpenOCD target config file
                        (e.g. /usr/share/openocd/scripts/target/stm32wlx.cfg).
        build_dir: Build directory name. Default: "build".
    """
    build_path = os.path.join(project_path, build_dir)
    elf = _find_elf(build_path)
    if not elf:
        return f"No .elf file found in {build_path}. Run build_firmware first."

    code, out, err = _run([
        "openocd",
        "-f", "interface/stlink.cfg",
        "-f", openocd_config,
        "-c", f"program {{{elf}}} verify reset exit",
    ], timeout=60)

    combined = (out + err).strip()
    if code != 0:
        errors = _extract_errors(combined, "error") or combined.splitlines()[-5:]
        return "Flash FAILED:\n" + "\n".join(errors[:10])

    if "verified" in combined.lower():
        return f"Flash SUCCESS. Firmware verified and device reset.\nELF: {os.path.basename(elf)}"
    return f"Flash completed (verify status unclear).\nOutput:\n{combined[-300:]}"


if __name__ == "__main__":
    mcp.run()
