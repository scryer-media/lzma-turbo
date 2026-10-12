//! Linux fixed-file buffered-input diagnostic for the zero-input audit.
//! Unlike encoder-perf-probe.rs, this caller requests huge pages before filling
//! its owned input. The encoder and original baseline remain unchanged.
//! Arguments: 1|2 total-threads immutable-input [raw-output].
#[cfg(not(target_os = "linux"))]
compile_error!("This diagnostic uses Linux MADV_HUGEPAGE");
use lzma_turbo::{BLOCK_SIZE_AUTO, Lzma2Encoder, LzmaEncProps, LzmaEncoder};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let data = read_input(&args[3]);
    let threads: u32 = args[2].parse().unwrap();
    let props = LzmaEncProps::new()
        .with_level(5)
        .with_num_threads(threads.min(2));
    let now = std::time::Instant::now();
    let out = if args[1] == "1" {
        LzmaEncoder::new(&props)
            .unwrap()
            .encode_to_vec(&data)
            .unwrap()
    } else {
        let mut enc = Lzma2Encoder::new(&props).unwrap();
        enc.set_total_threads(threads as usize);
        enc.set_block_size(BLOCK_SIZE_AUTO);
        enc.encode_to_vec(&data).unwrap()
    };
    println!("{} {:.9}", out.len(), now.elapsed().as_secs_f64());
    if args.len() > 4 {
        std::fs::write(&args[4], out).unwrap();
    }
}

// Diagnostic caller policy: advise owned storage before the first page fault.
// This is intentionally separate from the unchanged std::fs::read baseline.
fn read_input(path: &str) -> Vec<u8> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).unwrap();
    let len = file.metadata().unwrap().len() as usize;
    let mut data: Vec<u8> = Vec::with_capacity(len);
    let start = (data.as_ptr().addr() + (1 << 21) - 1) & !((1 << 21) - 1);
    let end = (data.as_ptr().addr() + data.capacity()) & !((1 << 21) - 1);
    unsafe extern "C" {
        fn madvise(addr: *mut core::ffi::c_void, length: usize, advice: i32) -> i32;
    }
    if end > start {
        // SAFETY: the aligned range lies entirely inside data's allocation;
        // MADV_HUGEPAGE changes policy without changing bytes or ownership.
        assert_eq!(
            unsafe { madvise(start as *mut core::ffi::c_void, end - start, 14) },
            0
        );
    }
    data.resize(len, 0);
    file.read_exact(&mut data).unwrap();
    data
}
