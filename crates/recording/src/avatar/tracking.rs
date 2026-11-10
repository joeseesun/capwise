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
