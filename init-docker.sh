#!/usr/bin/env bash
# init-docker.sh — build and run faultforge Docker image
# Usage: ./init-docker.sh [build|run|shell]

set -euo pipefail

IMAGE_NAME="faultforge"
CONTAINER_NAME="faultforge-run"

cmd="${1:-build}"

case "$cmd" in
    build)
        echo "=== Building faultforge Docker image ==="
        docker build -t "$IMAGE_NAME" .
        ;;

    run)
        echo "=== Running faultforge (simulated mode) ==="
        docker run --rm \
            --name "$CONTAINER_NAME" \
            "$IMAGE_NAME" \
            run --config /app/campaign.yaml --simulate
        ;;

    run-hw)
        echo "=== Running faultforge with USB passthrough (requires hardware) ==="
        # On Linux, /dev/ttyUSB* or /dev/ttyACM*; on macOS use docker run --device /dev/cu.usbmodem*
        docker run --rm \
            --name "$CONTAINER_NAME" \
            --device /dev/ttyUSB0:/dev/ttyUSB0 \
            --device /dev/ttyACM0:/dev/ttyACM0 \
            "$IMAGE_NAME" \
            run --config /app/campaign.yaml --port /dev/ttyUSB0
        ;;

    shell)
        echo "=== Dropping into faultforge container shell ==="
        docker run --rm -it \
            --name "$CONTAINER_NAME" \
            --entrypoint /bin/bash \
            "$IMAGE_NAME"
        ;;

    flash)
        echo "=== Flashing firmware to STM32 (requires probe-rs + hardware) ==="
        docker run --rm \
            --name "$CONTAINER_NAME" \
            --privileged \
            -v /dev/bus/usb:/dev/bus/usb \
            "$IMAGE_NAME" \
            sh -c "probe-rs download /app/firmware.elf --chip STM32F401RETx"
        ;;

    *)
        echo "Usage: $0 {build|run|run-hw|shell|flash}"
        echo "  build     - Build the Docker image"
        echo "  run       - Run faultforge in --simulate mode (no hardware)"
        echo "  run-hw    - Run with USB device passthrough (Linux)"
        echo "  shell     - Interactive shell in the container"
        echo "  flash     - Flash firmware to STM32 via probe-rs"
        exit 1
        ;;
esac