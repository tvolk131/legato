//! Legato wire protocol.
//!
//! A session between two paired machines is a single iroh connection with:
//! - one bidirectional **control stream** carrying length-prefixed [`Control`] frames
//!   (reliable, ordered: keys, buttons, scroll, enter/leave, hello), and
//! - unreliable **datagrams** carrying [`Datagram`]s (pointer motion, where only the
//!   newest position matters).
//!
//! All coordinates on the wire are in the *receiving* machine's native global
//! coordinate space (points on macOS, physical pixels on Windows).

use serde::{Deserialize, Serialize};

/// Bumped on incompatible wire changes.
pub const PROTOCOL_VERSION: u16 = 4;

/// ALPN for the input-sharing session. Only paired peers may use it.
pub const SESSION_ALPN: &[u8] = b"legato/1";

/// ALPN for pairing. Open to any peer on the network, gated by a user-confirmed code.
pub const PAIR_ALPN: &[u8] = b"legato/pair/1";

/// Largest control frame we accept, to bound memory use on hostile input.
pub const MAX_FRAME_LEN: usize = 64 * 1024;

/// Operating system of a peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Os {
    MacOs,
    Windows,
}

/// A 2D point in some coordinate space. Which space is documented at each use.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

/// An axis-aligned rectangle. `x`/`y` is the top-left corner; y grows downwards.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub const fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn left(&self) -> f64 {
        self.x
    }

    pub fn right(&self) -> f64 {
        self.x + self.width
    }

    pub fn top(&self) -> f64 {
        self.y
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.height
    }

    pub fn center(&self) -> Point {
        Point::new(self.x + self.width / 2.0, self.y + self.height / 2.0)
    }

    /// Half-open containment: the right and bottom edges are outside.
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.left() && p.x < self.right() && p.y >= self.top() && p.y < self.bottom()
    }
}

/// One physical display as the owning machine sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Display {
    /// Stable-ish identifier from the OS, for matching across reconnects.
    pub id: u32,
    /// Bounds in the machine's native global coordinates.
    pub bounds: Rect,
    /// Pixels per native unit (2.0 on Retina Macs, 1.0 on Windows where native units are pixels).
    pub pixel_scale: f64,
    /// UI scale of this display relative to 96 DPI on Windows (1.5 = 150%); 1.0 on macOS.
    pub ui_scale: f64,
    pub primary: bool,
    pub name: String,
}

/// A machine's display configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Screens {
    pub displays: Vec<Display>,
    /// Native units per desk unit. Desk units are the shared layout space in which all
    /// machines are arranged; they approximate the OS's logical (scaled) units so that
    /// pointer speed feels consistent across machines. On macOS this is 1.0 (native units
    /// are already points); on Windows it is the primary display's UI scale.
    pub native_per_desk: f64,
}

/// First frame each side sends on the control stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u16,
    pub app_version: String,
    pub name: String,
    pub os: Os,
    pub screens: Screens,
}

/// Mouse buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Button {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

/// Scroll amounts.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Scroll {
    /// Wheel movement in units where 120 is one notch (Windows convention; high-resolution
    /// wheels send fractions of a notch). Positive `y` scrolls up/away, positive `x` right.
    Wheel { x: f64, y: f64 },
    /// Continuous (trackpad) scrolling in native units of the sender.
    Pixels { x: f64, y: f64 },
}

/// Reliable, ordered messages on the control stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Control {
    Hello(Hello),
    /// The sender's displays changed.
    Screens(Screens),
    /// Controller → controlled: the shared cursor enters your screen at `pos`.
    /// Motion datagrams with a `seq` lower than `seq` belong to an earlier visit.
    Enter {
        seq: u32,
        pos: Point,
    },
    /// Controller → controlled: the cursor left. Release every key and button you pressed.
    Leave,
    /// A key, identified by its USB HID usage on the keyboard page (0x07).
    Key {
        usage: u16,
        down: bool,
    },
    /// A mouse button, with the cursor position so clicks land correctly even if the
    /// latest motion datagram was lost.
    Button {
        button: Button,
        down: bool,
        pos: Point,
    },
    Scroll(Scroll),
    /// Controlled → controller: physical input happened on this machine; give it its
    /// cursor back.
    Yield,
    /// Where the sender has placed the receiver on the shared desk: the receiver's desk
    /// origin in the sender's desk units, or `None` if not placed. The receiver mirrors it
    /// (placing the sender at the negated offset) unless it has its own arrangement.
    Placement {
        offset: Option<Point>,
    },
    /// The sender's control-mode setting, shared so every machine agrees. The newer one
    /// (by `updated_at`, seconds since the Unix epoch) wins.
    ControlMode(ControlMode),
    /// Virtual monitor mode, viewer → Mac: add a display of this many pixels and stream it
    /// to me.
    ExtendRequest(ExtendRequest),
    /// Mac → viewer: the display exists at `bounds` (the Mac's native coordinates);
    /// frames follow on a video stream.
    Extended {
        bounds: Rect,
    },
    /// Either side: stop showing the extra display. `reason` is shown if it isn't empty.
    ExtendStop {
        reason: String,
    },
    /// Viewer → Mac: send a keyframe (the viewer started, or lost its place).
    Keyframe,
}

