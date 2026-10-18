//! The session server's frames (DESIGN.md §14.4): a `u32` length, a `u8` kind, then the
//! payload, all little-endian. Hand-written, so the protocol needs no dependency.

use std::{
    ffi::OsString,
    io::{self, Read, Write},
    os::unix::{ffi::OsStringExt, process::ExitStatusExt},
    path::PathBuf,
    process::ExitStatus,
};

use crate::session::{GridSize, Program, SessionConfig};

/// Bumped only when frames change incompatibly: a mismatch costs every running job
/// (DESIGN.md §14.3).
pub(crate) const PROTOCOL: u32 = 1;
/// A frame larger than this is a corrupt or hostile stream.
const MAX_FRAME: usize = 16 << 20;
/// Output is forwarded in chunks of at most this many bytes.
pub(crate) const OUTPUT_CHUNK: usize = 64 << 10;

/// Where a session's process lives, so the client can query it like a local one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Identity {
    pub(crate) pid: i32,
    pub(crate) start_sec: u64,
    pub(crate) start_usec: u64,
    pub(crate) tty: u32,
    /// The server's grid when the session was attached: the state stream is laid out for it.
    pub(crate) size: GridSize,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Frame {
    /// The first frame on every connection, both ways.
    Hello {
        protocol: u32,
    },
    // Client to server.
    List,
    StopAll,
    Spawn {
        id: String,
        config: SessionConfig,
    },
    Attach {
        id: String,
    },
    Input(Vec<u8>),
    Resize(GridSize),
    /// Ends the session (hangup, then the reaper), unlike closing the connection.
    Close,
    // Server to client.
    Sessions(Vec<String>),
    Stopped,
    /// The connection now carries this session: output follows.
    Attached(Identity),
    Missing,
    Failed(String),
    Output(Vec<u8>),
    Exited(ExitStatus),
}

impl Frame {
    fn kind(&self) -> u8 {
        match self {
            Self::Hello { .. } => 1,
            Self::List => 2,
            Self::StopAll => 3,
            Self::Spawn { .. } => 4,
            Self::Attach { .. } => 5,
            Self::Input(_) => 6,
            Self::Resize(_) => 7,
            Self::Close => 8,
            Self::Sessions(_) => 9,
            Self::Stopped => 10,
            Self::Attached(_) => 11,
            Self::Missing => 12,
            Self::Failed(_) => 13,
            Self::Output(_) => OUTPUT,
            Self::Exited(_) => 15,
        }
    }

    /// Appends the encoded frame to `out`.
    pub(crate) fn encode(&self, out: &mut Vec<u8>) {
        let start = out.len();
        out.extend_from_slice(&[0; 4]);
        out.push(self.kind());
        let mut put = Put(out);
        match self {
            Self::Hello { protocol } => put.u32(*protocol),
            Self::List | Self::StopAll | Self::Close | Self::Stopped | Self::Missing => {}
            Self::Spawn { id, config } => {
                put.str(id);
                put.config(config);
            }
            Self::Attach { id } => put.str(id),
            Self::Input(bytes) | Self::Output(bytes) => put.0.extend_from_slice(bytes),
            Self::Resize(size) => put.size(*size),
            Self::Sessions(ids) => {
                put.len(ids.len());
                for id in ids {
                    put.str(id);
                }
            }
            Self::Attached(identity) => {
                put.u32(identity.pid.cast_unsigned());
                put.u64(identity.start_sec);
                put.u64(identity.start_usec);
                put.u32(identity.tty);
                put.size(identity.size);
            }
            Self::Failed(message) => put.str(message),
            Self::Exited(status) => put.u32(status.into_raw().cast_unsigned()),
        }
        let len = u32::try_from(out.len() - start - 4).unwrap_or(u32::MAX);
        out[start..start + 4].copy_from_slice(&len.to_le_bytes());
    }

    pub(crate) fn decode(kind: u8, payload: &[u8]) -> io::Result<Self> {
        let mut get = Get(payload);
        let frame = match kind {
            1 => Self::Hello {
                protocol: get.u32()?,
            },
            2 => Self::List,
            3 => Self::StopAll,
            4 => Self::Spawn {
                id: get.str()?,
                config: get.config()?,
            },
            5 => Self::Attach { id: get.str()? },
            6 => Self::Input(get.rest()),
            7 => Self::Resize(get.size()?),
            8 => Self::Close,
            9 => {
                let count = get.len()?;
                Self::Sessions((0..count).map(|_| get.str()).collect::<io::Result<_>>()?)
            }
            10 => Self::Stopped,
            11 => Self::Attached(Identity {
                pid: get.u32()?.cast_signed(),
                start_sec: get.u64()?,
                start_usec: get.u64()?,
                tty: get.u32()?,
                size: get.size()?,
            }),
            12 => Self::Missing,
            13 => Self::Failed(get.str()?),
            OUTPUT => Self::Output(get.rest()),
            15 => Self::Exited(ExitStatus::from_raw(get.u32()?.cast_signed())),
            _ => return Err(invalid("unknown frame kind")),
        };
        Ok(frame)
    }
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_owned())
}

