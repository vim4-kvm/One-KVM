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
