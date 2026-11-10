use super::{AvatarKind, Pose, views::SilverViews};
use tiny_skia::{
    Color, FillRule, FilterQuality, IntRect, Mask, Paint, Path, PathBuilder, Pixmap, PixmapPaint,
    Transform,
};

pub(super) struct LayeredHead {
    plate: Pixmap,
    mask: Mask,
    bounds: IntRect,
    pivot: [f32; 2],
    mouth: [f32; 2],
    silver: Option<SilverViews>,
}

impl LayeredHead {
    pub(super) fn new(kind: AvatarKind, image: &Pixmap) -> Option<Self> {
        let (path, bounds, pivot, mouth, robot) = match kind {
            AvatarKind::RetroRobot => (
                robot_outline(),
                [74, 45, 239, 171],
                [193., 225.],
                [196., 170.],
                true,
            ),
            AvatarKind::SilverPresenter => (
                silver_outline(),
                [222, 30, 255, 315],
                [351., 341.],
                [350., 260.],
                false,
            ),
            _ => return None,
        };
        let mut mask = Mask::new(image.width(), image.height())?;
        mask.fill_path(&path, FillRule::Winding, true, Transform::identity());
        let [x, y, width, height] = bounds;
        let bounds = IntRect::from_xywh(x, y, width as u32, height as u32)?;
        let mut plate = image.clone();
        let stride = image.width() as usize;
        for y in bounds.top() as usize..bounds.bottom() as usize {
            let row = y * stride;
            let left = (row + bounds.left() as usize) * 4;
            let right = (row + bounds.right() as usize) * 4;
            for x in bounds.left() as usize..bounds.right() as usize {
                if (robot || y >= 300) && mask.data()[row + x] == 0 {
                    continue;
                }
                let offset = (row + x) * 4;
                let position = (offset - left) as f32 / (right - left) as f32;
                for channel in 0..3 {
                    let start = f32::from(image.data()[left + channel]);
                    let end = f32::from(image.data()[right + channel]);
                    plate.data_mut()[offset + channel] = mix(start, end, position).round() as u8;
                }
            }
        }
        Some(Self {
            plate,
            mask,
            bounds,
            pivot,
            mouth,
            silver: (!robot).then(SilverViews::load),
        })
    }

