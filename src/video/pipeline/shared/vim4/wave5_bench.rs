//! Local measurements of production buffer helpers; no video devices are opened.
use super::*;
use std::os::fd::{AsRawFd, FromRawFd};
use std::{hint::black_box, time::Instant};

struct Case {
    name: String,
    payload_bytes: usize,
    copied_bytes: usize,
    run: Box<dyn FnMut()>,
}

fn dma_buffer(payload: usize, capacity: usize) -> gst::Buffer {
    use std::os::unix::fs::FileExt;

    let file = tempfile::tempfile().unwrap();
    file.set_len(capacity as u64).unwrap();
    // Populate before wrapping: a writable GstBuffer map may silently copy shared
    // memory and would stop exercising the file-backed DMA allocator.
    file.write_all_at(&vec![0x5a; payload], 0).unwrap();
    let memory = unsafe { DmaBufAllocator::new().alloc_dmabuf(file, capacity) }.unwrap();
    let mut buffer = gst::Buffer::new();
    buffer
        .get_mut()
        .unwrap()
        .append_memory(memory.share(0..payload));
    buffer
}

fn heap_sync(fd: i32, flags: u64) {
    // Linux generic ioctl encoding: _IOW('b', 0, struct dma_buf_sync).
    // The optional real-heap mode is supported on aarch64/x86_64 only.
    loop {
        if unsafe { libc::ioctl(fd, 0x40086200 as libc::c_ulong, &flags) } == 0 {
            return;
        }
        let error = std::io::Error::last_os_error();
        assert!(
            matches!(error.raw_os_error(), Some(libc::EINTR | libc::EAGAIN)),
            "DMA_BUF_IOCTL_SYNC failed: {error}"
        );
    }
}

fn heap_buffer(path: &str, payload: usize) -> gst::Buffer {
    #[repr(C)]
    struct Allocation {
        len: u64,
        fd: u32,
        fd_flags: u32,
        heap_flags: u64,
    }
    assert!(matches!(std::env::consts::ARCH, "aarch64" | "x86_64"));
    let heap = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    let mut request = Allocation {
        len: payload as u64,
        fd: 0,
        fd_flags: (libc::O_RDWR | libc::O_CLOEXEC) as u32,
        heap_flags: 0,
    };
    // _IOWR('H', 0, struct dma_heap_allocation_data), sizeof == 24.
    assert_eq!(std::mem::size_of::<Allocation>(), 24);
    assert_eq!(
        unsafe { libc::ioctl(heap.as_raw_fd(), 0xc0184800 as libc::c_ulong, &mut request) },
        0,
        "DMA_HEAP_IOCTL_ALLOC: {}",
        std::io::Error::last_os_error()
    );
    let file = unsafe { std::fs::File::from_raw_fd(request.fd as i32) };
    let mut memory = unsafe { DmaBufAllocator::new().alloc_dmabuf(file, payload) }.unwrap();
    heap_sync(request.fd as i32, 2); // START | WRITE
    memory
        .get_mut()
        .unwrap()
        .map_writable()
        .unwrap()
        .as_mut_slice()
        .fill(0x5a);
    heap_sync(request.fd as i32, 2 | 4); // END | WRITE
    let mut buffer = gst::Buffer::new();
    buffer.get_mut().unwrap().append_memory(memory);
    buffer
}

