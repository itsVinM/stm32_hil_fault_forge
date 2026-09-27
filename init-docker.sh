#!/usr/bin/env bash
# init-docker.sh — build and drive the faultforge Docker image.
# Usage: ./init-docker.sh [build|run|shell|flash]

set -euo pipefail

IMAGE_NAME="faultforge"
CONTAINER_NAME="faultforge-run"
SERIAL_DEVICE="${SERIAL_DEVICE:-/dev/ttyACM0}"

case "${1:-build}" in
    build)
        echo "=== Building $IMAGE_NAME ==="
        docker build -t "$IMAGE_NAME" .
        ;;

    run)
        # The sender needs the ST-LINK VCP passed through. On Linux that is
        # usually /dev/ttyACM* or /dev/ttyUSB*; on macOS pass
        # SERIAL_DEVICE=/dev/cu.usbmodem* instead.
        echo "=== Sending a campaign over $SERIAL_DEVICE ==="
        docker run --rm \
            --name "$CONTAINER_NAME" \
            --device "$SERIAL_DEVICE:$SERIAL_DEVICE" \
            "$IMAGE_NAME" \
            --port "$SERIAL_DEVICE" "${@:2}"
        ;;

    ports)
        docker run --rm --name "$CONTAINER_NAME" "$IMAGE_NAME" --list-ports
        ;;

    shell)
        docker run --rm -it --name "$CONTAINER_NAME" \
            --entrypoint /bin/bash "$IMAGE_NAME"
        ;;

    flash)
        # Needs probe-rs plus USB bus access.
        docker run --rm --privileged \
            -v /dev/bus/usb:/dev/bus/usb \
            --entrypoint probe-rs "$IMAGE_NAME" \
            download /app/firmware.elf --chip STM32F401RETx
        ;;

    *)
        echo "Usage: $0 [build|run|ports|shell|flash]"
        echo "  build   Build the image"
        echo "  run     Send a campaign (needs SERIAL_DEVICE, default $SERIAL_DEVICE)"
        echo "  ports   List serial ports visible to the container"
        echo "  shell   Interactive shell in the container"
        echo "  flash   Flash firmware.elf to the STM32 via probe-rs"
        exit 1
        ;;
esac
