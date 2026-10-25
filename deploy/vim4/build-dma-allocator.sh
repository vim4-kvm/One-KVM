#!/bin/sh
set -eu
cd "$(dirname "$0")/../.."
mkdir -p target/vim4
# Intentional word splitting: pkg-config emits compiler/linker arguments.
cc -Wall -Wextra -Werror -fPIC -shared deploy/vim4/dma-allocator.c \
  $(pkg-config --cflags --libs gstreamer-allocators-1.0) \
  -Wl,-soname,libgst-aml-dma-allocator.so -o target/vim4/libgst-aml-dma-allocator.so
cc -Wall -Wextra -Werror deploy/vim4/test-dma-allocator.c deploy/vim4/dma-allocator.c \
  $(pkg-config --cflags --libs gstreamer-allocators-1.0) \
  -Wl,--wrap=open,--wrap=ioctl -o target/vim4/test-dma-allocator
target/vim4/test-dma-allocator
install -D -m 0755 /usr/lib/aarch64-linux-gnu/gstreamer-1.0/libgstvideo4linux2.so \
  target/vim4/gstreamer-1.0/libgstvideo4linux2.so
if ldd target/vim4/gstreamer-1.0/libgstvideo4linux2.so | grep -q libamlv4l2src; then
  echo 'Build with the standard Ubuntu video4linux2 plugin, not the vendor HDMI wrapper' >&2
  exit 1
fi
