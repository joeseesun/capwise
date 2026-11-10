use super::{AvatarKind, Pose};
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};

pub(crate) struct Portrait {
    pub pixels: Pixmap,
}

impl Portrait {
    pub fn new() -> Self {
        Self {
            pixels: Pixmap::new(640, 640).expect("fixed portrait dimensions"),
        }
    }

    fn shape(&mut self, path: tiny_skia::Path, color: u32, transform: Transform) {
        let mut paint = Paint::default();
        paint.set_color_rgba8((color >> 16) as u8, (color >> 8) as u8, color as u8, 255);
        self.pixels
            .fill_path(&path, &paint, FillRule::Winding, transform, None);
    }

    fn oval(&mut self, bounds: [f32; 4], color: u32, transform: Transform) {
        if let Some(rect) = tiny_skia::Rect::from_xywh(bounds[0], bounds[1], bounds[2], bounds[3])
            && let Some(path) = PathBuilder::from_oval(rect)
        {
            self.shape(path, color, transform);
        }
    }

    fn curve(&mut self, points: [f32; 6], color: u32, width: f32, transform: Transform) {
        let mut path = PathBuilder::new();
        path.move_to(points[0], points[1]);
        path.quad_to(points[2], points[3], points[4], points[5]);
        if let Some(path) = path.finish() {
            let mut paint = Paint::default();
            paint.set_color_rgba8((color >> 16) as u8, (color >> 8) as u8, color as u8, 255);
            self.pixels.stroke_path(
                &path,
                &paint,
                &Stroke {
                    width,
                    line_cap: tiny_skia::LineCap::Round,
                    ..Stroke::default()
                },
                transform,
                None,
            );
        }
    }

    fn ear(&mut self, x: f32, color: u32, transform: Transform) {
        let mut path = PathBuilder::new();
        path.move_to(x, 245.);
        path.cubic_to(x - 12., 210., x - 10., 113., x + 14., 121.);
        path.cubic_to(x + 42., 130., x + 79., 182., x + 85., 221.);
        path.close();
        if let Some(path) = path.finish() {
            self.shape(path, color, transform);
        }
    }

