use tokio::io::AsyncReadExt;

/// Ring buffer that captures the last `max_bytes` of an async reader.
///
/// Used to capture stdout/stderr tails without unbounded memory growth.
pub struct TailBuffer {
    buf: Vec<u8>,
    max_bytes: usize,
}

impl TailBuffer {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            buf: Vec::with_capacity(max_bytes.min(8192)),
            max_bytes,
        }
    }

    /// Read from `reader` until EOF, keeping only the last `max_bytes`.
    pub async fn capture<R: tokio::io::AsyncRead + Unpin>(
        &mut self,
        mut reader: R,
    ) -> std::io::Result<()> {
        let mut chunk = [0u8; 4096];
        loop {
            let n = reader.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            self.push(&chunk[..n]);
        }
        Ok(())
    }

    pub(crate) fn push(&mut self, data: &[u8]) {
        if self.buf.len() + data.len() <= self.max_bytes {
            self.buf.extend_from_slice(data);
        } else if data.len() >= self.max_bytes {
            // New data alone exceeds max — just keep the tail of the new data
            self.buf.clear();
            self.buf
                .extend_from_slice(&data[data.len() - self.max_bytes..]);
        } else {
            // Drop enough from the front to fit
            let total = self.buf.len() + data.len();
            let drop = total - self.max_bytes;
            self.buf.drain(..drop);
            self.buf.extend_from_slice(data);
        }
    }

    /// Consume and return the captured tail as a UTF-8 string (lossy).
    pub fn into_string(self) -> Option<String> {
        if self.buf.is_empty() {
            None
        } else {
            Some(String::from_utf8_lossy(&self.buf).into_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_input_kept_fully() {
        let mut buf = TailBuffer::new(100);
        buf.push(b"hello");
        assert_eq!(buf.into_string().unwrap(), "hello");
    }

    #[test]
    fn overflow_keeps_tail() {
        let mut buf = TailBuffer::new(5);
        buf.push(b"hello world");
        assert_eq!(buf.into_string().unwrap(), "world");
    }

    #[test]
    fn incremental_overflow() {
        let mut buf = TailBuffer::new(5);
        buf.push(b"hel");
        buf.push(b"lo world");
        assert_eq!(buf.into_string().unwrap(), "world");
    }

    #[test]
    fn empty_returns_none() {
        let buf = TailBuffer::new(100);
        assert!(buf.into_string().is_none());
    }
}
