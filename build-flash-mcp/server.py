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


def _find_elfs(search_root: str) -> list[str]:
    return glob.glob(os.path.join(search_root, "**", "*.elf"), recursive=True)


@mcp.tool()
def build_firmware(
    project_path: str,
    preset: str = "",
    toolchain_file: str = "",
    build_dir: str = "build",
    build_type: str = "Debug",
) -> str:
    """
    Configure (CMake + Ninja) and build a firmware project.
    Supports both CMakePresets.json (preferred) and manual toolchain file.
    Handles both fresh configure and incremental builds.
    Returns a semantic summary of success or failure with key error lines.

    Args:
        project_path: Absolute path to the CMake project root (where CMakeLists.txt lives).
        preset: CMake preset name from CMakePresets.json (e.g. "Debug", "Release").
                If provided, toolchain_file/build_dir/build_type are ignored.
        toolchain_file: Absolute path to the CMake toolchain file. Used only when no preset.
        build_dir: Build output directory, relative to project_path. Used only when no preset.
        build_type: CMake build type (Debug or Release). Used only when no preset.
    """
    if preset:
        # Preset-based workflow
        presets_file = os.path.join(project_path, "CMakePresets.json")
        if not os.path.exists(presets_file):
            return f"ERROR: CMakePresets.json not found in {project_path}"

        # Determine build dir from preset (binaryDir pattern: build/<preset>)
        build_path = os.path.join(project_path, "build", preset)
        cmake_cache = os.path.join(build_path, "CMakeCache.txt")

        if not os.path.exists(cmake_cache):
            code, out, err = _run(
                ["cmake", "--preset", preset], cwd=project_path
            )
            if code != 0:
                errors = _extract_errors(err) or _extract_errors(out) or err.splitlines()[-5:]
                return "Configure FAILED. Key errors:\n" + "\n".join(errors[:10])

        code, out, err = _run(
            ["cmake", "--build", "--preset", preset], cwd=project_path, timeout=180
        )
    else:
        # Manual toolchain workflow
        if not toolchain_file:
            return "ERROR: Provide either a preset name or a toolchain_file path."
        build_path = os.path.join(project_path, build_dir)
        cmake_cache = os.path.join(build_path, "CMakeCache.txt")

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

        code, out, err = _run(
            ["cmake", "--build", build_path, "--", "-j4"], timeout=180
        )

    if code != 0:
        errors = _extract_errors(err) or _extract_errors(out) or err.splitlines()[-10:]
        return "Build FAILED. Key errors:\n" + "\n".join(errors[:15])

    elfs = _find_elfs(project_path)
    size_info = ""
    if elfs:
        lines = []
        for elf in sorted(elfs):
            scode, sout, _ = _run(["arm-none-eabi-size", elf])
            if scode == 0:
                lines.append(f"{os.path.basename(elf)}:\n{sout.strip()}")
        if lines:
            size_info = "\nBinary sizes:\n" + "\n\n".join(lines)

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
    elfs = _find_elfs(project_path)
    if not elfs:
        return f"No .elf file found under {project_path}. Run build_firmware first."
    lines = []
    for elf in sorted(elfs):
        code, out, err = _run(["arm-none-eabi-size", elf])
        if code == 0:
            lines.append(f"{os.path.basename(elf)}:\n{out.strip()}")
    return "\n\n".join(lines) if lines else "arm-none-eabi-size failed on all ELFs."


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
    elfs = _find_elfs(project_path)
    if not elfs:
        return f"No .elf file found under {project_path}. Run build_firmware first."
    elf = sorted(elfs)[0]

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
