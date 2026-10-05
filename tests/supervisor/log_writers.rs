#[cfg(unix)]
pub(crate) struct BrokenLog(pub(crate) std::fs::File);

#[cfg(unix)]
impl std::io::Write for BrokenLog {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("injected log write failure"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::Write::flush(&mut self.0)
    }
}
