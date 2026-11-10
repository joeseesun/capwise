use super::{AvatarKind, Pose};
use std::sync::OnceLock;
use tiny_skia::{Color, FilterQuality, IntRect, Pixmap, PixmapPaint, Transform};

type Region = [u32; 4];

struct Artwork {
    neutral: Pixmap,
    expression: Pixmap,
    bubble: IntRect,
    eyes: [Region; 2],
    gaze: [f32; 2],
    mouth: Region,
}

fn crop([x, y, width, height]: Region) -> IntRect {
    IntRect::from_xywh(x as i32, y as i32, width, height).expect("valid avatar crop")
}

fn artwork(kind: AvatarKind) -> Option<&'static Artwork> {
    static RAVEN: OnceLock<Artwork> = OnceLock::new();
    static ROBOT: OnceLock<Artwork> = OnceLock::new();
    static SILVER: OnceLock<Artwork> = OnceLock::new();
    static CURLY: OnceLock<Artwork> = OnceLock::new();
    static BLACK_SUIT: OnceLock<Artwork> = OnceLock::new();
    static BLUE_JACKET: OnceLock<Artwork> = OnceLock::new();

    let illustrated = include_bytes!("art/illustrated.png").as_slice();
    let illustrated_expression = include_bytes!("art/illustrated-expression.png").as_slice();
    let presenters = include_bytes!("art/presenters.png").as_slice();
    let presenters_expression = include_bytes!("art/presenters-expression.png").as_slice();
    let (cache, neutral, expression, split, bubble, eyes, gaze, mouth) = match kind {
        AvatarKind::Raven => (
            &RAVEN,
            illustrated,
            illustrated_expression,
            [578, 45, 382, 605],
            [601, 47, 347, 347],
            [[720, 133, 58, 35], [806, 133, 40, 35]],
            [0., 0.],
            [742, 176, 125, 57],
        ),
        AvatarKind::RetroRobot => (
            &ROBOT,
            illustrated,
            illustrated_expression,
            [1051, 45, 387, 605],
            [1062, 51, 368, 368],
            [[1182, 159, 51, 32], [1261, 159, 49, 32]],
            [0., 0.],
            [1213, 201, 69, 28],
        ),
        AvatarKind::SilverPresenter => (
            &SILVER,
            include_bytes!("art/silver.png").as_slice(),
            include_bytes!("art/silver-expression.png").as_slice(),
            [121, 152, 694, 800],
            [256, 159, 424, 424],
            [[411, 331, 49, 43], [478, 330, 49, 43]],
            [-1.5, 1.5],
            [425, 398, 92, 38],
        ),
        AvatarKind::CurlyPresenter => (
            &CURLY,
            presenters,
            presenters_expression,
            [166, 4, 437, 670],
            [179, 4, 403, 403],
            [[315, 197, 64, 56], [402, 197, 65, 55]],
            [-4., 4.],
            [326, 294, 126, 47],
        ),
        AvatarKind::BlackSuitPresenter => (
            &BLACK_SUIT,
            presenters,
            presenters_expression,
            [903, 4, 441, 670],
            [927, 4, 394, 394],
            [[1041, 191, 66, 57], [1134, 190, 66, 57]],
            [-4., 4.],
            [1054, 290, 135, 50],
        ),
        AvatarKind::BlueJacketPresenter => (
            &BLUE_JACKET,
            illustrated,
            illustrated_expression,
            [96, 45, 388, 605],
            [116, 47, 350, 350],
            [[250, 154, 43, 27], [306, 153, 40, 27]],
            [0., 0.],
            [274, 204, 54, 29],
        ),
        _ => return None,
    };
    Some(cache.get_or_init(|| {
        let relative = |[x, y, width, height]: Region| [x - split[0], y - split[1], width, height];
        let decode = |image| {
            Pixmap::decode_png(image)
                .expect("valid bundled avatar illustration")
                .clone_rect(crop(split))
                .expect("avatar art within sheet")
        };
        Artwork {
            neutral: decode(neutral),
            expression: decode(expression),
            bubble: crop(relative(bubble)),
            eyes: eyes.map(relative),
            gaze,
            mouth: relative(mouth),
        }
    }))
}

