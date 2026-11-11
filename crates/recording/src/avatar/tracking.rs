#[derive(Clone, Copy, Default)]
struct Axis {
    raw: f32,
    value: f32,
    velocity: f32,
}

impl Axis {
    fn update(&mut self, observed: f32, dt: f32) -> f32 {
        if !observed.is_finite() {
            return self.value;
        }
        let raw = observed.clamp(self.raw - 8. * dt, self.raw + 8. * dt);
        let velocity = (raw - self.raw) / dt;
        self.raw = raw;
        self.velocity += (velocity - self.velocity) * low_pass(1.3, dt);
        let cutoff = 1.45 + 2.2 * self.velocity.abs();
        self.value += (raw - self.value) * low_pass(cutoff, dt);
        self.value
    }
}

fn low_pass(hz: f32, dt: f32) -> f32 {
    let rate = std::f32::consts::TAU * hz * dt;
    rate / (1. + rate)
}

#[derive(Default)]
pub(super) struct EyeFilter {
    open_ratio: Option<f32>,
    last_sample: Option<(f64, f32)>,
}

impl EyeFilter {
    pub(super) fn update(&mut self, observed: Option<f32>, time: f64) -> f32 {
        let Some(ratio) = observed.filter(|ratio| (0.0..=0.48).contains(ratio)) else {
            return self
                .last_sample
                .filter(|(last, _)| (0.0..=0.12).contains(&(time - last)))
                .map_or(1., |(_, opening)| opening);
        };
        match &mut self.open_ratio {
            Some(reference) if ratio >= *reference * 0.82 => {
                let difference = (ratio - *reference).clamp(-0.025, 0.04);
                *reference = (*reference + difference * 0.06).clamp(0.13, 0.4);
            }
            None if ratio >= 0.11 => self.open_ratio = Some(ratio.clamp(0.13, 0.4)),
            _ => {}
        }
        let relative_opening = ratio / self.open_ratio.unwrap_or(0.24);
        let opening = ((relative_opening - 0.36) / 0.5).clamp(0., 1.);
        self.last_sample = Some((time, opening));
        opening
    }
}

#[derive(Default)]
pub(super) struct HeadFilter {
    axes: [Axis; 3],
    last_sample: Option<f64>,
}

impl HeadFilter {
    pub(super) fn update(
        &mut self,
        observed: [f32; 3],
        displayed: [f32; 3],
        time: f64,
    ) -> [f32; 3] {
        let dt = self
            .last_sample
            .replace(time)
            .map(|last| time - last)
            .filter(|dt| *dt > 0. && *dt <= 0.30);
        if dt.is_none() {
            self.axes = displayed.map(|value| Axis {
                raw: value,
                value,
                velocity: 0.,
            });
        }
        let dt = dt.unwrap_or(1. / 30.).clamp(1. / 120., 0.1) as f32;
        let [yaw, pitch, roll] = &mut self.axes;
        [
            yaw.update(observed[0].clamp(-1., 1.), dt),
            pitch.update(observed[1].clamp(-1., 1.), dt),
            roll.update(observed[2].clamp(-0.5, 0.5), dt),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eye_calibrates_at_rest_and_recovers_after_repeated_blinks() {
        for rest in [0.13, 0.19, 0.32] {
            let mut eye = EyeFilter::default();
            assert_eq!(eye.update(Some(rest), 0.), 1.);
            for frame in 1..=30 {
                let time = f64::from(frame) / 30.;
                assert_eq!(eye.update(Some(rest * 0.28), time), 0.);
                assert_eq!(eye.update(Some(rest), time + 0.01), 1.);
            }
        }
    }

    #[test]
    fn eye_does_not_calibrate_from_a_closed_lid_or_invalid_landmarks() {
        let mut eye = EyeFilter::default();
        assert_eq!(eye.update(Some(0.05), 0.), 0.);
        assert_eq!(eye.update(Some(f32::NAN), 0.03), 0.);
        assert_eq!(eye.update(None, 0.10), 0.);
        assert_eq!(eye.update(None, 0.15), 1.);
        assert_eq!(eye.update(Some(0.17), 0.18), 1.);
    }

    #[test]
    fn a_wink_and_landmark_loss_do_not_move_the_other_eye() {
        let [mut left, mut right] = [EyeFilter::default(), EyeFilter::default()];
        assert_eq!(left.update(Some(0.17), 0.), 1.);
        assert_eq!(right.update(Some(0.23), 0.), 1.);
        assert_eq!(left.update(Some(0.04), 0.03), 0.);
        assert_eq!(right.update(Some(0.22), 0.03), 1.);
        assert_eq!(left.update(None, 0.06), 0.);
        assert_eq!(right.update(Some(0.23), 0.06), 1.);
        assert_eq!(left.update(Some(0.17), 0.09), 1.);
    }

    #[test]
    fn head_filter_reduces_stationary_jitter_and_follows_a_turn() {
        let mut filter = HeadFilter::default();
        let mut time = 0.;
        let mut peak = 0_f32;
        for frame in 0..60 {
            let noise = if frame % 2 == 0 { 0.03 } else { -0.03 };
            let [yaw, _, _] = filter.update([noise, 0., 0.], [0.; 3], time);
            if frame > 20 {
                peak = peak.max(yaw.abs());
            }
            time += 1. / 30.;
        }
        assert!(peak < 0.007, "stationary jitter: {peak}");
        let mut yaw = 0.;
        for _ in 0..6 {
            [yaw, _, _] = filter.update([0.75, 0., 0.], [0.; 3], time);
            time += 1. / 30.;
        }
        assert!(yaw > 0.65, "turn response: {yaw}");
        let [recovered, _, _] = filter.update([-0.75, 0., 0.], [0.35, 0., 0.], time + 1.);
        assert!(
            (recovered - 0.35).abs() < 0.25,
            "reacquisition: {recovered}"
        );
    }
}
