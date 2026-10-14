// Exercise fd ownership without requiring a DMA heap in the build container.
#define _GNU_SOURCE
#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <linux/dma-heap.h>
#include <stdarg.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>
#include <gst/allocators/gstdmabuf.h>

extern int __real_open(const char *, int, ...);
extern GstAllocator *gst_amldma_allocator_obtain(const char *);
int __wrap_open(const char *path, int flags, ...)
{
    (void)path;
    return __real_open("/dev/null", flags);
}

int __wrap_ioctl(int fd, unsigned long request, ...)
{
    assert(fcntl(fd, F_GETFD) >= 0);
    assert(request == DMA_HEAP_IOCTL_ALLOC);
    va_list args;
    va_start(args, request);
    struct dma_heap_allocation_data *data = va_arg(args, struct dma_heap_allocation_data *);
    va_end(args);
    int buffer = memfd_create("dma-test", MFD_CLOEXEC);
    assert(buffer >= 0 && ftruncate(buffer, data->len) == 0);
    data->fd = buffer;
    return 0;
}

int main(int argc, char **argv)
{
    gst_init(&argc, &argv);
    for (int i = 0; i < 100; i++) {
        GstAllocator *allocator = gst_amldma_allocator_obtain("heap-codecmm");
        assert(allocator);
        GstMemory *memory = gst_allocator_alloc(allocator, 4096, NULL);
        assert(memory && gst_is_dmabuf_memory(memory));
        int fd = gst_dmabuf_memory_get_fd(memory);
        assert(fcntl(fd, F_GETFD) >= 0);
        // The memory must retain the allocator until the last buffer is released.
        gst_object_unref(allocator);
        GstMemory *held = gst_memory_ref(memory);
        gst_memory_unref(memory);
        assert(fcntl(fd, F_GETFD) >= 0);
        gst_memory_unref(held);
        assert(fcntl(fd, F_GETFD) == -1 && errno == EBADF);
        int reused = __real_open("/dev/null", O_RDONLY | O_CLOEXEC);
        assert(reused >= 0 && fcntl(reused, F_GETFD) >= 0);
        close(reused);
    }
    return 0;
}
