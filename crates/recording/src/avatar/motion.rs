use super::Pose;
use tiny_skia::{FilterQuality, IntRect, Mask, Pixmap, PixmapPaint, Transform};

const RAVEN_NECK_ROW: u32 = 183;

pub(super) struct RavenHead {
    plate: Pixmap,
    matte: Mask,
    bounds: IntRect,
}

impl RavenHead {
    pub(super) fn new(frame: &Pixmap) -> Self {
        let bounds =
            IntRect::from_xywh(0, 0, frame.width(), RAVEN_NECK_ROW).expect("raven head bounds");
        let mut matte = Mask::new(bounds.width(), bounds.height()).expect("raven head matte");
        let mut plate = frame.clone();
        let width = frame.width() as usize;
        let stride = width * 4;
        let original = frame.data();
        let neck = &original[RAVEN_NECK_ROW as usize * stride..][..stride];
        for (y, (row, alpha_row)) in plate
            .data_mut()
            .chunks_exact_mut(stride)
            .zip(matte.data_mut().chunks_exact_mut(width))
            .enumerate()
        {
            let original_row = &original[y * stride..][..stride];
            let left = &original_row[..4];
            let right = &original_row[stride - 4..];
            let neck_opacity = ((y as f32 - 151.) / 26.).clamp(0., 1.);
            for (x, (target, alpha)) in row.chunks_exact_mut(4).zip(alpha_row).enumerate() {
                let offset = x * 4;
                let source = &original_row[offset..offset + 4];
                let neck_pixel = &neck[offset..offset + 4];
                let background_weight = x as f32 / (width - 1) as f32;
                let neck_weight = neck_opacity * raven_ink(neck_pixel);
                for channel in 0..3 {
                    let background = mix(left[channel], right[channel], background_weight);
                    target[channel] = mix(background, neck_pixel[channel], neck_weight);
                }
                let feather_start = if x > 220 && source[0] > 70 {
                    179.
                } else {
                    166.
                };
                let feather = ((RAVEN_NECK_ROW as f32 - y as f32)
                    / (RAVEN_NECK_ROW as f32 - feather_start))
                    .clamp(0., 1.);
                *alpha = (raven_ink(source) * feather * 255.).round() as u8;
            }
        }
        Self {
            plate,
            matte,
            bounds,
        }
    }

    pub(super) fn render(&self, frame: Pixmap, pose: Pose) -> Pixmap {
        let mut head = frame
            .clone_rect(self.bounds)
            .expect("raven head within artwork");
        head.apply_mask(&self.matte);
        let yaw = pose.yaw.clamp(-1., 1.);
        let pitch = pose.pitch.clamp(-1., 1.);
        let roll = pose.roll.clamp(-0.5, 0.5) * 0.28;
        let transform = Transform::from_scale(1. - 0.08 * yaw.abs(), 1. - 0.04 * pitch.abs())
            .pre_translate(-187., -168.)
            .post_rotate(roll.to_degrees() + pitch * 3.)
            .post_translate(187. + yaw * 10., 168. + pitch * 3.);
        let mut output = self.plate.clone();
        output.draw_pixmap(
            0,
            0,
            head.as_ref(),
            &PixmapPaint {
                quality: FilterQuality::Bilinear,
                ..PixmapPaint::default()
            },
            transform,
            None,
        );
        output
    }
}

fn raven_ink(pixel: &[u8]) -> f32 {
    let warmth = f32::from(pixel[0]) - f32::from(pixel[1]);
    ((84. - warmth) / 28.).clamp(0., 1.)
}

fn mix(a: u8, b: u8, amount: f32) -> u8 {
    (f32::from(a) + (f32::from(b) - f32::from(a)) * amount).round() as u8
}

pub(super) fn sample(source: &[u8], width: usize, height: usize, x: f32, y: f32, pixel: &mut [u8]) {
    let x = x.clamp(0., (width - 1) as f32);
    let y = y.clamp(0., (height - 1) as f32);
    let ix = x as usize;
    let iy = y as usize;
    let fx = x - ix as f32;
    let fy = y - iy as f32;
    let right = (ix + 1).min(width - 1);
    let bottom = (iy + 1).min(height - 1);
    let a = (iy * width + ix) * 4;
    let b = (iy * width + right) * 4;
    let c = (bottom * width + ix) * 4;
    let d = (bottom * width + right) * 4;
    for (channel, out) in pixel.iter_mut().enumerate().take(3) {
        let top = f32::from(source[a + channel]) * (1. - fx) + f32::from(source[b + channel]) * fx;
        let bottom =
            f32::from(source[c + channel]) * (1. - fx) + f32::from(source[d + channel]) * fx;
        *out = (top * (1. - fy) + bottom * fy + 0.5) as u8;
    }
}
