//! 内存探针：只在 `cfg(test)` 编译，生产的 dll 里不带这一块。
//!
//! 要回答的问题是"用完的资源有没有释放"，而任务管理器只给一个进程总数、还分不清常驻与峰值。
//! 这里用一层计数分配器包住系统分配器，于是这三件事能各自落成数字：
//! * **常驻**：一批对象活着时占多少字节（字体、素材、画布）；
//! * **峰值**：整个探针期间存活字节的高水位（画一张卡时同时压着多少份拷贝）；
//! * **回落**：把那批对象 drop 掉之后常驻有没有回到基线——回不去就是泄漏。
//!
//! 记账口径是"分配器要求的布局大小"，不含分配器自身的开销，所以比真实私有字节略小；
//! 用它做前后对比足够，别拿去和对端报的数字较真。

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

/// 当前存活字节
static LIVE: AtomicUsize = AtomicUsize::new(0);
/// 探针期间的高水位
static PEAK: AtomicUsize = AtomicUsize::new(0);
/// 累计分配过的字节（含已释放），用来暴露"反复整份拷贝"这种白烧的流量
static CUMUL: AtomicUsize = AtomicUsize::new(0);

/// 包住系统分配器，只多三个原子操作
pub(crate) struct Counting;

/// 记一笔：`delta` 为负表示释放
fn track(old: usize, new: usize) {
    // 用 wrapping：计数只是观测，不该因为一次加减顺序问题把测试弄崩
    let live = if new >= old {
        LIVE.fetch_add(new - old, Ordering::Relaxed) + (new - old)
    } else {
        LIVE.fetch_sub(old - new, Ordering::Relaxed)
            .saturating_sub(old - new)
    };
    PEAK.fetch_max(live, Ordering::Relaxed);
    if new > 0 {
        CUMUL.fetch_add(new, Ordering::Relaxed);
    }
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            track(0, layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            track(0, layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        track(layout.size(), 0);
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let done = unsafe { System.realloc(ptr, layout, new_size) };
        if !done.is_null() {
            track(layout.size(), new_size);
        }
        done
    }
}

/// 一个时刻的三项读数
#[derive(Clone, Copy, Debug)]
pub(crate) struct Mark {
    pub live: usize,
    pub peak: usize,
    pub cumul: usize,
}

/// 取当前读数
pub(crate) fn mark() -> Mark {
    Mark {
        live: LIVE.load(Ordering::Relaxed),
        peak: PEAK.load(Ordering::Relaxed),
        cumul: CUMUL.load(Ordering::Relaxed),
    }
}

/// 高水位从零重算：让每个探针各报各的峰值，不被上一条用例的历史值污染
pub(crate) fn reset_peak() {
    PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
}

/// 打印成"多少 MB"，整数位留两位足够看量级
pub(crate) fn mb(bytes: usize) -> String {
    format!("{:.1} MB", bytes as f64 / 1048576.0)
}

/// 操作系统那一头的读数：（工作集, 峰值工作集, 私有字节）。
///
/// 存活字节只回答"分配器以为你还拿着多少"，而任务管理器「内存」列的是工作集——
/// drop 之后内存到底降不降、是不是只是把页留在了进程里，得看这一组数。
/// 只在 Windows 上取（部署机与开发机都是它），别的平台回 0。
pub(crate) fn os_footprint() -> (usize, usize, usize) {
    #[cfg(windows)]
    {
        use std::ffi::c_void;
        // PROCESS_MEMORY_COUNTERS_EX 的字段一个不能少：`cb` 报的长度对不上，API 直接回 FALSE
        #[repr(C)]
        struct Counters {
            cb: u32,
            page_faults: u32,
            peak_working_set: usize,
            working_set: usize,
            quota_peak_paged: usize,
            quota_paged: usize,
            quota_peak_nonpaged: usize,
            quota_nonpaged: usize,
            pagefile: usize,
            peak_pagefile: usize,
            private_usage: usize,
        }
        unsafe extern "system" {
            fn K32GetProcessMemoryInfo(
                process: *mut c_void,
                counters: *mut Counters,
                cb: u32,
            ) -> i32;
        }
        let mut taken = Counters {
            cb: std::mem::size_of::<Counters>() as u32,
            page_faults: 0,
            peak_working_set: 0,
            working_set: 0,
            quota_peak_paged: 0,
            quota_paged: 0,
            quota_peak_nonpaged: 0,
            quota_nonpaged: 0,
            pagefile: 0,
            peak_pagefile: 0,
            private_usage: 0,
        };
        // (HANDLE)-1 就是当前进程，不必再开一份 FFI 去取
        let current = -1isize as *mut c_void;
        let ok = unsafe { K32GetProcessMemoryInfo(current, &mut taken, taken.cb) };
        if ok == 0 {
            return (0, 0, 0);
        }
        (
            taken.working_set,
            taken.peak_working_set,
            taken.private_usage,
        )
    }
    #[cfg(not(windows))]
    {
        (0, 0, 0)
    }
}
