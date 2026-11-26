//! Frame → window: aspect-preserving fit, CPU blit, and the inverse mapping for input.

use mvf_display::FrameInfo;

/// Background outside the frame (and before the first frame), `0x00RRGGBB`.
pub const BACKGROUND: u32 = 0x0020_2428;

/// Where the frame lands inside the window, in window pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Letterbox {
    /// Left edge.
    pub x: u32,
    /// Top edge.
    pub y: u32,
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
}

/// Fits `frame_w × frame_h` into `win_w × win_h`, preserving aspect, centred.
pub fn letterbox(frame_w: u32, frame_h: u32, win_w: u32, win_h: u32) -> Letterbox {
    if frame_w == 0 || frame_h == 0 || win_w == 0 || win_h == 0 {
        return Letterbox::default();
    }
    let (fw, fh, ww, wh) = (
        u64::from(frame_w),
        u64::from(frame_h),
        u64::from(win_w),
        u64::from(win_h),
    );
    let (width, height) = if ww * fh <= wh * fw {
        (ww, (ww * fh / fw).max(1))
    } else {
        ((wh * fw / fh).max(1), wh)
    };
    Letterbox {
        x: ((ww - width) / 2) as u32,
        y: ((wh - height) / 2) as u32,
        width: width as u32,
        height: height as u32,
    }
}

/// Window pixel → frame pixel, `None` outside the frame.
pub fn window_to_frame(
    lb: &Letterbox,
    frame_w: u32,
    frame_h: u32,
    x: f64,
    y: f64,
) -> Option<(f32, f32)> {
    let (lx, ly) = (x - f64::from(lb.x), y - f64::from(lb.y));
    if lb.width == 0
        || lx < 0.0
        || ly < 0.0
        || lx >= f64::from(lb.width)
        || ly >= f64::from(lb.height)
    {
        return None;
    }
    Some((
        (lx * f64::from(frame_w) / f64::from(lb.width)) as f32,
        (ly * f64::from(frame_h) / f64::from(lb.height)) as f32,
    ))
}

/// Draws `frame` (described by `info`) into a `win_w × win_h` buffer of `0x00RRGGBB` pixels.
pub fn blit(frame: &[u8], info: &FrameInfo, dst: &mut [u32], win_w: u32, win_h: u32) -> Letterbox {
    let lb = letterbox(info.width, info.height, win_w, win_h);
    dst.fill(BACKGROUND);
    if lb.width == 0 {
        return lb;
    }
    let bpp = info.format.bytes_per_pixel() as usize;
    let stride = info.stride as usize;
    let xs: Vec<usize> = (0..lb.width)
        .map(|x| (u64::from(x) * u64::from(info.width) / u64::from(lb.width)) as usize * bpp)
        .collect();
    for y in 0..lb.height {
        let sy = (u64::from(y) * u64::from(info.height) / u64::from(lb.height)) as usize;
        let row = sy * stride;
        let out = (lb.y + y) as usize * win_w as usize + lb.x as usize;
        for (i, sx) in xs.iter().enumerate() {
            dst[out + i] = info.format.rgb(frame, row + sx);
        }
    }
    lb
}

#[cfg(test)]
mod tests {
    use super::*;
    use mvf_display::PixelFormat;

    #[test]
    fn frames_are_letterboxed_and_input_maps_back_to_frame_pixels() {
        let lb = letterbox(640, 360, 800, 800);
        assert_eq!(
            lb,
            Letterbox {
                x: 0,
                y: 175,
                width: 800,
                height: 450
            }
        );
        assert_eq!(
            window_to_frame(&lb, 640, 360, 400.0, 400.0),
            Some((320.0, 180.0))
        );
        assert_eq!(
            window_to_frame(&lb, 640, 360, 400.0, 100.0),
            None,
            "the bar is not the frame"
        );
        let tall = letterbox(100, 200, 400, 200);
        assert_eq!(
            tall,
            Letterbox {
                x: 150,
                y: 0,
                width: 100,
                height: 200
            }
        );
    }

    #[test]
    fn blit_scales_and_converts_every_format() {
        let info = |format, w, h| FrameInfo {
            seq: 1,
            width: w,
            height: h,
            stride: w * PixelFormat::bytes_per_pixel(format),
            format,
            timestamp_us: 0,
        };
        let mut dst = vec![0u32; 4 * 4];
        // 2x2 BGRA: blue, green / red, white → scaled to 4x4.
        let bgra = [
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ];
        blit(&bgra, &info(PixelFormat::Bgra8, 2, 2), &mut dst, 4, 4);
        assert_eq!(&dst[..4], &[0x0000FF, 0x0000FF, 0x00FF00, 0x00FF00]);
        assert_eq!(dst[15], 0xFFFFFF);
        let gray = [7u8];
        blit(&gray, &info(PixelFormat::Gray8, 1, 1), &mut dst, 4, 4);
        assert!(dst.iter().all(|&p| p == 0x070707));
        let rgb = [1u8, 2, 3];
        let mut wide = vec![0u32; 4 * 2];
        let lb = blit(&rgb, &info(PixelFormat::Rgb8, 1, 1), &mut wide, 4, 2);
        assert_eq!(
            lb,
            Letterbox {
                x: 1,
                y: 0,
                width: 2,
                height: 2
            }
        );
        assert_eq!(wide[0], BACKGROUND);
        assert_eq!(wide[1], 0x010203);
    }
}
