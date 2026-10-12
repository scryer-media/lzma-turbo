//! SDK level-5 encoder probe used by docs/encoder-perf-0.9.1.md.
//! Arguments: 1|2 total-threads input [raw-output].
//! The optional output write is outside the printed encoding time.
use lzma_turbo::{BLOCK_SIZE_AUTO, Lzma2Encoder, LzmaEncProps, LzmaEncoder};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let data = std::fs::read(&args[3]).unwrap();
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
