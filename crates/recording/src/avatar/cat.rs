use super::{Pose, paint::Portrait};
use tiny_skia::{
    Color, FillRule, GradientStop, LinearGradient, Paint, Path, PathBuilder, Point, SpreadMode,
    Transform,
};

const FUR: u32 = 0xb8a8aa;
const SHADE: u32 = 0x9e8b95;
const CREAM: u32 = 0xffeee3;
const INK: u32 = 0x4d333e;

impl Portrait {
    pub(super) fn draw_cat(&mut self, pose: Pose, elapsed: f32, split: bool) {
        self.pixels.fill(Color::from_rgba8(244, 231, 225, 255));
        let still = Transform::identity();
        let breath = elapsed.mul_add(1.7, 0.).sin() * 2.;
        let body = Transform::from_translate(0., breath + if split { 0. } else { 21. });
        if split {
            self.oval([137., 584., 387., 42.], 0xe6d3d0, still);
            self.curve([408., 551., 522., 578., 472., 488.], SHADE, 48., body);
        }
        let mut torso = PathBuilder::new();
        torso.move_to(320., 384.);
        torso.cubic_to(406., 384., 425., 467., 433., 508.);
        torso.cubic_to(464., 557., 444., 590., 411., 598.);
        torso.quad_to(320., 617., 229., 598.);
        torso.cubic_to(196., 590., 176., 557., 207., 508.);
        torso.cubic_to(215., 467., 234., 384., 320., 384.);
        torso.close();
        self.cat_fur(torso.finish().expect("cat torso"), 425., 601., body);
        self.oval([245., 428., 150., 139.], CREAM, body);
        for (x, side) in [(276., -1.), (364., 1.)] {
            self.curve([x + side * 15., 495., x, 528., x, 575.], FUR, 51., body);
            self.oval([x - 34., 562., 68., 39.], CREAM, body);
            for toe in [-9., 9.] {
                self.curve(
                    [x + toe, 592., x + toe, 589., x + toe, 585.],
                    0xdac0b7,
                    2.5,
                    body,
                );
            }
        }

        let yaw = pose.yaw * 0.36;
        let pitch = pose.pitch * 0.28;
        let head = Transform::from_scale(yaw.cos(), pitch.cos())
            .pre_translate(-320., -419.)
            .post_translate(320., 419.)
            .post_concat(Transform::from_rotate_at(
                pose.roll.to_degrees() * 0.55,
                320.,
                419.,
            ))
            .post_translate(yaw.sin() * 24., pitch.sin() * 16. + breath);
        self.cat_ear(head);
        self.cat_ear(
            Transform::from_scale(-1., 1.)
                .post_translate(640., 0.)
                .post_concat(head),
        );

        let mut cheeks = PathBuilder::new();
        cheeks.move_to(314., 171.);
        cheeks.quad_to(332., 151., 352., 160.);
        cheeks.quad_to(341., 168., 341., 177.);
        cheeks.cubic_to(427., 178., 487., 230., 495., 307.);
        cheeks.cubic_to(519., 341., 510., 383., 476., 411.);
        cheeks.cubic_to(442., 440., 380., 458., 320., 458.);
        cheeks.cubic_to(260., 458., 198., 440., 164., 411.);
        cheeks.cubic_to(130., 383., 121., 341., 145., 307.);
        cheeks.cubic_to(154., 226., 220., 177., 314., 171.);
        cheeks.close();
        self.cat_fur(cheeks.finish().expect("cat head"), 181., 454., head);

        let face = Transform::from_translate(yaw.sin() * 36., pitch.sin() * 25.).post_concat(head);
        self.oval([243., 352., 154., 88.], CREAM, face);
        self.oval([222., 344., 108., 70.], CREAM, face);
        self.oval([310., 344., 108., 70.], CREAM, face);
        self.oval([178., 342., 49., 23.], 0xe1aeb2, face);
        self.oval([413., 342., 49., 23.], 0xe1aeb2, face);
        for (x, open) in [(247., pose.left_eye), (393., pose.right_eye)] {
            let eye_height = 5. + open * 59.;
            self.oval(
                [x - 25., 302. - eye_height * 0.50, 50., eye_height],
                INK,
                face,
            );
            if open > 0.45 {
                let glint = 5. + open * 9.;
                self.oval(
                    [x - 13., 295. - eye_height * 0.18, glint, glint],
                    0xfffaf4,
                    face,
                );
                self.oval([x + 8., 304. + open * 4., 5., 5.], 0xd8b9a8, face);
            }
        }

        let mut nose = PathBuilder::new();
        nose.move_to(307., 350.);
        nose.quad_to(320., 343., 333., 350.);
        nose.quad_to(331., 358., 320., 362.);
        nose.quad_to(309., 358., 307., 350.);
        nose.close();
        self.shape(nose.finish().expect("cat nose"), 0xba7f88, face);
        self.curve([320., 361., 320., 371., 320., 375.], INK, 3., face);
        let width = 34. * (1. + pose.mouth_width * 0.40);
        let height = pose.mouth * 22.;
        if height > 3. {
            self.oval([320. - width / 2., 374., width, height], INK, face);
        }
        self.curve(
            [320., 374., 310., 390., 296., 378. - pose.smile * 4.],
            INK,
            3.,
            face,
        );
        self.curve(
            [320., 374., 330., 390., 344., 378. - pose.smile * 4.],
            INK,
            3.,
            face,
        );
        for (x, side) in [(230., -1.), (410., 1.)] {
            self.curve(
                [x, 382., x + side * 27., 372., x + side * 57., 373.],
                0x88717c,
                3.,
                face,
            );
            self.curve(
                [x, 392., x + side * 26., 394., x + side * 52., 403.],
                0x88717c,
                3.,
                face,
            );
        }
    }

    fn cat_fur(&mut self, path: Path, top: f32, bottom: f32, transform: Transform) {
        let shader = LinearGradient::new(
            Point::from_xy(225., top),
            Point::from_xy(413., bottom),
            vec![
                GradientStop::new(0., Color::from_rgba8(212, 197, 192, 255)),
                GradientStop::new(0.58, Color::from_rgba8(188, 170, 174, 255)),
                GradientStop::new(1., Color::from_rgba8(163, 143, 155, 255)),
            ],
            SpreadMode::Pad,
            Transform::identity(),
        )
        .expect("cat fur gradient");
        let paint = Paint {
            shader,
            anti_alias: true,
            ..Paint::default()
        };
        self.pixels
            .fill_path(&path, &paint, FillRule::Winding, transform, None);
    }

    fn cat_ear(&mut self, transform: Transform) {
        let mut outline = PathBuilder::new();
        outline.move_to(154., 271.);
        outline.cubic_to(136., 241., 136., 187., 146., 155.);
        outline.quad_to(152., 140., 164., 146.);
        outline.cubic_to(200., 157., 231., 180., 251., 205.);
        outline.close();
        self.cat_fur(outline.finish().expect("cat ear"), 145., 271., transform);

        let mut inner = PathBuilder::new();
        inner.move_to(161., 232.);
        inner.quad_to(151., 205., 161., 171.);
        inner.quad_to(193., 180., 220., 204.);
        inner.quad_to(192., 205., 161., 232.);
        inner.close();
        self.shape(inner.finish().expect("cat inner ear"), 0xe3b0b4, transform);
    }
}