/// The extra display a viewer asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtendRequest {
    pub width: u32,
    pub height: u32,
    /// Retina: the display looks like half its pixel size.
    pub hidpi: bool,
    pub fps: u32,
    /// Bits per second.
    pub bitrate: u32,
}

/// Which machines may drive the others.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlMode {
    /// `None`: whichever machine's keyboard and mouse is being used. `Some(id)`: only the
    /// machine with this endpoint id (as text).
    pub controller: Option<String>,
    pub updated_at: u64,
}

/// Tags for bulk transfers on their own unidirectional streams.
pub mod blob {
    /// A [`super::ClipboardContent`].
    pub const CLIPBOARD: u8 = 1;
    /// A file, as a [`super::FileHeader`] frame followed by its bytes.
    pub const FILE: u8 = 2;
    /// A stream of H.264 frames, each a [`super::VideoFrameHeader`] then its bytes.
    pub const VIDEO: u8 = 3;
}

/// Precedes each frame on a video stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoFrameHeader {
    pub len: u32,
    pub keyframe: bool,
    /// How long the sender spent on the frame, in microseconds: from the picture appearing
    /// on its display to the frame being sent (capture, encoding, queueing).
    pub sender_us: u32,
}

impl VideoFrameHeader {
    pub const SIZE: usize = 9;
    /// Larger frames are refused.
    pub const MAX_LEN: u32 = 32 * 1024 * 1024;

    pub fn encode(self) -> [u8; Self::SIZE] {
        let mut out = [0u8; Self::SIZE];
        out[..4].copy_from_slice(&self.len.to_le_bytes());
        out[4] = u8::from(self.keyframe);
        out[5..].copy_from_slice(&self.sender_us.to_le_bytes());
        out
    }

    pub fn decode(bytes: [u8; Self::SIZE]) -> Option<Self> {
        let len = u32::from_le_bytes(bytes[..4].try_into().ok()?);
        (len <= Self::MAX_LEN && bytes[4] <= 1).then_some(Self {
            len,
            keyframe: bytes[4] == 1,
            sender_us: u32::from_le_bytes(bytes[5..].try_into().ok()?),
        })
    }
}

/// Largest blob accepted in memory (clipboard contents).
pub const MAX_BLOB_LEN: u64 = 64 * 1024 * 1024;

/// Clipboard contents shared between machines.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ClipboardContent {
    Text(String),
    /// A PNG image.
    Image {
        png: Vec<u8>,
    },
}

/// Describes a file sent on a [`blob::FILE`] stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileHeader {
    /// Groups files sent together (one drop or paste).
    pub batch: u64,
    /// Path relative to the batch root, with `/` separators (folders keep their shape).
    pub name: String,
    pub size: u64,
    /// Files in the batch, and this one's position, for progress.
    pub count: u32,
    pub index: u32,
    /// What to do once the batch has arrived.
    pub purpose: FilePurpose,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilePurpose {
    /// Save to Downloads and tell the user.
    Send,
    /// Put the files on the clipboard (someone copied them).
    Clipboard,
    /// Dropped here after dragging across the edge: save to Downloads and reveal.
    Drop,
}

/// Unreliable messages sent as QUIC datagrams.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Datagram {
    /// Absolute cursor position. Newer `seq` wins; older ones are dropped.
    Motion { seq: u32, pos: Point },
}

/// Messages on the pairing stream (ALPN [`PAIR_ALPN`]).
///
/// Both sides send `Hello`, then show the user a short code derived from the TLS session
/// (see [`pairing_code`]) and send the user's `Decision`. The peers are paired only if
/// both accept.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Pair {
    Hello {
        protocol: u16,
        app_version: String,
        name: String,
        os: Os,
    },
    Decision {
        accept: bool,
    },
}

/// Label for deriving the pairing code with TLS keying-material export (RFC 5705).
pub const PAIRING_CODE_LABEL: &[u8] = b"legato pairing code v1";

/// Turns 4 bytes of exported keying material into a 6-digit code.
pub fn pairing_code(keying_material: [u8; 4]) -> u32 {
    u32::from_le_bytes(keying_material) % 1_000_000
}

/// Formats a pairing code for display, e.g. `042 917`.
pub fn format_pairing_code(code: u32) -> String {
    format!("{:03} {:03}", code / 1000, code % 1000)
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("frame of {0} bytes exceeds the {MAX_FRAME_LEN}-byte limit")]
    TooLarge(usize),
    #[error("malformed message: {0}")]
    Malformed(#[from] postcard::Error),
}

