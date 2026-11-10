use super::Pose;
use tiny_skia::{FilterQuality, IntRect, Pixmap, PixmapPaint, Transform};

const WIDTH: usize = 242;
const HEIGHT: usize = 300;
const EYES: [[f32; 3]; 3] = [[0.165, 0.31, 0.46], [0.35, 0.50, 0.65], [0.57, 0.71, 0.84]];
const MOUTH: [[f32; 3]; 3] = [[0.17, 0.35, 0.52], [0.34, 0.50, 0.66], [0.54, 0.70, 0.85]];

pub(super) struct SilverViews {
    heads: [Pixmap; 3],
    contours: [[[f32; 2]; HEIGHT]; 3],
    lids: [Pixmap; 2],
    mouths: [Pixmap; 3],
}

impl SilverViews {
    pub(super) fn load() -> Self {
        let atlas = Pixmap::decode_png(include_bytes!("art/silver-head-poses.png"))
            .expect("valid avatar poses");
        let heads = [
            [158, 45, 485, 612],
            [842, 45, 486, 612],
            [1525, 45, 501, 612],
        ]
        .map(|area| extract(&atlas, area, [WIDTH as u32, HEIGHT as u32]));
        let expressions = Pixmap::decode_png(include_bytes!("art/silver-head-expressions.png"))
            .expect("valid avatar expressions");
        let lids = [[125, 367, 127, 62], [303, 367, 127, 62]]
            .map(|area| extract(&expressions, area, [52, 26]));
        let mouths = [
            [722, 521, 204, 80],
            [1265, 521, 204, 80],
            [1807, 521, 204, 80],
        ]
        .map(|area| extract(&expressions, area, [90, 36]));
        let contours =
            std::array::from_fn(|i| std::array::from_fn(|row| row_contour(&heads[i], row)));
        Self {
            heads,
            contours,
            lids,
            mouths,
        }
    }

    pub(super) fn render(&self, pose: Pose) -> Pixmap {
        let side = if pose.yaw < 0. { 0 } else { 2 };
        let amount = pose.yaw.abs().clamp(0., 1.);
        let mut output = if amount > 0.001 {
            self.blend_head(side, amount)
        } else {
            self.heads[1].clone()
        };
        let eyes = interpolate(EYES[1], EYES[side], amount);
        for ((x, opening), lid) in [eyes[0], eyes[2]]
            .into_iter()
            .zip([pose.left_eye, pose.right_eye])
            .zip(&self.lids)
        {
            let closed = ((0.55 - opening) / 0.43).clamp(0., 1.);
            stamp(
                &mut output,
                lid,
                [x * WIDTH as f32, 163.],
                1. - amount * 0.22,
                closed,
            );
        }
        let jaw = pose.mouth.clamp(0., 1.);
        if jaw > 0.055 {
            let form = pose.mouth_width.clamp(-1., 1.);
            let mouth = interpolate(MOUTH[1], MOUTH[side], amount);
            let x = mouth[1] * WIDTH as f32;
            let width = 1. - amount * 0.20;
            let force = ((jaw - 0.055) / 0.37).clamp(0., 1.);
            stamp(&mut output, &self.mouths[0], [x, 234.], width, force);
            let variant = if form < 0. { 2 } else { 1 };
            stamp(
                &mut output,
                &self.mouths[variant],
                [x, 234.],
                width,
                force * form.abs(),
            );
        }
        output
    }

    fn blend_head(&self, side: usize, amount: f32) -> Pixmap {
        let mut output = self.heads[1].clone();
        let center = self.heads[1].data();
        let turned = self.heads[side].data();
        let width = (WIDTH - 1) as f32;
        for (y, row) in output.data_mut().chunks_exact_mut(WIDTH * 4).enumerate() {
            let p = y as f32 / (HEIGHT - 1) as f32;
            let front = with_contour(row_anchors(1, p), self.contours[1][y]);
            let angle = with_contour(row_anchors(side, p), self.contours[side][y]);
            let anchors =
                std::array::from_fn::<_, 5, _>(|i| front[i] + (angle[i] - front[i]) * amount);
            let row_offset = y * WIDTH * 4;
            for (x, pixel) in row.chunks_exact_mut(4).enumerate() {
                let u = x as f32 / width;
                if u < anchors[0] || u > anchors[4] {
                    pixel.fill(0);
                    continue;
                }
                let column = anchors.partition_point(|point| *point <= u).clamp(1, 4);
                let span = anchors[column] - anchors[column - 1];
                let position = (u - anchors[column - 1]) / span;
                let coord = |points: [f32; 5]| {
                    (points[column - 1] + position * (points[column] - points[column - 1])) * width
                };
                let a = coord(front).clamp(0., width);
                let b = coord(angle).clamp(0., width);
                let sample = |source: &[u8], x: f32, channel| {
                    let left = x as usize;
                    let right = (left + 1).min(WIDTH - 1);
                    let start = f32::from(source[row_offset + left * 4 + channel]);
                    let end = f32::from(source[row_offset + right * 4 + channel]);
                    start + (end - start) * x.fract()
                };
                for (channel, out) in pixel.iter_mut().enumerate() {
                    let f = sample(center, a, channel);
                    let s = sample(turned, b, channel);
                    *out = (f + (s - f) * amount).round() as u8;
                }
            }
        }
        output
    }
}

