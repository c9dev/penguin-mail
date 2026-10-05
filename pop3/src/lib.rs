//! A POP3 client (RFC 1939, CAPA from RFC 2449, STLS from RFC 2595, SASL
//! PLAIN from RFC 5034) on tokio and rustls, behind [`Pop3Api`]. One
//! session runs from `connect` to `quit`, as the protocol has it: message
//! numbers hold for one session only, and `DELE` takes effect at a clean
//! `QUIT`. TLS is required, from the first byte or after `STLS`, and the
//! password never goes out before it.

mod client;
mod error;
mod tls;
mod wire;

pub use client::{
    Capabilities, Connect, Io, ListItem, Login, Pop3Api, Pop3Client, Pop3Tls, Stat, Stream, Uidl,
};
pub use error::Pop3Error;

/// The most bytes one message or one listing may take. A message over it
/// is never asked for: the downloader reads `LIST` first. 64 MiB is past
/// what any provider in the table accepts in one message.
pub const MOST_MESSAGE_BYTES: u64 = 64 << 20;
