// SPDX-License-Identifier: AGPL-3.0-only
// Private replacement for the allocator entry point used by amlvconv.
// Each DMA fd belongs solely to GstDmaBufMemory. The vendor implementation
// also closes it from attached qdata, which can close an unrelated reused fd.
#include <fcntl.h>
#include <linux/dma-heap.h>
#include <linux/dma-buf.h>
#include <sys/ioctl.h>
#include <unistd.h>
#include <stdio.h>
#include <gst/allocators/gstdmabuf.h>

typedef struct {
    GstDmaBufAllocator parent;
    int heap_fd;
    GMutex lock;
    gint64 stats_at;
    guint64 allocations;
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
    gint64 started = self->stats_at ? g_get_monotonic_time() : 0;
    if (size > G_MAXSIZE - params->prefix || size + params->prefix > G_MAXSIZE - params->padding)
        return NULL;
    gsize total = size + params->prefix + params->padding;
    struct dma_heap_allocation_data data = { .len = total, .fd_flags = O_RDWR | O_CLOEXEC };
    if (ioctl(self->heap_fd, DMA_HEAP_IOCTL_ALLOC, &data) < 0)
        return NULL;
    if (started) {
        gint64 now = g_get_monotonic_time();
        guint64 elapsed = now - started;
        g_mutex_lock(&self->lock);
        self->allocations++;
        self->allocation_us += elapsed;
        self->max_allocation_us = MAX(self->max_allocation_us, elapsed);
        if (now - self->stats_at >= 5 * G_USEC_PER_SEC) {
            fprintf(stderr, "VIM4 DMA allocations count=%" G_GUINT64_FORMAT
                    " mean_us=%" G_GUINT64_FORMAT " max_us=%" G_GUINT64_FORMAT " bytes=%zu\n",
                    self->allocations, self->allocation_us / self->allocations,
                    self->max_allocation_us, total);
            self->stats_at = now;
            self->allocations = self->allocation_us = self->max_allocation_us = 0;
        }
        g_mutex_unlock(&self->lock);
    }
    GstMemory *memory = gst_dmabuf_allocator_alloc(allocator, data.fd, total);
    if (!memory) {
        close(data.fd);
        return NULL;
    }
    gst_memory_resize(memory, params->prefix, size);
    return memory;
}

static void finalize(GObject *object)
{
    GstVim4DmaAllocator *self = (GstVim4DmaAllocator *)object;
    if (self->heap_fd >= 0)
        close(self->heap_fd);
    g_mutex_clear(&self->lock);
    G_OBJECT_CLASS(gst_vim4_dma_allocator_parent_class)->finalize(object);
}

static void gst_vim4_dma_allocator_class_init(GstVim4DmaAllocatorClass *klass)
{
    GST_ALLOCATOR_CLASS(klass)->alloc = allocate;
    // Keep the inherited GstFdAllocator free function: it owns each DMA fd.
    G_OBJECT_CLASS(klass)->finalize = finalize;
}

static void gst_vim4_dma_allocator_init(GstVim4DmaAllocator *self)
{
    self->heap_fd = -1;
    g_mutex_init(&self->lock);
    self->stats_at = g_strcmp0(g_getenv("ONEKVM_VIDEO_STATS"), "1") == 0
        ? g_get_monotonic_time() : 0;
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
