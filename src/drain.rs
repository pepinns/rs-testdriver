//! Keeping a child process's output pipes empty.
//!
//! A child that writes to a pipe nobody is reading blocks as soon as the pipe is full (64 KiB by
//! default on Linux, less under some container runtimes). A test server that logs to stdout then
//! freezes in the middle of a test, with nothing to say why. So every piped stream has to be read
//! until the child closes it, whatever the child prints: these readers never stop early because of
//! bad input, and never fail on bytes that are not UTF-8.

use std::io::{self, Read};

use futures::{AsyncRead, AsyncReadExt};

/// How much is read from the pipe at a time.
const CHUNK: usize = 8 * 1024;

/// The longest line held in memory. A child that prints this much without a newline has it handed
/// on in pieces of this size, rather than buffered without limit.
const MAX_LINE: usize = 16 * 1024;

/// Turns the bytes read from a pipe into lines.
#[derive(Default)]
struct Lines {
    pending: Vec<u8>,
}

impl Lines {
    fn push(&mut self, mut data: &[u8], emit: &mut impl FnMut(&str)) {
        while let Some(newline) = data.iter().position(|&b| b == b'\n') {
            self.pending.extend_from_slice(&data[..newline]);
            self.emit_pending(emit);
            data = &data[newline + 1..];
        }
        self.pending.extend_from_slice(data);

        while self.pending.len() >= MAX_LINE {
            let rest = self.pending.split_off(MAX_LINE);
            self.emit_pending(emit);
            self.pending = rest;
        }
    }

    /// The child closed the pipe: whatever it printed after its last newline is a line too.
    fn finish(&mut self, emit: &mut impl FnMut(&str)) {
        if !self.pending.is_empty() {
            self.emit_pending(emit);
        }
    }

    fn emit_pending(&mut self, emit: &mut impl FnMut(&str)) {
        // Lossy: a stray non-UTF-8 byte is shown as U+FFFD, it does not stop the reading.
        let line = String::from_utf8_lossy(&self.pending);
        emit(line.trim_end_matches('\r'));
        self.pending.clear();
    }
}

/// Read `reader` until the child closes it, handing each line to `emit`.
///
/// Returns the I/O error that ended the reading early, if any; the lines read before it have
/// already been emitted.
pub(crate) async fn drain<R: AsyncRead + Unpin>(
    mut reader: R,
    mut emit: impl FnMut(&str),
) -> io::Result<()> {
    let mut lines = Lines::default();
    let mut chunk = [0u8; CHUNK];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => lines.push(&chunk[..n], &mut emit),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => {
                lines.finish(&mut emit);
                return Err(e);
            }
        }
    }
    lines.finish(&mut emit);
    Ok(())
}

/// [`drain`] for a blocking reader, meant to run on a thread of its own.
pub(crate) fn drain_blocking<R: Read>(mut reader: R, mut emit: impl FnMut(&str)) -> io::Result<()> {
    let mut lines = Lines::default();
    let mut chunk = [0u8; CHUNK];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => lines.push(&chunk[..n], &mut emit),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => {
                lines.finish(&mut emit);
                return Err(e);
            }
        }
    }
    lines.finish(&mut emit);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hands out `data` a few bytes at a time, as a pipe does.
    struct Trickle<'a> {
        data: &'a [u8],
        step: usize,
    }

    impl Read for Trickle<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = self.step.min(self.data.len()).min(buf.len());
            buf[..n].copy_from_slice(&self.data[..n]);
            self.data = &self.data[n..];
            Ok(n)
        }
    }

    fn lines_of(data: &[u8], step: usize) -> Vec<String> {
        let mut out = Vec::new();
        drain_blocking(Trickle { data, step }, |l| out.push(l.to_string())).unwrap();
        out
    }

    #[test]
    fn it_splits_lines_however_the_bytes_arrive() {
        for step in [1, 2, 3, 7, 4096] {
            assert_eq!(lines_of(b"one\ntwo\n\nthree\n", step), ["one", "two", "", "three"]);
        }
    }

    #[test]
    fn it_emits_a_last_line_with_no_newline() {
        assert_eq!(lines_of(b"one\nlast", 3), ["one", "last"]);
    }

    #[test]
    fn it_emits_nothing_for_no_output() {
        assert!(lines_of(b"", 3).is_empty());
    }

    #[test]
    fn it_drops_the_carriage_return_of_crlf() {
        assert_eq!(lines_of(b"one\r\ntwo\r\n", 2), ["one", "two"]);
    }

    #[test]
    fn it_reads_past_bytes_that_are_not_utf8() {
        assert_eq!(
            lines_of(b"before\n\xff\xfe bad\nafter\n", 3),
            ["before", "\u{fffd}\u{fffd} bad", "after"]
        );
    }

    #[test]
    fn it_hands_on_a_very_long_line_in_pieces() {
        let data = vec![b'x'; MAX_LINE * 2 + 5];
        let lines = lines_of(&data, 1000);
        assert_eq!(
            lines.iter().map(String::len).collect::<Vec<_>>(),
            [MAX_LINE, MAX_LINE, 5]
        );
    }

    #[test]
    fn it_reports_the_error_that_ended_the_reading_after_what_it_had_read() {
        struct Fails(bool);
        impl Read for Fails {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.0 {
                    return Err(io::Error::other("boom"));
                }
                self.0 = true;
                buf[..7].copy_from_slice(b"a\nb\ncde");
                Ok(7)
            }
        }
        let mut out = Vec::new();
        let err = drain_blocking(Fails(false), |l| out.push(l.to_string())).unwrap_err();
        assert_eq!(err.to_string(), "boom");
        assert_eq!(out, ["a", "b", "cde"]);
    }

    #[tokio::test]
    async fn the_async_reader_matches_the_blocking_one() {
        let data = b"one\n\xff\xfe\nlast";
        let mut out = Vec::new();
        drain(futures::io::Cursor::new(&data[..]), |l| out.push(l.to_string()))
            .await
            .unwrap();
        assert_eq!(out, lines_of(data, 4096));
    }
}
