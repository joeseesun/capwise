use super::Pose;
use tiny_skia::Pixmap;

pub(super) struct HeadRig {
    center: [f32; 2],
    radius: [f32; 2],
    neck_y: f32,
}

impl HeadRig {
    pub(super) fn new([x, y, width, height]: [u32; 4], neck_y: u32) -> Self {
        let radius = [width as f32 * 0.5, height as f32 * 0.5];
        Self {
            center: [x as f32 + radius[0], y as f32 + radius[1]],
            radius,
            neck_y: neck_y as f32,
        }
    }

    pub(super) fn render(&self, frame: Pixmap, pose: Pose) -> Pixmap {
        if pose.yaw.abs() + pose.pitch.abs() + pose.roll.abs() < 0.004 {
            return frame;
        }
        let [cx, cy] = self.center;
        let [rx, ry] = self.radius;
        let (yaw_sin, yaw_cos) = (pose.yaw * 0.38).sin_cos();
        let (pitch_sin, pitch_cos) = (pose.pitch * 0.30).sin_cos();
        let (roll_sin, roll_cos) = (pose.roll * 0.52).sin_cos();
        let width = frame.width() as usize;
        let height = frame.height() as usize;
        let left = (cx - rx * 1.46).max(0.) as usize;
        let right = ((cx + rx * 1.46).ceil() as usize).min(width);
        let top = (cy - ry * 1.28).max(0.) as usize;
        let bottom = (self.neck_y.ceil() as usize).min(height);
        let mut result = frame.clone();
        let source = frame.data();
        let output = result.data_mut();

        for y in top..bottom {
            let y_float = y as f32;
            let ny = (y_float - cy) / ry;
            let head_depth = smoothstep((1.23 - ny.abs()) / 0.50);
            let y_weight =
                smoothstep((ny + 1.28) / 0.28) * smoothstep((self.neck_y - y_float) / (ry * 0.42));
            let neck_offset = y_float - self.neck_y;
            let row = &mut output[(y * width + left) * 4..(y * width + right) * 4];
            for (col, pixel) in row.chunks_exact_mut(4).enumerate() {
                let x = (col + left) as f32;
                let nx = (x - cx) / rx;
                let weight = y_weight * smoothstep((1.46 - nx.abs()) / 0.46);
                let depth = smoothstep((1.20 - nx.abs()) / 0.55) * head_depth;

                let rotated_x = (x - cx) * roll_cos + neck_offset * roll_sin;
                let rotated_y = neck_offset * roll_cos - (x - cx) * roll_sin;
                let yaw_offset = rx * (yaw_sin * (0.17 + 0.41 * depth) + nx * (yaw_cos - 1.));
                let pitch_offset = ry * (pitch_sin * (0.12 + 0.30 * depth) + ny * (pitch_cos - 1.));

                let sx = x + weight * (rotated_x - (x - cx) - yaw_offset);
                let sy = y_float + weight * (rotated_y - neck_offset - pitch_offset);
                sample(source, width, height, sx, sy, pixel);
            }
        }
        result
    }
}

fn smoothstep(value: f32) -> f32 {
    let t = value.clamp(0., 1.);
    t * t * (3. - 2. * t)
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
