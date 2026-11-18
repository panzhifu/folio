//! 不可信容器的实际产出预算；与 ParseBudget 的并行度无关。

use std::io::{self, Read, Write};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

pub const MAX_ENTRY_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_ARCHIVE_BYTES: u64 = 1024 * 1024 * 1024;
pub const MAX_ARCHIVE_ENTRIES: usize = 100_000;
pub const MAX_ARCHIVE_DIRECTORY_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_DOM_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_DOM_NODES: usize = 250_000;
pub const MAX_DOM_DEPTH: usize = 256;
pub const MAX_IMAGE_PIXELS: u64 = 32 * 1024 * 1024;
pub const MAX_CSS_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_CSS_DEPTH: usize = 64;
pub const MAX_CSS_BLOCKS: usize = 20_000;
pub const MAX_CSS_DECLARATIONS: usize = 100_000;

pub fn image_dimensions_allowed(width: u32, height: u32) -> bool {
    width > 0
        && height > 0
        && width <= 16_384
        && height <= 16_384
        && u64::from(width) * u64::from(height) <= MAX_IMAGE_PIXELS
}

pub fn exceeded() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "内容超过读取预算")
}

pub fn read_bounded(reader: &mut dyn Read, max: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(max.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(exceeded());
    }
    Ok(bytes)
}

/// 所有解码输出（包括丢弃的 solid 前置条目）共用预算。
#[derive(Clone)]
pub struct OutputBudget(Arc<AtomicU64>);

impl Default for OutputBudget {
    fn default() -> Self {
        Self(Arc::new(AtomicU64::new(MAX_ARCHIVE_BYTES)))
    }
}

impl OutputBudget {
    pub fn writer<W: Write>(&self, inner: W) -> BoundedWriter<W> {
        BoundedWriter {
            inner,
            entry_left: MAX_ENTRY_BYTES,
            total: self.0.clone(),
        }
    }
}

pub struct BoundedWriter<W> {
    inner: W,
    entry_left: u64,
    total: Arc<AtomicU64>,
}

impl<W: Write> Write for BoundedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let len = bytes.len() as u64;
        if len > self.entry_left {
            return Err(exceeded());
        }
        let mut left = self.total.load(Ordering::Relaxed);
        loop {
            let Some(next) = left.checked_sub(len) else {
                return Err(exceeded());
            };
            match self
                .total
                .compare_exchange_weak(left, next, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => break,
                Err(observed) => left = observed,
            }
        }
        match self.inner.write(bytes) {
            Ok(written) => {
                self.entry_left -= written as u64;
                self.total
                    .fetch_add(len - written as u64, Ordering::Relaxed);
                Ok(written)
            }
            Err(error) => {
                self.total.fetch_add(len, Ordering::Relaxed);
                Err(error)
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use crate::limits::{MAX_ENTRY_BYTES, OutputBudget, image_dimensions_allowed, read_bounded};
    use std::io::{Cursor, Write};

    #[test]
    fn actual_output_is_bounded_even_without_a_declared_size() {
        assert_eq!(read_bounded(&mut Cursor::new(b"1234"), 4).unwrap(), b"1234");
        assert!(read_bounded(&mut Cursor::new(b"12345"), 4).is_err());
        let budget = OutputBudget::default();
        let mut writer = budget.writer(std::io::sink());
        let block = [0; 8192];
        for _ in 0..MAX_ENTRY_BYTES / block.len() as u64 {
            writer.write_all(&block).unwrap();
        }
        assert!(writer.write_all(b"x").is_err());
    }

    #[test]
    fn image_budget_checks_both_axes_and_pixel_product() {
        assert!(image_dimensions_allowed(4096, 4096));
        for (w, h) in [
            (0, 1),
            (1, 0),
            (16_385, 1),
            (1, 16_385),
            (8192, 8192),
            (u32::MAX, u32::MAX),
        ] {
            assert!(!image_dimensions_allowed(w, h));
        }
    }
}
