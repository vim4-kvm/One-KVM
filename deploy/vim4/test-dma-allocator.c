// Exercise fd ownership without requiring a DMA heap in the build container.
#define _GNU_SOURCE
#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <linux/dma-heap.h>
#include <pthread.h>
#include <stdarg.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>
#include <gst/allocators/gstdmabuf.h>

extern int __real_open(const char *, int, ...);
extern GstAllocator *gst_amldma_allocator_obtain(const char *);
static unsigned heap_allocations;
static gboolean fail_next_allocation;
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
    if (fail_next_allocation) {
        fail_next_allocation = FALSE;
        errno = ENOMEM;
        return -1;
    }
    heap_allocations++;
    int buffer = memfd_create("dma-test", MFD_CLOEXEC);
    assert(buffer >= 0 && ftruncate(buffer, data->len) == 0);
    data->fd = buffer;
    return 0;
}

static void held_buffers_and_shared_views_are_not_recycled(void)
{
    GstAllocator *allocator = gst_amldma_allocator_obtain("heap-codecmm");
    unsigned before = heap_allocations;
    GstMemory *first = gst_allocator_alloc(allocator, 4096, NULL);
    GstMemory *second = gst_allocator_alloc(allocator, 4096, NULL);
    int first_fd = gst_dmabuf_memory_get_fd(first);
    int second_fd = gst_dmabuf_memory_get_fd(second);
    assert(first_fd != second_fd);
    GstMemory *view = gst_memory_share(first, 0, 64);
    assert(view);
    gst_memory_unref(first);
    GstMemory *third = gst_allocator_alloc(allocator, 4096, NULL);
    int third_fd = gst_dmabuf_memory_get_fd(third);
    assert(third_fd != first_fd && third_fd != second_fd);
    gst_memory_unref(view);
    for (unsigned frame = 0; frame < 1000; frame++) {
        GstMemory *next = gst_allocator_alloc(allocator, 4096, NULL);
        assert(gst_dmabuf_memory_get_fd(next) == first_fd);
        assert(fcntl(second_fd, F_GETFD) >= 0 && fcntl(third_fd, F_GETFD) >= 0);
        gst_memory_unref(next);
    }
    assert(heap_allocations - before == 3);
    gst_memory_unref(second);
    gst_memory_unref(third);
    gst_object_unref(allocator);
    assert(fcntl(first_fd, F_GETFD) == -1 && errno == EBADF);
    assert(fcntl(second_fd, F_GETFD) == -1 && errno == EBADF);
    assert(fcntl(third_fd, F_GETFD) == -1 && errno == EBADF);
}

typedef struct {
    GstAllocator *allocator;
    pthread_barrier_t *barrier;
    int *descriptors;
    unsigned index;
} ConcurrentWorker;

static void *allocate_concurrently(void *argument)
{
    ConcurrentWorker *worker = argument;
    for (unsigned frame = 0; frame < 1000; frame++) {
        GstMemory *memory = gst_allocator_alloc(worker->allocator, 4096, NULL);
        assert(memory);
        worker->descriptors[worker->index] = gst_dmabuf_memory_get_fd(memory);
        pthread_barrier_wait(worker->barrier);
        assert(worker->descriptors[0] != worker->descriptors[1]);
        assert(fcntl(worker->descriptors[0], F_GETFD) >= 0);
        assert(fcntl(worker->descriptors[1], F_GETFD) >= 0);
        pthread_barrier_wait(worker->barrier);
        gst_memory_unref(memory);
        pthread_barrier_wait(worker->barrier);
    }
    return NULL;
}

static void concurrent_stages_keep_exclusive_buffer_ownership(void)
{
    GstAllocator *allocator = gst_amldma_allocator_obtain("heap-codecmm");
    pthread_barrier_t barrier;
    assert(pthread_barrier_init(&barrier, NULL, 2) == 0);
    int descriptors[2];
    pthread_t threads[2];
    ConcurrentWorker workers[2];
    for (unsigned index = 0; index < G_N_ELEMENTS(workers); index++) {
        workers[index] = (ConcurrentWorker){ allocator, &barrier, descriptors, index };
        assert(pthread_create(&threads[index], NULL, allocate_concurrently, &workers[index]) == 0);
    }
    for (unsigned index = 0; index < G_N_ELEMENTS(workers); index++)
        assert(pthread_join(threads[index], NULL) == 0);
    assert(pthread_barrier_destroy(&barrier) == 0);
    gst_object_unref(allocator);
    for (unsigned index = 0; index < G_N_ELEMENTS(workers); index++)
        assert(fcntl(descriptors[index], F_GETFD) == -1 && errno == EBADF);
}

static void cache_is_bounded_and_allocation_failure_is_recoverable(void)
{
    GstAllocator *allocator = gst_amldma_allocator_obtain("heap-codecmm");
    GstMemory *held[8];
    int descriptors[8];
    for (unsigned index = 0; index < G_N_ELEMENTS(held); index++) {
        held[index] = gst_allocator_alloc(allocator, 8192, NULL);
        descriptors[index] = gst_dmabuf_memory_get_fd(held[index]);
    }
    for (unsigned index = 0; index < G_N_ELEMENTS(held); index++)
        gst_memory_unref(held[index]);
    unsigned retained = 0;
    for (unsigned index = 0; index < G_N_ELEMENTS(held); index++)
        retained += fcntl(descriptors[index], F_GETFD) >= 0;
    assert(retained > 0 && retained < G_N_ELEMENTS(held));
    fail_next_allocation = TRUE;
    assert(gst_allocator_alloc(allocator, 16384, NULL) == NULL);
    GstAllocationParams params;
    gst_allocation_params_init(&params);
    params.prefix = 32;
    params.padding = 64;
    GstMemory *resized = gst_allocator_alloc(allocator, 16384, &params);
    assert(resized && resized->offset == 32 && resized->size == 16384);
    gst_memory_unref(resized);
    unsigned before = heap_allocations;
    resized = gst_allocator_alloc(allocator, 16384, &params);
    assert(resized);
    gst_memory_unref(resized);
    assert(heap_allocations == before + 1);
    gst_object_unref(allocator);
    for (unsigned index = 0; index < G_N_ELEMENTS(held); index++)
        assert(fcntl(descriptors[index], F_GETFD) == -1 && errno == EBADF);
}

int main(int argc, char **argv)
{
    gst_init(&argc, &argv);
    held_buffers_and_shared_views_are_not_recycled();
    concurrent_stages_keep_exclusive_buffer_ownership();
    cache_is_bounded_and_allocation_failure_is_recoverable();
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
