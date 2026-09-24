//! Reads mail as it arrives from any server: RFC 822 bytes into the body,
//! the files and the headers the window shows, and the text helpers that
//! go with them (charsets, address lists, HTML to text).

pub mod address;
pub mod charset;
pub mod html;
pub mod provenance;
pub mod snippet;