    pub(super) fn draw(&self, mut animated: Pixmap, pose: Pose) -> Pixmap {
        let yaw = pose.yaw.clamp(-1., 1.);
        let pitch = pose.pitch.clamp(-1., 1.);
        let roll = pose.roll.clamp(-0.50, 0.50) * 0.47;
        let [pivot_x, pivot_y] = self.pivot;
        let (head, x, y) = if let Some(silver) = &self.silver {
            let head = silver.render(pose);
            (head, 230., 40.)
        } else {
            self.draw_robot_mouth(&mut animated, pose);
            animated.apply_mask(&self.mask);
            (
                self.turn(&animated, yaw),
                self.bounds.left() as f32,
                self.bounds.top() as f32,
            )
        };
        let mut plate = self.plate.clone();
        self.draw_neck_socket(&mut plate, yaw);
        let translate_x = yaw * if self.silver.is_none() { 5. } else { 3. };
        let translate_y = pitch * if self.silver.is_none() { 4. } else { 5. };
        let transform = Transform::from_translate(x - pivot_x, y - pivot_y)
            .post_scale(1., 1. - 0.035 * pitch.abs())
            .post_rotate(roll.to_degrees())
            .post_translate(pivot_x + translate_x, pivot_y + translate_y);
        plate.draw_pixmap(
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
        plate
    }

    fn turn(&self, image: &Pixmap, yaw: f32) -> Pixmap {
        let source = image.clone_rect(self.bounds).expect("head within artwork");
        let width = source.width() as usize;
        let height = source.height() as usize;
        let mut target = source.clone();
        let u = [0., 0.16, 0.34, 0.50, 0.66, 0.84, 1.];
        let offsets = [0., -0.045, -0.035, -0.015, 0.013, 0.015, 0.];
        let mut key = std::array::from_fn::<_, 7, _>(|i| u[i] + offsets[i] * yaw.abs());
        if yaw < 0. {
            key.reverse();
            key = key.map(|v| 1. - v);
        }
        let span = (width - 1) as f32;
        let samples: Vec<_> = (0..width)
            .map(|x| {
                let t = x as f32 / span;
                let i = key
                    .partition_point(|point| *point <= t)
                    .clamp(1, key.len() - 1);
                let f = (t - key[i - 1]) / (key[i] - key[i - 1]);
                let sx = (mix(u[i - 1], u[i], f) * span).clamp(0., span);
                (sx as usize, sx.fract())
            })
            .collect();
        for y in 0..height {
            let row = y * width * 4;
            for (x, &(sx, fraction)) in samples.iter().enumerate() {
                let a = row + sx * 4;
                let b = row + (sx + 1).min(width - 1) * 4;
                let out = row + x * 4;
                for channel in 0..4 {
                    let start = f32::from(source.data()[a + channel]);
                    let end = f32::from(source.data()[b + channel]);
                    target.data_mut()[out + channel] = mix(start, end, fraction).round() as u8;
                }
            }
        }
        target
    }

    fn draw_neck_socket(&self, plate: &mut Pixmap, yaw: f32) {
        let mut path = PathBuilder::new();
        if self.silver.is_none() {
            path.move_to(172., 201.);
            path.line_to(216., 201.);
            path.line_to(214., 219.);
            path.quad_to(194., 225., 172., 218.);
            fill(plate, path, [33, 30, 28, 255]);
        } else {
            let offset = yaw * 2.;
            path.move_to(299. + offset, 296.);
            path.cubic_to(
                329. + offset,
                293.,
                373. + offset,
                294.,
                391. + offset,
                303.,
            );
            path.cubic_to(400., 313., 395., 333., 385., 344.);
            path.quad_to(353., 357., 321., 348.);
            path.cubic_to(314., 335., 306., 311., 299. + offset, 296.);
            path.close();
            let path = path.finish().expect("valid neck socket");
            let gradient = tiny_skia::LinearGradient::new(
                tiny_skia::Point::from_xy(323., 340.),
                tiny_skia::Point::from_xy(380., 341.),
                vec![
                    tiny_skia::GradientStop::new(0., Color::from_rgba8(106, 59, 44, 255)),
                    tiny_skia::GradientStop::new(0.78, Color::from_rgba8(202, 133, 94, 255)),
                    tiny_skia::GradientStop::new(1., Color::from_rgba8(158, 92, 63, 255)),
                ],
                tiny_skia::SpreadMode::Pad,
                Transform::identity(),
            )
            .expect("valid neck gradient");
            let paint = Paint {
                shader: gradient,
                anti_alias: true,
                ..Paint::default()
            };
            plate.fill_path(
                &path,
                &paint,
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }

    fn draw_robot_mouth(&self, frame: &mut Pixmap, pose: Pose) {
        let [x, y] = self.mouth;
        let mut base = PathBuilder::new();
        base.push_rect(
            tiny_skia::Rect::from_xywh(x - 28., y - 5., 56., 10.).expect("mouth region"),
        );
        let bg = frame
            .pixel((x - 34.) as u32, y as u32)
            .expect("robot display pixel")
            .demultiply();
        fill(frame, base, [bg.red(), bg.green(), bg.blue(), 255]);
        let jaw = pose.mouth;
        let spread = pose.mouth_width;
        let half_width = 23. + 10. * spread;
        let half_height = 2.5 + jaw * (7. - 2. * spread);
        let mut path = PathBuilder::new();
        path.move_to(x - half_width, y);
        path.cubic_to(
            x - half_width,
            y - half_height,
            x + half_width,
            y - half_height,
            x + half_width,
            y,
        );
        path.cubic_to(
            x + half_width,
            y + half_height,
            x - half_width,
            y + half_height,
            x - half_width,
            y,
        );
        fill(frame, path, [174, 205, 143, 255]);
    }
}

fn fill(target: &mut Pixmap, mut path: PathBuilder, rgba: [u8; 4]) {
    path.close();
    if let Some(path) = path.finish() {
        let [r, g, b, a] = rgba;
        let paint = Paint {
            shader: tiny_skia::Shader::SolidColor(Color::from_rgba8(r, g, b, a)),
            anti_alias: true,
            ..Paint::default()
        };
        target.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
}

fn robot_outline() -> Path {
    let mut path = PathBuilder::new();
    path.move_to(79., 110.);
    path.line_to(95., 103.);
    path.line_to(93., 89.);
    path.line_to(125., 53.);
    path.line_to(261., 49.);
    path.quad_to(293., 70., 292., 103.);
    path.line_to(307., 108.);
    path.line_to(307., 165.);
    path.line_to(289., 175.);
    path.quad_to(279., 198., 263., 208.);
    path.line_to(123., 209.);
    path.quad_to(99., 194., 96., 175.);
    path.line_to(78., 167.);
    path.close();
    path.finish().expect("valid robot outline")
}

fn silver_outline() -> Path {
    let mut path = PathBuilder::new();
    path.move_to(229., 124.);
    path.cubic_to(231., 83., 294., 44., 352., 40.);
    path.cubic_to(374., 34., 389., 43., 403., 68.);
    path.cubic_to(453., 60., 463., 114., 468., 144.);
    path.line_to(460., 195.);
    path.cubic_to(474., 213., 464., 260., 440., 264.);
    path.cubic_to(436., 293., 408., 308., 389., 323.);
    path.cubic_to(368., 341., 338., 342., 310., 320.);
    path.cubic_to(299., 310., 289., 306., 272., 300.);
    path.cubic_to(253., 287., 259., 268., 250., 264.);
    path.cubic_to(232., 254., 226., 217., 239., 200.);
    path.quad_to(230., 173., 229., 124.);
    path.close();
    path.finish().expect("valid silver outline")
}

fn mix(start: f32, end: f32, fraction: f32) -> f32 {
    start + (end - start) * fraction
}