fn row_anchors(view: usize, y: f32) -> [f32; 3] {
    if y < 0.51 {
        interpolate(
            [0.25, 0.50, 0.75],
            EYES[view],
            ((y - 0.34) / 0.17).clamp(0., 1.),
        )
    } else if y < 0.79 {
        interpolate(EYES[view], MOUTH[view], (y - 0.51) / 0.28)
    } else {
        interpolate(MOUTH[view], [0.25, 0.50, 0.75], (y - 0.79) / 0.21)
    }
}

fn row_contour(image: &Pixmap, row: usize) -> [f32; 2] {
    let alpha = |x| image.data()[(row * WIDTH + x) * 4 + 3] > 5;
    let span = (WIDTH - 1) as f32;
    let left = (0..WIDTH)
        .find(|&x| alpha(x))
        .unwrap_or_default()
        .saturating_sub(1);
    let right = (0..WIDTH)
        .rev()
        .find(|&x| alpha(x))
        .map_or(WIDTH - 1, |x| (x + 1).min(WIDTH - 1));
    [left as f32 / span, right as f32 / span]
}

fn with_contour([a, b, c]: [f32; 3], [left, right]: [f32; 2]) -> [f32; 5] {
    let step = (right - left) * 0.04;
    let a = a.clamp(left + step, right - 3. * step);
    let b = b.clamp(a + step, right - 2. * step);
    let c = c.clamp(b + step, right - step);
    [left, a, b, c, right]
}

fn interpolate(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t)
}

fn extract(atlas: &Pixmap, [x, y, w, h]: [u32; 4], [width, height]: [u32; 2]) -> Pixmap {
    let bounds = IntRect::from_xywh(x as i32, y as i32, w, h).expect("valid pose bounds");
    let crop = atlas.clone_rect(bounds).expect("pose within artwork");
    let mut image = Pixmap::new(width, height).expect("avatar dimensions");
    image.draw_pixmap(
        0,
        0,
        crop.as_ref(),
        &PixmapPaint {
            quality: FilterQuality::Bilinear,
            ..PixmapPaint::default()
        },
        Transform::from_scale(width as f32 / w as f32, height as f32 / h as f32),
        None,
    );
    image
}

fn stamp(frame: &mut Pixmap, sprite: &Pixmap, [cx, cy]: [f32; 2], scale: f32, weight: f32) {
    if weight < 0.015 {
        return;
    }
    let width = sprite.width() as usize;
    let height = sprite.height() as usize;
    let dest_width = (width as f32 * scale).round() as usize;
    let start_x = (cx - dest_width as f32 * 0.5).round() as usize;
    let start_y = (cy - height as f32 * 0.5).round() as usize;
    for y in 0..height {
        let vertical = 1. - (y as f32 * 2. / (height - 1) as f32 - 1.).abs();
        for x in 0..dest_width {
            let horizontal = 1. - (x as f32 * 2. / (dest_width - 1) as f32 - 1.).abs();
            let mix = weight * (vertical.min(horizontal) * 3.7).clamp(0., 1.);
            let sx = ((x as f32 / scale).round() as usize).min(width - 1);
            let source = (y * width + sx) * 4;
            let offset = ((y + start_y) * WIDTH + start_x + x) * 4;
            for channel in 0..4 {
                let target = &mut frame.data_mut()[offset + channel];
                let base = f32::from(*target);
                let paint = f32::from(sprite.data()[source + channel]);
                *target = (base + (paint - base) * mix).round() as u8;
            }
        }
    }
}