struct Put<'a>(&'a mut Vec<u8>);

impl Put<'_> {
    fn u16(&mut self, value: u16) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    fn len(&mut self, len: usize) {
        self.u32(u32::try_from(len).unwrap_or(u32::MAX));
    }

    fn bytes(&mut self, bytes: &[u8]) {
        self.len(bytes.len());
        self.0.extend_from_slice(bytes);
    }

    fn str(&mut self, text: &str) {
        self.bytes(text.as_bytes());
    }

    fn os(&mut self, text: &OsString) {
        self.bytes(text.as_encoded_bytes());
    }

    fn size(&mut self, size: GridSize) {
        self.u16(size.rows);
        self.u16(size.cols);
        self.u16(size.cell_width);
        self.u16(size.cell_height);
    }

    fn config(&mut self, config: &SessionConfig) {
        match &config.program {
            Program::LoginShell => self.0.push(0),
            Program::Command { program, args } => {
                self.0.push(1);
                self.os(&program.clone().into_os_string());
                self.len(args.len());
                for arg in args {
                    self.os(arg);
                }
            }
        }
        match &config.cwd {
            Some(cwd) => {
                self.0.push(1);
                self.os(&cwd.clone().into_os_string());
            }
            None => self.0.push(0),
        }
        self.size(config.size);
        self.u64(config.scrollback_lines as u64);
        self.len(config.env.len());
        for (key, value) in &config.env {
            self.os(key);
            self.os(value);
        }
        self.bytes(&config.replay);
    }
}

struct Get<'a>(&'a [u8]);

impl Get<'_> {
    fn take(&mut self, count: usize) -> io::Result<&[u8]> {
        if self.0.len() < count {
            return Err(invalid("truncated frame"));
        }
        let (head, tail) = self.0.split_at(count);
        self.0 = tail;
        Ok(head)
    }

    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().map_err(|_| invalid("u16"))?,
        ))
    }

    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().map_err(|_| invalid("u32"))?,
        ))
    }

    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().map_err(|_| invalid("u64"))?,
        ))
    }

    fn len(&mut self) -> io::Result<usize> {
        usize::try_from(self.u32()?).map_err(|_| invalid("length"))
    }

    fn bytes(&mut self) -> io::Result<Vec<u8>> {
        let len = self.len()?;
        Ok(self.take(len)?.to_vec())
    }

    fn str(&mut self) -> io::Result<String> {
        String::from_utf8(self.bytes()?).map_err(|_| invalid("not UTF-8"))
    }

    fn os(&mut self) -> io::Result<OsString> {
        Ok(OsString::from_vec(self.bytes()?))
    }

    fn rest(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.0).to_vec()
    }

    fn size(&mut self) -> io::Result<GridSize> {
        Ok(GridSize {
            rows: self.u16()?,
            cols: self.u16()?,
            cell_width: self.u16()?,
            cell_height: self.u16()?,
        })
    }

    fn config(&mut self) -> io::Result<SessionConfig> {
        let program = match self.u8()? {
            0 => Program::LoginShell,
            1 => {
                let program = PathBuf::from(self.os()?);
                let count = self.len()?;
                let args = (0..count).map(|_| self.os()).collect::<io::Result<_>>()?;
                Program::Command { program, args }
            }
            _ => return Err(invalid("program")),
        };
        let cwd = match self.u8()? {
            0 => None,
            _ => Some(PathBuf::from(self.os()?)),
        };
        let size = self.size()?;
        let scrollback_lines = usize::try_from(self.u64()?).map_err(|_| invalid("scrollback"))?;
        let count = self.len()?;
        let env = (0..count)
            .map(|_| Ok((self.os()?, self.os()?)))
            .collect::<io::Result<_>>()?;
        let replay = self.bytes()?;
        let mut config = SessionConfig::new(size);
        config.program = program;
        config.cwd = cwd;
        config.scrollback_lines = scrollback_lines;
        config.env = env;
        config.replay = replay;
        Ok(config)
    }
}

/// Collects bytes from a non-blocking stream and cuts them into frames.
#[derive(Default)]
pub(crate) struct Inbox {
    buf: Vec<u8>,
    /// Where the next frame starts: frames are taken without moving what follows them.
    start: usize,
}

