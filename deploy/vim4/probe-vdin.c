#include <errno.h>
#include <fcntl.h>
#include <linux/videodev2.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

static void checked(int result, const char *operation)
{
    if (result < 0) {
        perror(operation);
        exit(1);
    }
}

static double seconds(void)
{
    struct timespec value;
    clock_gettime(CLOCK_MONOTONIC, &value);
    return value.tv_sec + value.tv_nsec / 1e9;
}

int main(int argc, char **argv)
{
    if (argc != 6) {
        fprintf(stderr, "usage: %s DEVICE INPUT WIDTH HEIGHT FRAMES\n", argv[0]);
        return 2;
    }
    unsigned int input = strtoul(argv[2], NULL, 10);
    unsigned int width = strtoul(argv[3], NULL, 10);
    unsigned int height = strtoul(argv[4], NULL, 10);
    unsigned int frames = strtoul(argv[5], NULL, 10);
    if (!width || !height || width > 3840 || height > 2160 || !frames || frames > 3600)
        return 2;
    int descriptor = open(argv[1], O_RDWR | O_NONBLOCK | O_CLOEXEC);
    checked(descriptor, "open");
    checked(ioctl(descriptor, VIDIOC_S_INPUT, &input), "S_INPUT");
    struct v4l2_format format = { .type = V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE };
    format.fmt.pix_mp.width = width;
    format.fmt.pix_mp.height = height;
    format.fmt.pix_mp.pixelformat = V4L2_PIX_FMT_NV21;
    format.fmt.pix_mp.field = V4L2_FIELD_NONE;
    format.fmt.pix_mp.num_planes = 1;
    checked(ioctl(descriptor, VIDIOC_S_FMT, &format), "S_FMT");
    printf("format=%ux%u planes=%u size=%u stride=%u\n",
           format.fmt.pix_mp.width, format.fmt.pix_mp.height, format.fmt.pix_mp.num_planes,
           format.fmt.pix_mp.plane_fmt[0].sizeimage, format.fmt.pix_mp.plane_fmt[0].bytesperline);
    struct v4l2_requestbuffers request = {
        .count = 4, .type = format.type, .memory = V4L2_MEMORY_MMAP
    };
    checked(ioctl(descriptor, VIDIOC_REQBUFS, &request), "REQBUFS");
    if (request.count > 4 || request.count < 2)
        return 1;
    void *mapped[4] = {0};
    size_t lengths[4] = {0};
    for (unsigned int index = 0; index < request.count; index++) {
        struct v4l2_plane planes[VIDEO_MAX_PLANES] = {0};
        struct v4l2_buffer buffer = {
            .type = format.type, .memory = request.memory, .index = index,
            .length = 1, .m.planes = planes
        };
        checked(ioctl(descriptor, VIDIOC_QUERYBUF, &buffer), "QUERYBUF");
        lengths[index] = planes[0].length;
        mapped[index] = mmap(NULL, lengths[index], PROT_READ, MAP_SHARED,
                             descriptor, planes[0].m.mem_offset);
        if (mapped[index] == MAP_FAILED) {
            perror("mmap");
            return 1;
        }
        checked(ioctl(descriptor, VIDIOC_QBUF, &buffer), "QBUF");
    }
    checked(ioctl(descriptor, VIDIOC_STREAMON, &format.type), "STREAMON");
    double start = seconds();
    double first_frame = 0;
    unsigned int completed = 0, gaps = 0, previous = 0, sequence_advances = 0;
    for (; completed < frames; completed++) {
        struct pollfd waiting = { .fd = descriptor, .events = POLLIN };
        int ready = poll(&waiting, 1, 3000);
        if (ready <= 0) {
            fprintf(stderr, "poll failed/timed out after %u frames: %d\n", completed, ready);
            break;
        }
        struct v4l2_plane planes[VIDEO_MAX_PLANES] = {0};
        struct v4l2_buffer buffer = {
            .type = format.type, .memory = request.memory, .length = 1, .m.planes = planes
        };
        checked(ioctl(descriptor, VIDIOC_DQBUF, &buffer), "DQBUF");
        if (!completed)
            first_frame = seconds();
        if (buffer.index >= request.count)
            return 1;
        if (completed && buffer.sequence > previous + 1)
            gaps += buffer.sequence - previous - 1;
        if (completed && buffer.sequence > previous)
            sequence_advances++;
        previous = buffer.sequence;
        if (completed == 0 || completed == frames - 1) {
            const uint8_t *pixels = mapped[buffer.index];
            uint32_t fingerprint = 2166136261U;
            for (size_t offset = 0; offset < (size_t)width * height; offset += 4096)
                fingerprint = (fingerprint ^ pixels[offset]) * 16777619U;
            printf("frame=%u sequence=%u bytes=%u y=%u,%u,%u hash=%08x timestamp=%ld.%06ld\n", completed,
                   buffer.sequence, planes[0].bytesused, pixels[0], pixels[width / 2], pixels[width - 1],
                   fingerprint, (long)buffer.timestamp.tv_sec, (long)buffer.timestamp.tv_usec);
        }
        checked(ioctl(descriptor, VIDIOC_QBUF, &buffer), "requeue");
    }
    double finished = seconds();
    double elapsed = finished - start;
    checked(ioctl(descriptor, VIDIOC_STREAMOFF, &format.type), "STREAMOFF");
    for (unsigned int index = 0; index < request.count; index++)
        munmap(mapped[index], lengths[index]);
    close(descriptor);
    printf("frames=%u seconds=%.6f fps=%.3f steady_fps=%.3f sequence_advances=%u sequence_gaps=%u\n",
           completed, elapsed, completed / elapsed,
           completed > 1 ? (completed - 1) / (finished - first_frame) : 0, sequence_advances, gaps);
    return completed == frames ? 0 : 1;
}
