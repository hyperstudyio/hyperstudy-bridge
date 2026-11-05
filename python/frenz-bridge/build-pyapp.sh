#!/usr/bin/env bash
#
# Build the FRENZ LSL Bridge as a self-contained PyApp binary.
#
# Prerequisites:
#   - Rust toolchain (cargo)
#   - Internet access (downloads PyApp + Python distribution)
#
# Usage:
#   ./build-pyapp.sh                  # Build for current platform
#   ./build-pyapp.sh --target aarch64-apple-darwin   # Cross-compile
#
# Output:
#   src-tauri/resources/frenz-bridge       (macOS/Linux)
#   src-tauri/resources/frenz-bridge.exe   (Windows)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
RESOURCES_DIR="$REPO_ROOT/src-tauri/resources"

# Parse arguments
TARGET=""
while [[ $# -gt 0 ]]; do
    case $1 in
        --target)
            TARGET="$2"
            shift 2
            ;;
        *)
            echo "Unknown argument: $1"
            exit 1
            ;;
    esac
done

# Build wheel first — PyApp requires a file (wheel/sdist), not a directory
echo "=== Building wheel ==="
pip install build 2>/dev/null || python -m pip install build
python -m build --wheel --outdir "$SCRIPT_DIR/dist/"
WHEEL=$(ls "$SCRIPT_DIR"/dist/*.whl | head -1)
echo "Built wheel: $WHEEL"

# PyApp configuration
export PYAPP_PROJECT_PATH="$WHEEL"
export PYAPP_EXEC_CODE="from frenz_lsl_bridge import main; main()"
export PYAPP_DISTRIBUTION_EMBED=1
export PYAPP_PYTHON_VERSION=3.9
export PYAPP_PIP_EXTRA_ARGS="--no-cache-dir"

echo ""
echo "=== Building FRENZ LSL Bridge (PyApp) ==="
echo "Project path: $PYAPP_PROJECT_PATH"
echo "Python version: $PYAPP_PYTHON_VERSION"
echo "Embed distribution: $PYAPP_DISTRIBUTION_EMBED"

# Build PyApp
BUILD_DIR="$SCRIPT_DIR/build"
mkdir -p "$BUILD_DIR"

CARGO_ARGS=(install pyapp --force --root "$BUILD_DIR")
if [ -n "$TARGET" ]; then
    echo "Target: $TARGET"
    # For cross-compilation, we need to set the target for the embedded Python too
    CARGO_ARGS+=(--target "$TARGET")
fi

echo "Running: cargo ${CARGO_ARGS[*]}"
cargo "${CARGO_ARGS[@]}"

# Determine binary name and copy to resources
mkdir -p "$RESOURCES_DIR"

if [[ "$OSTYPE" == "msys" ]] || [[ "$OSTYPE" == "cygwin" ]] || [[ "$OSTYPE" == "win32" ]]; then
    BINARY_NAME="frenz-bridge.exe"
else
    BINARY_NAME="frenz-bridge"
fi

# Find the built binary
# Note: cargo install always places binaries in $BUILD_DIR/bin/
# regardless of --target flag, so no target-specific path needed.
BUILT_BINARY="$BUILD_DIR/bin/$BINARY_NAME"

if [ ! -f "$BUILT_BINARY" ]; then
    # Try alternative pyapp output name
    BUILT_BINARY="$BUILD_DIR/bin/pyapp"
    if [ ! -f "$BUILT_BINARY" ] && [ ! -f "${BUILT_BINARY}.exe" ]; then
        echo "ERROR: Could not find built binary"
        echo "Searched: $BUILD_DIR/bin/"
        ls -la "$BUILD_DIR/bin/" 2>/dev/null || echo "(directory not found)"
        exit 1
    fi
    [ -f "${BUILT_BINARY}.exe" ] && BUILT_BINARY="${BUILT_BINARY}.exe"
fi

cp "$BUILT_BINARY" "$RESOURCES_DIR/$BINARY_NAME"
chmod +x "$RESOURCES_DIR/$BINARY_NAME"

echo ""
echo "=== Build complete ==="
echo "Binary: $RESOURCES_DIR/$BINARY_NAME"
echo "Size: $(du -h "$RESOURCES_DIR/$BINARY_NAME" | cut -f1)"
