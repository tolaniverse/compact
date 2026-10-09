//! How outside agents talk to Compact.
//!
//! Two programs share this crate. `compact-mcp` runs as an agent's MCP server
//! over stdio and relays each tool call to the running app; the app listens
//! on a Unix socket and answers. This crate holds what both sides must agree
//! on:
//!
//! - [`paths`]: where the socket lives.
//! - [`wire`]: the line protocol over it — one JSON object per line.
//! - [`tools`]: the tools an agent is offered, with their schemas.
//! - [`server`]: the app's side of the socket.
//!
//! **The trust boundary is the socket file.** It is created mode 0600 in your
//! own Application Support folder, so only processes running as you can
//! connect. Approval on first contact is consent, not authentication: any
//! program running as you could claim to be "Claude Code", and one that can
//! do that can already read your files. What approval adds is that nothing
//! gets a workspace without you seeing it ask.

pub mod paths;
pub mod tools;
pub mod wire;

#[cfg(unix)]
pub mod server;

/// Standard base64 with padding, for PNGs in tool results. Small enough to
/// keep rather than take a dependency for.
pub fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(super::base64(b""), "");
        assert_eq!(super::base64(b"f"), "Zg==");
        assert_eq!(super::base64(b"fo"), "Zm8=");
        assert_eq!(super::base64(b"foo"), "Zm9v");
        assert_eq!(super::base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(super::base64(&[0xff, 0xfe]), "//4=");
    }
}