fn center_gaze(neutral: &Pixmap, frame: &mut Pixmap, [x, y, width, height]: Region, shift: f32) {
    if shift == 0. {
        return;
    }
    let stride = neutral.width() as usize * 4;
    let source = neutral.data();
    let target = frame.data_mut();
    for row in 1..height - 1 {
        let vertical = (row as f32 * 2. / (height - 1) as f32 - 1.).abs();
        for col in 1..width - 1 {
            let horizontal = (col as f32 * 2. / (width - 1) as f32 - 1.).abs();
            let sample = (col as f32 - shift * (1. - horizontal.powi(3)) * (1. - vertical.powi(3)))
                .clamp(0., width as f32 - 1.001);
            let start = (y + row) as usize * stride + (x as usize + sample as usize) * 4;
            let offset = (y + row) as usize * stride + (x + col) as usize * 4;
            for channel in 0..3 {
                let a = f32::from(source[start + channel]);
                let b = f32::from(source[start + channel + 4]);
                target[offset + channel] = (a + (b - a) * sample.fract()).round() as u8;
            }
        }
    }
}

fn blend(
    expression: &Pixmap,
    frame: &mut Pixmap,
    [x, y, width, height]: Region,
    strength: f32,
    lid: bool,
) {
    if strength <= 0. {
        return;
    }
    let stride = expression.width() as usize * 4;
    let source = expression.data();
    let target = frame.data_mut();
    let feather = 5.;
    for row in 0..height {
        let weight = if lid {
            let edge = strength * (height as f32 + feather * 2.) - feather;
            ((edge - row as f32) / feather + 0.5).clamp(0., 1.)
        } else {
            strength
        };
        for col in 0..width {
            let distance = col.min(width - 1 - col).min(row).min(height - 1 - row);
            let alpha = (distance as f32 / feather).min(1.) * weight;
            let offset = (y + row) as usize * stride + (x + col) as usize * 4;
            for channel in 0..3 {
                let idx = offset + channel;
                let old = f32::from(target[idx]);
                target[idx] = (old + (f32::from(source[idx]) - old) * alpha).round() as u8;
            }
        }
    }
}

fn extend_background(frame: &Pixmap) -> Pixmap {
    let size = frame.height();
    let mut canvas = Pixmap::new(size, size).expect("valid avatar dimensions");
    let stride = frame.width() as usize * 4;
    let inset = ((size - frame.width()) / 2) as usize * 4;
    for (source, target) in frame
        .data()
        .chunks_exact(stride)
        .zip(canvas.data_mut().chunks_exact_mut(size as usize * 4))
    {
        let (left, rest) = target.split_at_mut(inset);
        let (center, right) = rest.split_at_mut(stride);
        for pixel in left.chunks_exact_mut(4) {
            pixel.copy_from_slice(&source[..4]);
        }
        center.copy_from_slice(source);
        for pixel in right.chunks_exact_mut(4) {
            pixel.copy_from_slice(&source[stride - 4..]);
        }
    }
    canvas
}

pub(super) fn draw(
    target: &mut Pixmap,
    kind: AvatarKind,
    pose: Pose,
    elapsed: f32,
    split: bool,
) -> bool {
    let Some(artwork) = artwork(kind) else {
        return false;
    };
    let mut frame = artwork.neutral.clone();
    for ((eye, shift), opening) in artwork
        .eyes
        .into_iter()
        .zip(artwork.gaze)
        .zip([pose.left_eye, pose.right_eye])
    {
        center_gaze(&artwork.neutral, &mut frame, eye, shift);
        let closed = ((0.52 - opening) / 0.41).clamp(0., 1.);
        blend(&artwork.expression, &mut frame, eye, closed, true);
    }
    let speaking = ((pose.mouth - 0.05) / 0.46).clamp(0., 1.);
    blend(
        &artwork.expression,
        &mut frame,
        artwork.mouth,
        speaking,
        false,
    );
    let frame = if split {
        extend_background(&frame)
    } else {
        frame
            .clone_rect(artwork.bubble)
            .expect("avatar bubble within portrait")
    };

    let size = target.height() as f32;
    let background = frame.pixel(0, 0).expect("avatar background").demultiply();
    target.fill(Color::from_rgba8(
        background.red(),
        background.green(),
        background.blue(),
        255,
    ));
    let scale = size * 1.045 / frame.height() as f32;
    let width = frame.width() as f32 * scale;
    let pivot = size * 0.5;
    let transform = Transform::from_scale(scale, scale)
        .post_translate((size - width) * 0.5, -size * 0.0225)
        .post_concat(Transform::from_rotate_at(
            pose.roll.to_degrees() * 0.15,
            pivot,
            pivot,
        ))
        .post_translate(pose.yaw * 9., pose.pitch * 7. + (elapsed * 1.7).sin() * 1.3);
    target.draw_pixmap(
        0,
        0,
        frame.as_ref(),
        &PixmapPaint {
            quality: FilterQuality::Bilinear,
            ..PixmapPaint::default()
        },
        transform,
        None,
    );
    true
}
