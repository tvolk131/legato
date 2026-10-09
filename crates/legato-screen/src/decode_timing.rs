//! CPU wall times for the Windows decoder. GPU wait includes queued decoding and
//! the staging copy; it is not a GPU execution timer or a pure transfer benchmark.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const EVERY: Duration = Duration::from_secs(5);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Timings {
    pub total: Duration,
    /// Input sample preparation and ProcessInput (including a retry if necessary).
    pub input: Duration,
    /// Time inside ProcessOutput, including stream-change/need-more-input calls.
    pub output: Duration,
    /// CopySubresourceRegion through blocking Map. May wait for unfinished decode.
    pub gpu_wait: Duration,
    /// Allocating and copying the visible NV12 rows into CPU memory.
    pub cpu_copy: Duration,
}

impl Timings {
    fn add(&mut self, sample: Self) {
        self.total += sample.total;
        self.input += sample.input;
        self.output += sample.output;
        self.gpu_wait += sample.gpu_wait;
        self.cpu_copy += sample.cpu_copy;
    }

    fn max(&mut self, sample: Self) {
        self.total = self.total.max(sample.total);
        self.input = self.input.max(sample.input);
        self.output = self.output.max(sample.output);
        self.gpu_wait = self.gpu_wait.max(sample.gpu_wait);
        self.cpu_copy = self.cpu_copy.max(sample.cpu_copy);
    }

    fn mean(self, count: u32) -> Self {
        Self {
            total: self.total / count,
            input: self.input / count,
            output: self.output / count,
            gpu_wait: self.gpu_wait / count,
            cpu_copy: self.cpu_copy / count,
        }
    }

    fn other(self) -> Duration {
        self.total
            .saturating_sub(self.input + self.output + self.gpu_wait + self.cpu_copy)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Stream {
    pub hardware: bool,
    pub width: u32,
    pub height: u32,
}

pub(crate) struct Profiler {
    id: u64,
    window: Option<Window>,
}

impl Default for Profiler {
    fn default() -> Self {
        Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            window: None,
        }
    }
}

struct Window {
    stream: Stream,
    since: Instant,
    inputs: u32,
    pictures: u32,
    sum: Timings,
    worst: Timings,
}

impl Window {
    fn new(stream: Stream, since: Instant) -> Self {
        Self {
            stream,
            since,
            inputs: 0,
            pictures: 0,
            sum: Timings::default(),
            worst: Timings::default(),
        }
    }
}

pub(crate) struct Report {
    id: u64,
    stream: Stream,
    inputs: u32,
    pictures: u32,
    mean: Timings,
    worst: Timings,
}

impl Profiler {
    /// One successful live decode call. Startup self-checks and failed calls never
    /// reach here. Keep tracks separate and discard a partial window on resize.
    pub fn record(
        &mut self,
        stream: Stream,
        sample: Timings,
        pictures: u32,
        now: Instant,
    ) -> Option<Report> {
        let window = self.window.get_or_insert_with(|| Window::new(stream, now));
        if window.stream != stream {
            *window = Window::new(stream, now);
        }
        window.inputs += 1;
        window.pictures += pictures;
        window.sum.add(sample);
        window.worst.max(sample);
        if now.duration_since(window.since) < EVERY {
            return None;
        }
        let window = self.window.take().unwrap();
        Some(Report {
            id: self.id,
            stream,
            inputs: window.inputs,
            pictures: window.pictures,
            mean: window.sum.mean(window.inputs),
            worst: window.worst,
        })
    }
}

impl Report {
    pub fn log(self) {
        let ms = |d: Duration| (d.as_secs_f64() * 1e6).round() / 1000.0;
        tracing::info!(
            decoder = self.id,
            backend = if self.stream.hardware {
                "D3D11"
            } else {
                "software"
            },
            width = self.stream.width,
            height = self.stream.height,
            inputs = self.inputs,
            pictures = self.pictures,
            total_ms = ms(self.mean.total),
            input_ms = ms(self.mean.input),
            output_ms = ms(self.mean.output),
            gpu_wait_ms = ms(self.mean.gpu_wait),
            cpu_copy_ms = ms(self.mean.cpu_copy),
            other_ms = ms(self.mean.other()),
            worst_total_ms = ms(self.worst.total),
            worst_input_ms = ms(self.worst.input),
            worst_output_ms = ms(self.worst.output),
            worst_gpu_wait_ms = ms(self.worst.gpu_wait),
            worst_cpu_copy_ms = ms(self.worst.cpu_copy),
            "H.264 decode timing (CPU wall time; GPU wait includes queued decode/copy)"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STREAM: Stream = Stream {
        hardware: true,
        width: 3840,
        height: 2160,
    };

    fn sample(total: u64, gpu_wait: u64) -> Timings {
        Timings {
            total: Duration::from_millis(total),
            input: Duration::from_millis(1),
            output: Duration::from_millis(2),
            gpu_wait: Duration::from_millis(gpu_wait),
            cpu_copy: Duration::from_millis(3),
        }
    }

    #[test]
    fn reports_means_and_independent_worst_times_then_starts_a_fresh_window() {
        let now = Instant::now();
        let mut p = Profiler::default();
        assert!(p.record(STREAM, sample(20, 10), 1, now).is_none());
        assert!(
            p.record(STREAM, sample(30, 6), 2, now + EVERY / 2)
                .is_none()
        );
        let report = p.record(STREAM, sample(10, 2), 0, now + EVERY).unwrap();
        assert_eq!((report.inputs, report.pictures), (3, 3));
        assert_eq!(report.mean.total, Duration::from_millis(20));
        assert_eq!(report.mean.gpu_wait, Duration::from_millis(6));
        assert_eq!(report.mean.other(), Duration::from_millis(8));
        assert_eq!(report.worst.total, Duration::from_millis(30));
        assert_eq!(report.worst.gpu_wait, Duration::from_millis(10));
        assert!(p.record(STREAM, sample(9, 1), 1, now + EVERY * 2).is_none());
        let next = p.record(STREAM, sample(9, 1), 1, now + EVERY * 3).unwrap();
        assert_eq!(next.id, report.id);
        assert_eq!(next.inputs, 2);
        assert_eq!(next.worst.total, Duration::from_millis(9));
    }

    #[test]
    fn resize_and_backend_changes_do_not_mix_measurements() {
        let now = Instant::now();
        let mut p = Profiler::default();
        assert!(p.record(STREAM, sample(100, 90), 1, now).is_none());
        let resized = Stream {
            width: 1920,
            height: 1080,
            ..STREAM
        };
        assert!(p.record(resized, sample(10, 2), 1, now + EVERY).is_none());
        let software = Stream {
            hardware: false,
            ..resized
        };
        assert!(
            p.record(software, sample(8, 0), 1, now + EVERY * 2)
                .is_none()
        );
        let report = p
            .record(software, sample(8, 0), 1, now + EVERY * 3)
            .unwrap();
        assert_eq!(report.stream, software);
        assert_eq!(report.inputs, 2);
        assert_eq!(report.worst.total, Duration::from_millis(8));
        assert_eq!(report.mean.gpu_wait, Duration::ZERO);
    }
}