    pub fn draw(&mut self, kind: AvatarKind, pose: Pose, elapsed: f32, split: bool) {
        if super::illustrated::draw(&mut self.pixels, kind, pose, split) {
            return;
        }
        let (fur, dark, background, shirt) = match kind {
            AvatarKind::Bear => (0xb58c69, 0x77543c, 0xe8e3d9, 0x546f73),
            AvatarKind::Fox => (0xd58c50, 0x955733, 0xe9e1d5, 0x59637d),
            _ => (0xb9c8c7, 0x7b9998, 0xe0e9e5, 0x42695e),
        };
        self.pixels.fill(Color::from_rgba8(
            (background >> 16) as u8,
            (background >> 8) as u8,
            background as u8,
            255,
        ));
        let still = Transform::identity();
        self.oval([76., 540., 488., 80.], 0xd0d8d1, still);
        let breath = elapsed.mul_add(1.8, 0.).sin() * 2.;
        if split {
            self.oval([149., 463. + breath, 80., 142.], shirt, still);
            self.oval([411., 463. + breath, 80., 142.], shirt, still);
            self.oval([154., 571. + breath, 47., 49.], fur, still);
            self.oval([439., 571. + breath, 47., 49.], fur, still);
        }
        self.oval([175., 426. + breath, 290., 260.], shirt, still);
        self.curve(
            [
                286.,
                466. + breath,
                320.,
                493. + breath,
                354.,
                466. + breath,
            ],
            0xf6eee0,
            8.,
            still,
        );
        self.oval([300., 516. + breath, 40., 40.], 0xf6eee0, still);
        self.oval([315., 527. + breath, 10., 16.], shirt, still);
        let yaw = pose.yaw * 0.38;
        let pitch = pose.pitch * 0.30;
        let head = Transform::from_scale(yaw.cos(), pitch.cos())
            .pre_translate(-320., -460.)
            .post_translate(320., 460.)
            .post_concat(Transform::from_rotate_at(
                pose.roll.to_degrees() * 0.65,
                320.,
                460.,
            ))
            .post_translate(yaw.sin() * 22., pitch.sin() * 17. + breath);
        if kind == AvatarKind::Bear {
            self.oval([165., 170., 104., 110.], fur, head);
            self.oval([371., 170., 104., 110.], fur, head);
            self.oval([186., 193., 61., 65.], dark, head);
            self.oval([392., 193., 61., 65.], dark, head);
        } else {
            self.ear(184., fur, head);
            self.ear(368., fur, head);
            self.ear(
                197.,
                0xdcae9b,
                Transform::from_scale(0.67, 0.67)
                    .post_translate(69., 68.)
                    .post_concat(head),
            );
            self.ear(
                381.,
                0xdcae9b,
                Transform::from_scale(0.67, 0.67)
                    .post_translate(129., 68.)
                    .post_concat(head),
            );
        }
        self.oval([174., 205., 292., 280.], dark, head);
        self.oval([176., 199., 288., 274.], fur, head);
        self.oval([205., 345., 230., 112.], 0xf6eee0, head);
        let face = Transform::from_translate(yaw.sin() * 63., pitch.sin() * 44.).post_concat(head);
        for (x, eye, side) in [(258., pose.left_eye, -1.), (382., pose.right_eye, 1.)] {
            let height = 4. + eye * 36.;
            let width = 28. * (1. + side * yaw.sin() * 0.30);
            self.oval(
                [x - width / 2., 311. - height / 2., width, height],
                0x293638,
                face,
            );
            if eye > 0.4 {
                self.oval([x - 7., 299., 7., 9.], 0xffffff, face);
            }
            self.curve(
                [x - 22., 270., x, 260. - pose.smile * 5., x + 18., 268.],
                dark,
                6.,
                face,
            );
        }
        self.oval([220., 346., 32., 14.], 0xdcae9b, face);
        self.oval([390., 346., 32., 14.], 0xdcae9b, face);
        self.oval([306., 347., 28., 18.], 0x293638, face);
        let mouth_width = 50. * (1. + pose.mouth_width * 0.34);
        let mouth_height = 4. + pose.mouth * 39.;
        self.oval(
            [320. - mouth_width * 0.5, 384., mouth_width, mouth_height],
            0x463139,
            face,
        );
        if mouth_height > 12. {
            self.oval(
                [
                    320. - mouth_width * 0.24,
                    384. + mouth_height * 0.62,
                    mouth_width * 0.48,
                    mouth_height * 0.25,
                ],
                0xd99596,
                face,
            );
        }
        if kind == AvatarKind::Cat {
            for (x, dx) in [(239., -55.), (401., 55.)] {
                self.curve([x, 373., x + dx * 0.5, 367., x + dx, 365.], dark, 3., head);
                self.curve([x, 386., x + dx * 0.5, 390., x + dx, 398.], dark, 3., head);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn characters_and_expressions_produce_distinct_opaque_frames() {
        let mut portrait = Portrait::new();
        portrait.draw(AvatarKind::Cat, Pose::default(), 0., false);
        let cat = portrait.pixels.data().to_vec();
        assert!(cat.chunks_exact(4).all(|pixel| pixel[3] == 255));
        if let Ok(directory) = std::env::var("CAP_AVATAR_TEST_PORTRAITS") {
            for kind in [AvatarKind::Cat, AvatarKind::Bear, AvatarKind::Fox] {
                portrait.draw(kind, Pose::default(), 0., false);
                portrait
                    .pixels
                    .save_png(std::path::Path::new(&directory).join(format!("{kind:?}.png")))
                    .unwrap();
            }
        }
        portrait.draw(AvatarKind::Bear, Pose::default(), 0., false);
        assert_ne!(cat, portrait.pixels.data());
        portrait.draw(
            AvatarKind::Cat,
            Pose {
                mouth: 1.,
                left_eye: 0.,
                yaw: 0.7,
                ..Pose::default()
            },
            0.,
            false,
        );
        assert_ne!(cat, portrait.pixels.data());
    }
}
