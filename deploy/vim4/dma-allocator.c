// SPDX-License-Identifier: AGPL-3.0-only
// Private replacement for the allocator entry point used by amlvconv.
#include <fcntl.h>
#include <linux/dma-heap.h>
#include <linux/dma-buf.h>
#include <sys/ioctl.h>
#include <unistd.h>
#include <stdio.h>
#include <gst/allocators/gstdmabuf.h>

typedef struct {
    int fd;
    gsize size;
} CachedBuffer;

enum { MAX_CACHED_BUFFERS = 4 };

typedef struct {
    GstDmaBufAllocator parent;
    int heap_fd;
    GMutex lock;
    CachedBuffer cached[MAX_CACHED_BUFFERS];
    guint cached_count;
    gboolean stats_enabled;
    gint64 stats_at;
    guint64 allocations;
    guint64 reused;
    guint64 allocation_us;
    guint64 max_allocation_us;
} GstVim4DmaAllocator;
typedef GstDmaBufAllocatorClass GstVim4DmaAllocatorClass;
G_DEFINE_TYPE(GstVim4DmaAllocator, gst_vim4_dma_allocator, GST_TYPE_DMABUF_ALLOCATOR)

// libgst-aml-gfx2d also relies on this symbol from the allocator's dependencies.
int aml_dmabuf_sync(int fd, int phase)
{
    struct dma_buf_sync request = { .flags = DMA_BUF_SYNC_RW | (unsigned)phase };
    return ioctl(fd, DMA_BUF_IOCTL_SYNC, &request);
}

static GstMemory *allocate(GstAllocator *allocator, gsize size, GstAllocationParams *params)
{
    GstVim4DmaAllocator *self = (GstVim4DmaAllocator *)allocator;
    gint64 started = self->stats_enabled ? g_get_monotonic_time() : 0;
    if (size > G_MAXSIZE - params->prefix || size + params->prefix > G_MAXSIZE - params->padding)
        return NULL;
    gsize total = size + params->prefix + params->padding;
    int buffer_fd = -1;
    g_mutex_lock(&self->lock);
    for (guint index = 0; index < self->cached_count; index++) {
        if (self->cached[index].size == total) {
            buffer_fd = self->cached[index].fd;
            self->cached[index] = self->cached[--self->cached_count];
            break;
        }
    }
    g_mutex_unlock(&self->lock);
    gboolean reused = buffer_fd >= 0;
    if (!reused) {
        struct dma_heap_allocation_data data = { .len = total, .fd_flags = O_RDWR | O_CLOEXEC };
        if (ioctl(self->heap_fd, DMA_HEAP_IOCTL_ALLOC, &data) < 0)
            return NULL;
        buffer_fd = data.fd;
    }
    if (started) {
        gint64 now = g_get_monotonic_time();
        guint64 elapsed = now - started;
        g_mutex_lock(&self->lock);
        self->allocations++;
        self->reused += reused;
        self->allocation_us += elapsed;
        self->max_allocation_us = MAX(self->max_allocation_us, elapsed);
        if (now - self->stats_at >= 5 * G_USEC_PER_SEC) {
            fprintf(stderr, "VIM4 DMA allocations count=%" G_GUINT64_FORMAT
                    " reused=%" G_GUINT64_FORMAT
                    " mean_us=%" G_GUINT64_FORMAT " max_us=%" G_GUINT64_FORMAT " bytes=%zu\n",
                    self->allocations, self->reused, self->allocation_us / self->allocations,
                    self->max_allocation_us, total);
            self->stats_at = now;
            self->allocations = self->allocation_us = self->max_allocation_us = 0;
            self->reused = 0;
        }
        g_mutex_unlock(&self->lock);
    }
    GstMemory *memory = gst_dmabuf_allocator_alloc_with_flags(
        allocator, buffer_fd, total, GST_FD_MEMORY_FLAG_DONT_CLOSE);
    if (!memory) {
        close(buffer_fd);
        return NULL;
    }
    gst_memory_resize(memory, params->prefix, size);
    return memory;
}

static void release_memory(GstAllocator *allocator, GstMemory *memory)
{
    GstVim4DmaAllocator *self = (GstVim4DmaAllocator *)allocator;
    int buffer_fd = memory->parent ? -1 : gst_dmabuf_memory_get_fd(memory);
    gsize total = memory->maxsize;
    GST_ALLOCATOR_CLASS(gst_vim4_dma_allocator_parent_class)->free(allocator, memory);
    if (buffer_fd < 0)
        return;
    g_mutex_lock(&self->lock);
    if (self->cached_count < MAX_CACHED_BUFFERS) {
        self->cached[self->cached_count++] = (CachedBuffer){ .fd = buffer_fd, .size = total };
        buffer_fd = -1;
    }
    g_mutex_unlock(&self->lock);
    if (buffer_fd >= 0)
        close(buffer_fd);
}

static void finalize(GObject *object)
{
    GstVim4DmaAllocator *self = (GstVim4DmaAllocator *)object;
    if (self->heap_fd >= 0)
        close(self->heap_fd);
    for (guint index = 0; index < self->cached_count; index++)
        close(self->cached[index].fd);
    g_mutex_clear(&self->lock);
    G_OBJECT_CLASS(gst_vim4_dma_allocator_parent_class)->finalize(object);
}

static void gst_vim4_dma_allocator_class_init(GstVim4DmaAllocatorClass *klass)
{
    GST_ALLOCATOR_CLASS(klass)->alloc = allocate;
    GST_ALLOCATOR_CLASS(klass)->free = release_memory;
    G_OBJECT_CLASS(klass)->finalize = finalize;
}

static void gst_vim4_dma_allocator_init(GstVim4DmaAllocator *self)
{
    self->heap_fd = -1;
    g_mutex_init(&self->lock);
    self->stats_enabled = g_strcmp0(g_getenv("ONEKVM_VIDEO_STATS"), "1") == 0;
    self->stats_at = self->stats_enabled ? g_get_monotonic_time() : 0;
}

GstAllocator *gst_amldma_allocator_obtain(const char *name)
{
    if (g_strcmp0(name, "heap-codecmm") != 0)
        return NULL;
    GstVim4DmaAllocator *self = g_object_new(gst_vim4_dma_allocator_get_type(), NULL);
    self->heap_fd = open("/dev/dma_heap/heap-codecmm", O_RDWR | O_CLOEXEC);
    if (self->heap_fd < 0) {
        g_object_unref(self);
        return NULL;
    }
    return GST_ALLOCATOR(self);
}
