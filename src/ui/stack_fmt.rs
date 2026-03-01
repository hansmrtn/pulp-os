// No-alloc fmt::Write buffers.
// StackFmt<N> owns a [u8; N]; BorrowedFmt wraps &mut [u8].
// Both silently truncate on overflow.

pub struct StackFmt<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> StackFmt<N> {
    pub const fn new() -> Self {
        Self {
            buf: [0u8; N],
            len: 0,
        }
    }

    #[inline]
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }

    #[inline]
    pub fn clear(&mut self) {
        self.len = 0;
    }
}

impl<const N: usize> core::fmt::Write for StackFmt<N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let n = bytes.len().min(N - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&bytes[..n]);
        self.len += n;
        Ok(())
    }
}

pub struct BorrowedFmt<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl<'a> BorrowedFmt<'a> {
    #[inline]
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    #[inline]
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.pos]).unwrap_or("")
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.pos
    }
}

impl core::fmt::Write for BorrowedFmt<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let room = self.buf.len() - self.pos;
        let n = bytes.len().min(room);
        self.buf[self.pos..self.pos + n].copy_from_slice(&bytes[..n]);
        self.pos += n;
        Ok(())
    }
}

// format into a borrowed slice via closure; returns bytes written
#[inline]
pub fn stack_fmt(buf: &mut [u8], f: impl FnOnce(&mut BorrowedFmt<'_>)) -> usize {
    let mut w = BorrowedFmt::new(buf);
    f(&mut w);
    w.pos
}
