// fixed-capacity, heapless formatting buffer for `write!`.
pub(crate) struct FmtBuf<const N: usize> {
    buf: [u8; N],
    pos: usize,
}

impl<const N: usize> FmtBuf<N> {
    pub(crate) fn new() -> Self {
        Self {
            buf: [0; N],
            pos: 0,
        }
    }

    #[cfg(feature = "gps")]
    pub(crate) fn reset(&mut self) {
        self.pos = 0;
    }

    pub(crate) fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.pos]).unwrap_or("")
    }
}

impl<const N: usize> core::fmt::Write for FmtBuf<N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let len = bytes.len().min(N - self.pos);
        self.buf[self.pos..self.pos + len].copy_from_slice(&bytes[..len]);
        self.pos += len;
        Ok(())
    }
}
