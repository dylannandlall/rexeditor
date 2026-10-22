use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;

use crate::buffer::ByteBuffer;

const READ_CHUNK: usize = 1024 * 1024;
const MAX_BUCKETS: usize = 256;

#[derive(Clone, Debug)]
pub struct EntropyReport {
    pub buckets: Vec<f64>,
    pub histogram: [u64; 256],
    pub average: f64,
    pub bucket_size: usize,
    pub min: f64,
    pub max: f64,
    pub peak_offset: usize,
}

impl EntropyReport {
    pub fn offset_for_bucket(&self, bucket_index: usize, file_len: usize) -> usize {
        if self.buckets.is_empty() || file_len == 0 {
            return 0;
        }
        let bsize = if self.bucket_size > 0 {
            self.bucket_size
        } else {
            file_len.div_ceil(self.buckets.len())
        };
        (bucket_index * bsize).min(file_len.saturating_sub(1))
    }

    pub fn bucket_for_offset(&self, offset: usize) -> usize {
        if self.buckets.is_empty() || self.bucket_size == 0 {
            return 0;
        }
        (offset / self.bucket_size).min(self.buckets.len() - 1)
    }
}

#[derive(Debug)]
pub enum EntropyMessage {
    Progress(usize),
    Done(EntropyReport),
}

pub struct EntropyWorker {
    pub receiver: Receiver<EntropyMessage>,
    cancel: Arc<AtomicBool>,
}

impl EntropyWorker {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

fn shannon(counts: &[u64; 256], total: u64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    let t = total as f64;
    let mut e = 0.0;
    for &c in counts.iter() {
        if c > 0 {
            let p = c as f64 / t;
            e -= p * p.log2();
        }
    }
    e.clamp(0.0, 8.0)
}

fn compute(
    buffer: &ByteBuffer,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(usize),
) -> Option<EntropyReport> {
    let len = buffer.len();
    let mut histogram = [0u64; 256];
    if len == 0 {
        return Some(EntropyReport {
            buckets: Vec::new(),
            histogram,
            average: 0.0,
            bucket_size: 0,
            min: 0.0,
            max: 0.0,
            peak_offset: 0,
        });
    }
    let bucket_size = len.div_ceil(MAX_BUCKETS).max(1);
    let bucket_count = len.div_ceil(bucket_size);
    let contiguous = buffer.as_contiguous_slice();
    let mut buckets = Vec::with_capacity(bucket_count);
    let mut tmp: Vec<u8> = Vec::new();
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    let mut peak_offset = 0usize;

    for i in 0..bucket_count {
        let start = i * bucket_size;
        let end = (start + bucket_size).min(len);
        let mut counts = [0u64; 256];
        let mut pos = start;
        while pos < end {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            let n = (end - pos).min(READ_CHUNK);
            let data: &[u8] = match contiguous {
                Some(s) => &s[pos..pos + n],
                None => {
                    tmp.resize(n, 0);
                    let got = buffer.copy_range(pos, &mut tmp);
                    &tmp[..got]
                }
            };
            for &b in data {
                counts[b as usize] += 1;
            }
            if data.is_empty() {
                break;
            }
            pos += data.len();
        }
        let total: u64 = counts.iter().sum();
        let e = shannon(&counts, total);
        if e < min {
            min = e;
        }
        if e > max {
            max = e;
            peak_offset = start;
        }
        buckets.push(e);
        for (h, c) in histogram.iter_mut().zip(counts.iter()) {
            *h += *c;
        }
        progress(end);
    }

    let total: u64 = histogram.iter().sum();
    let average = shannon(&histogram, total);
    Some(EntropyReport {
        buckets,
        histogram,
        average,
        bucket_size,
        min: if min.is_finite() { min } else { 0.0 },
        max: if max.is_finite() { max } else { 0.0 },
        peak_offset,
    })
}

pub fn spawn(buffer: ByteBuffer) -> EntropyWorker {
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_t = Arc::clone(&cancel);
    thread::spawn(move || {
        let mut progress = |n: usize| {
            let _ = tx.send(EntropyMessage::Progress(n));
        };
        if let Some(report) = compute(&buffer, &cancel_t, &mut progress) {
            let _ = tx.send(EntropyMessage::Done(report));
        }
    });
    EntropyWorker {
        receiver: rx,
        cancel,
    }
}

pub fn calculate(buffer: &ByteBuffer) -> EntropyReport {
    let cancel = AtomicBool::new(false);
    compute(buffer, &cancel, &mut |_| {}).expect("calculation cannot be cancelled")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calculates_entropy_and_histogram() {
        let r = calculate(&ByteBuffer::from_bytes(vec![7u8; 1000]));
        assert_eq!(r.average, 0.0);
        assert_eq!(r.histogram[7], 1000);

        let data: Vec<u8> = (0..=255u8).cycle().take(256 * 16).collect();
        let r2 = calculate(&ByteBuffer::from_bytes(data));
        assert!((r2.average - 8.0).abs() < 1e-9);
    }

    #[test]
    fn metadata_and_bucket_mapping() {
        // 256 buckets of 16 bytes each (total 4096 bytes)
        let mut data = vec![0u8; 4096];
        // Make bucket 10 (offset 160..176) have maximum diversity (16 distinct bytes -> 4.0 bits)
        for (i, b) in data[160..176].iter_mut().enumerate() {
            *b = i as u8;
        }
        let r = calculate(&ByteBuffer::from_bytes(data));
        assert_eq!(r.bucket_size, 16);
        assert_eq!(r.min, 0.0);
        assert!((r.max - 4.0).abs() < 1e-9);
        assert_eq!(r.peak_offset, 160);
        assert_eq!(r.offset_for_bucket(10, 4096), 160);
        assert_eq!(r.bucket_for_offset(165), 10);
    }
}