impl Inbox {
    /// Reads what `stream` has. `Ok(false)` once it is closed.
    pub(crate) fn fill(&mut self, stream: &mut impl Read) -> io::Result<bool> {
        // What is left is at most one partial frame.
        self.buf.drain(..self.start);
        self.start = 0;
        loop {
            // Read straight into the buffer's tail.
            let filled = self.buf.len();
            self.buf.resize(filled + OUTPUT_CHUNK, 0);
            let result = stream.read(&mut self.buf[filled..]);
            self.buf.truncate(filled + *result.as_ref().unwrap_or(&0));
            match result {
                Ok(0) => return Ok(false),
                Ok(_) => {}
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(true),
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err),
            }
        }
    }

    /// The next complete frame, if one has arrived.
    pub(crate) fn next(&mut self) -> io::Result<Option<Frame>> {
        match self.next_raw()? {
            Some((kind, payload)) => Frame::decode(kind, payload).map(Some),
            None => Ok(None),
        }
    }

    /// The next complete frame's kind and payload, borrowed: output is parsed straight
    /// from here, without a copy.
    pub(crate) fn next_raw(&mut self) -> io::Result<Option<(u8, &[u8])>> {
        let Some(header) = self.buf.get(self.start..self.start + 5) else {
            return Ok(None);
        };
        let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
        if len == 0 || len > MAX_FRAME {
            return Err(invalid("frame length"));
        }
        let end = self.start + 4 + len;
        if self.buf.len() < end {
            return Ok(None);
        }
        let (kind, payload) = (self.buf[self.start + 4], self.start + 5..end);
        self.start = end;
        Ok(Some((kind, &self.buf[payload])))
    }
}

/// Kind of [`Frame::Output`], which the hot path encodes and decodes in place.
pub(crate) const OUTPUT: u8 = 14;

/// Appends an output frame for `bytes`, as `Frame::Output(bytes.to_vec()).encode` would.
pub(crate) fn encode_output(out: &mut Vec<u8>, bytes: &[u8]) {
    let len = u32::try_from(bytes.len() + 1).unwrap_or(u32::MAX);
    out.extend_from_slice(&len.to_le_bytes());
    out.push(OUTPUT);
    out.extend_from_slice(bytes);
}

/// Writes one frame to a blocking stream.
pub(crate) fn send(stream: &mut impl Write, frame: &Frame) -> io::Result<()> {
    let mut out = Vec::new();
    frame.encode(&mut out);
    stream.write_all(&out)
}

/// Reads one frame from a blocking stream; `None` if it closed first.
pub(crate) fn receive(stream: &mut impl Read) -> io::Result<Option<Frame>> {
    let mut header = [0; 4];
    match stream.read_exact(&mut header) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err),
    }
    let len = u32::from_le_bytes(header) as usize;
    if len == 0 || len > MAX_FRAME {
        return Err(invalid("frame length"));
    }
    let mut body = vec![0; len];
    stream.read_exact(&mut body)?;
    Frame::decode(body[0], &body[1..]).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(frame: &Frame) -> Frame {
        let mut out = Vec::new();
        frame.encode(&mut out);
        let mut inbox = Inbox { buf: out, start: 0 };
        inbox.next().unwrap().unwrap()
    }

    #[test]
    fn frames_should_round_trip() {
        let size = GridSize {
            rows: 30,
            cols: 100,
            cell_width: 8,
            cell_height: 16,
        };
        let mut config = SessionConfig::new(size);
        config.program = Program::Command {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "echo 中".into()],
        };
        config.cwd = Some("/tmp".into());
        config.env = vec![("A".into(), "b".into())];
        config.replay = b"\x1b[31mold".to_vec();
        let frames = [
            Frame::Hello { protocol: PROTOCOL },
            Frame::Spawn {
                id: "a-1".into(),
                config,
            },
            Frame::Sessions(vec!["a-1".into(), "b-2".into()]),
            Frame::Attached(Identity {
                pid: 42,
                start_sec: 1,
                start_usec: 2,
                tty: 3,
                size,
            }),
            Frame::Output(b"\x00\xff".to_vec()),
            Frame::Exited(ExitStatus::from_raw(3 << 8)),
            Frame::Close,
        ];
        for frame in frames {
            assert_eq!(round_trip(&frame), frame);
        }
    }

    #[test]
    fn encode_output_should_match_the_frame() {
        let (mut direct, mut framed) = (Vec::new(), Vec::new());
        encode_output(&mut direct, b"abc");
        Frame::Output(b"abc".to_vec()).encode(&mut framed);
        assert_eq!(direct, framed);
    }

    #[test]
    fn inbox_should_wait_for_a_whole_frame() {
        let mut out = Vec::new();
        Frame::Input(b"abc".to_vec()).encode(&mut out);
        let mut inbox = Inbox {
            buf: out[..out.len() - 1].to_vec(),
            start: 0,
        };
        assert_eq!(inbox.next().unwrap(), None);
    }
}
