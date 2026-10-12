//! File-to-file SDK-level-5 encoder probe.
//! Arguments: 2 total-threads input [raw-output]; output defaults to /dev/null.
//! Measure process wall, user + system CPU time, and peak RSS externally.
use lzma_turbo::{BLOCK_SIZE_AUTO, Error, Lzma2Encoder, LzmaEncProps, SeqInStream, SeqOutStream};
use std::{
    fs::File,
    io::{Read, Write},
};

struct Input(File);
impl SeqInStream for Input {
    fn read(&mut self, data: &mut [u8]) -> Result<usize, Error> {
        self.0.read(data).map_err(|_| Error::Read)
    }
}

struct Output {
    file: File,
    bytes: u64,
}
impl SeqOutStream for Output {
    fn write(&mut self, data: &[u8]) -> Result<(), Error> {
        self.file.write_all(data).map_err(|_| Error::Write)?;
        self.bytes += data.len() as u64;
        Ok(())
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    assert_eq!(args[1], "2");
    let threads: u32 = args[2].parse().unwrap();
    let source = File::open(&args[3]).unwrap();
    let len = source.metadata().unwrap().len();
    let mut input = Input(source);
    let mut output = Output {
        file: File::create(args.get(4).map_or("/dev/null", String::as_str)).unwrap(),
        bytes: 0,
    };
    let props = LzmaEncProps::new()
        .with_level(5)
        .with_num_threads(threads.min(2));
    let now = std::time::Instant::now();
    let mut encoder = Lzma2Encoder::new(&props).unwrap();
    encoder.set_total_threads(threads as usize);
    encoder.set_block_size(BLOCK_SIZE_AUTO);
    encoder.set_data_size(len);
    encoder.set_data_limit(len);
    encoder.encode_mt(&mut input, &mut output).unwrap();
    println!("{} {:.9}", output.bytes, now.elapsed().as_secs_f64());
}
