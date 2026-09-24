//! A known picture sequence, encoded on the Mac and decoded on Windows in tests to check
//! both ends agree.

/// Size of the test stream.
pub const WIDTH: u32 = 320;
pub const HEIGHT: u32 = 240;
pub const FRAMES: u32 = 8;

/// The (Y, U, V) colour of the left and right halves of frame `n` (video range).
pub fn colors(n: u32) -> [(u8, u8, u8); 2] {
    let n = n as u8;
    [(50 + n * 15, 90, 200), (200 - n * 10, 200, 90)]
}

/// Frame `n` as NV12 with no row padding.
pub fn nv12(n: u32) -> Vec<u8> {
    let [left, right] = colors(n);
    let (w, h) = (WIDTH as usize, HEIGHT as usize);
    let mut data = vec![0u8; w * h * 3 / 2];
    for y in 0..h {
        for x in 0..w {
            data[y * w + x] = if x < w / 2 { left.0 } else { right.0 };
        }
    }
    for y in 0..h / 2 {
        for x in 0..w / 2 {
            let (u, v) = if x < w / 4 {
                (left.1, left.2)
            } else {
                (right.1, right.2)
            };
            let i = w * h + y * w + x * 2;
            data[i] = u;
            data[i + 1] = v;
        }
    }
    data
}

/// Checks a decoded frame against frame `n`, sampling away from the middle seam.
pub fn matches(frame: &crate::Nv12, n: u32) -> Result<(), String> {
    let [left, right] = colors(n);
    let (w, h) = (frame.width as usize, frame.height as usize);
    if (w, h) != (WIDTH as usize, HEIGHT as usize) {
        return Err(format!("frame is {w}x{h}"));
    }
    let stride = frame.stride as usize;
    let close = |got: u8, want: u8| got.abs_diff(want) <= 6;
    for (x, want) in [(w / 4, left), (w * 3 / 4, right)] {
        let y = h / 2;
        let luma = frame.y()[y * stride + x];
        let c = (y / 2) * stride + (x / 2) * 2;
        let (u, v) = (frame.uv()[c], frame.uv()[c + 1]);
        if !(close(luma, want.0) && close(u, want.1) && close(v, want.2)) {
            return Err(format!(
                "frame {n} at x={x}: got YUV ({luma}, {u}, {v}), want {want:?}"
            ));
        }
    }
    Ok(())
}

/// Frames as stored in the fixture: each one prefixed with its length (u32, little endian).
pub fn pack(frames: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    for frame in frames {
        out.extend_from_slice(&(frame.len() as u32).to_le_bytes());
        out.extend_from_slice(frame);
    }
    out
}

pub fn unpack(mut data: &[u8]) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    while data.len() >= 4 {
        let len = u32::from_le_bytes(data[..4].try_into().unwrap()) as usize;
        frames.push(data[4..4 + len].to_vec());
        data = &data[4 + len..];
    }
    frames
}