/// Encodes a stream frame: a little-endian `u32` length followed by the postcard payload.
pub fn encode_frame<T: Serialize>(msg: &T) -> Vec<u8> {
    let payload = postcard::to_stdvec(msg).expect("serializing to a Vec cannot fail");
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&payload);
    out
}

/// Validates a frame length read from a stream.
pub fn check_frame_len(len: u32) -> Result<usize, DecodeError> {
    let len = len as usize;
    if len > MAX_FRAME_LEN {
        Err(DecodeError::TooLarge(len))
    } else {
        Ok(len)
    }
}

/// Decodes a frame payload (without its length prefix).
pub fn decode_frame<T: serde::de::DeserializeOwned>(payload: &[u8]) -> Result<T, DecodeError> {
    Ok(postcard::from_bytes(payload)?)
}

pub fn encode_datagram(msg: &Datagram) -> Vec<u8> {
    postcard::to_stdvec(msg).expect("serializing to a Vec cannot fail")
}

pub fn decode_datagram(bytes: &[u8]) -> Result<Datagram, DecodeError> {
    Ok(postcard::from_bytes(bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn hello() -> Hello {
        Hello {
            protocol: PROTOCOL_VERSION,
            app_version: "0.1.0".into(),
            name: "Studio PC".into(),
            os: Os::Windows,
            screens: Screens {
                displays: vec![Display {
                    id: 1,
                    bounds: Rect::new(0.0, 0.0, 3840.0, 2160.0),
                    pixel_scale: 1.0,
                    ui_scale: 1.5,
                    primary: true,
                    name: "DELL U2723QE".into(),
                }],
                native_per_desk: 1.5,
            },
        }
    }

    #[test]
    fn control_round_trips() {
        let msgs = [
            Control::Hello(hello()),
            Control::Enter {
                seq: 7,
                pos: Point::new(10.5, 20.0),
            },
            Control::Leave,
            Control::Key {
                usage: 0x04,
                down: true,
            },
            Control::Button {
                button: Button::Right,
                down: false,
                pos: Point::new(1.0, 2.0),
            },
            Control::Scroll(Scroll::Wheel { x: 0.0, y: -120.0 }),
            Control::Yield,
            Control::ExtendRequest(ExtendRequest {
                width: 3840,
                height: 2160,
                hidpi: true,
                fps: 60,
                bitrate: 40_000_000,
            }),
            Control::Extended {
                bounds: Rect::new(-1920.0, 0.0, 1920.0, 1080.0),
            },
            Control::ExtendStop {
                reason: "closed".into(),
            },
            Control::Keyframe,
        ];
        for msg in msgs {
            let frame = encode_frame(&msg);
            let len = u32::from_le_bytes(frame[..4].try_into().unwrap());
            assert_eq!(check_frame_len(len).unwrap(), frame.len() - 4);
            assert_eq!(decode_frame::<Control>(&frame[4..]).unwrap(), msg);
        }
    }

    #[test]
    fn video_frame_headers_round_trip_and_refuse_nonsense() {
        let header = VideoFrameHeader {
            len: 123_456,
            keyframe: true,
            sender_us: 21_500,
        };
        assert_eq!(VideoFrameHeader::decode(header.encode()), Some(header));
        let mut huge = header.encode();
        huge[..4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(VideoFrameHeader::decode(huge), None);
        let mut bad_flag = header.encode();
        bad_flag[4] = 7;
        assert_eq!(VideoFrameHeader::decode(bad_flag), None);
    }

    #[test]
    fn motion_datagram_is_small() {
        let d = Datagram::Motion {
            seq: u32::MAX,
            pos: Point::new(-3840.25, 2159.75),
        };
        let bytes = encode_datagram(&d);
        assert!(
            bytes.len() <= 24,
            "motion datagram is {} bytes",
            bytes.len()
        );
        assert_eq!(decode_datagram(&bytes).unwrap(), d);
    }

    #[test]
    fn pairing_codes_are_six_digits() {
        assert_eq!(pairing_code([0xff; 4]), 967_295);
        assert_eq!(format_pairing_code(42_917), "042 917");
        assert_eq!(format_pairing_code(7), "000 007");
    }

    #[test]
    fn oversized_frames_are_rejected() {
        assert!(check_frame_len(MAX_FRAME_LEN as u32 + 1).is_err());
    }

    proptest! {
        // Anything off the network must be rejected cleanly, never panic.
        #[test]
        fn decoders_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
            let _ = decode_frame::<Control>(&bytes);
            let _ = decode_frame::<Pair>(&bytes);
            let _ = decode_datagram(&bytes);
        }
    }
}