fn cases() -> Vec<Case> {
    let mut cases = Vec::new();
    for (width, height) in [(1920usize, 1080usize), (3840, 2160)] {
        let payload = width * height * 3 / 2;
        let capacity = width * height + width * ((height + 15) & !15) / 2;
        let parents: Vec<_> = (0..4).map(|_| dma_buffer(payload, capacity)).collect();
        let mut index = 0;
        cases.push(Case {
            name: format!("plane_view_{width}x{height}"),
            payload_bytes: payload,
            copied_bytes: 0,
            run: Box::new(move || {
                let view = plane_view(black_box(&parents[index % 4]), width as u32, height as u32)
                    .unwrap();
                black_box(view);
                index += 1;
            }),
        });

        // Ordinary cached-memory reference only, not the kernel's DMA memcpy.
        // Rotate full frames so this is not repeatedly copying one tiny hot span.
        let sources = vec![vec![0x5a; payload]; 3];
        let mut destinations = vec![vec![0; payload]; 4];
        let mut index = 0;
        cases.push(Case {
            name: format!("cached_ram_copy_{width}x{height}"),
            payload_bytes: payload,
            copied_bytes: payload,
            run: Box::new(move || {
                black_box(&mut destinations[index % 4])
                    .copy_from_slice(black_box(&sources[index % 3]));
                black_box(&destinations[index % 4]);
                index += 1;
            }),
        });
    }
    // 30 Mbps / 60 fps average payload and a synthetic large keyframe. Contents
    // are not an encoded stream: this measures copying, never codec parsing.
    for payload in [62_500usize, 1024 * 1024] {
        let sources: Vec<_> = (0..4)
            .map(|_| dma_buffer(payload, 2 * 1024 * 1024))
            .collect();
        let mut index = 0;
        cases.push(Case {
            name: format!("cached_bitstream_{payload}"),
            payload_bytes: payload,
            copied_bytes: payload,
            run: Box::new(move || {
                black_box(cached_bitstream(black_box(&sources[index % 4])).unwrap());
                index += 1;
            }),
        });
    }
    if let Ok(heaps) = std::env::var("VIM4_BENCH_DMA_HEAPS") {
        for path in heaps.split(',') {
            assert!(
                path.starts_with("/dev/dma_heap/"),
                "expected a DMA heap path"
            );
            for payload in [62_500usize, 1024 * 1024] {
                let sources: Vec<_> = (0..4).map(|_| heap_buffer(path, payload)).collect();
                let mut index = 0;
                cases.push(Case {
                    name: format!(
                        "cached_bitstream_{payload}_{}",
                        path.rsplit('/').next().unwrap()
                    ),
                    payload_bytes: payload,
                    copied_bytes: payload,
                    run: Box::new(move || {
                        let source = &sources[index % 4];
                        let fd = source
                            .peek_memory(0)
                            .downcast_memory_ref::<DmaBufMemory>()
                            .unwrap()
                            .fd();
                        heap_sync(fd, 1); // START | READ
                        let cached = cached_bitstream(black_box(source));
                        heap_sync(fd, 1 | 4); // END | READ
                        black_box(cached.unwrap());
                        index += 1;
                    }),
                });
            }
        }
    }
    cases
}

fn batch(case: &mut Case, iterations: usize) -> f64 {
    let start = Instant::now();
    for _ in 0..iterations {
        (case.run)();
    }
    start.elapsed().as_secs_f64() * 1e9
}

#[test]
#[ignore = "local benchmark; run through tools/onekvm-local-bench.py in the main workspace"]
fn local_buffer_paths() {
    assert!(!cfg!(debug_assertions), "benchmark requires --release");
    gst::init().unwrap();
    let samples: usize = std::env::var("VIM4_BENCH_SAMPLES")
        .unwrap_or("9".into())
        .parse()
        .unwrap();
    let batch_ms: usize = std::env::var("VIM4_BENCH_BATCH_MS")
        .unwrap_or("100".into())
        .parse()
        .unwrap();
    assert!((3..=100).contains(&samples));
    assert!((10..=1000).contains(&batch_ms));
    let mut results = Vec::new();
    for mut case in cases() {
        let mut iterations = 1;
        // Calibration also warms the file mappings, allocator and frame spans.
        while batch(&mut case, iterations) < batch_ms as f64 * 1e6 {
            iterations *= 2;
            assert!(iterations <= 1 << 26, "calibration did not converge");
        }
        let mut times: Vec<_> = (0..samples)
            .map(|_| batch(&mut case, iterations) / iterations as f64)
            .collect();
        times.sort_by(f64::total_cmp);
        let median = if samples % 2 == 0 {
            (times[samples / 2 - 1] + times[samples / 2]) / 2.0
        } else {
            times[samples / 2]
        };
        results.push(serde_json::json!({
            "name": case.name, "payload_bytes": case.payload_bytes,
            "copied_bytes_per_op": case.copied_bytes,
            "iterations_per_batch": iterations, "ns_per_op": times,
            "median_ns": median, "p95_ns": times[(samples * 95).div_ceil(100) - 1],
        }));
    }
    println!(
        "VIM4_BENCH_JSON {}",
        serde_json::json!({
            "schema": 1, "workload": "wave5-local-buffers-v1",
            "memory_model": "ordinary cached RAM and file-backed GstDmaBufMemory; no hardware DMA",
            "gstreamer": gst::version_string().as_str(), "arch": std::env::consts::ARCH,
            "dma_heaps": std::env::var("VIM4_BENCH_DMA_HEAPS").ok(),
            "samples": samples, "batch_ms": batch_ms, "cases": results,
        })
    );
}
