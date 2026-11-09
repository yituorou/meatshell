//! Speed estimates alongside the existing transfer model; owns no transfer rows.

use super::transfer_rate::TransferRate;
use crate::resource::system::format_bytes_per_sec;
use crate::ui::{AppWindow, TransferInfo};
use slint::{ComponentHandle, Model, SharedString, VecModel};
use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    time::{Duration, Instant},
};

pub(super) struct TransferSpeeds {
    samples: RefCell<HashMap<String, TransferRate>>,
    timer: slint::Timer,
    window: slint::Weak<AppWindow>,
}

impl TransferSpeeds {
    pub(super) fn new(window: &AppWindow) -> Rc<Self> {
        Rc::new(Self {
            samples: RefCell::new(HashMap::new()),
            timer: slint::Timer::default(),
            window: window.as_weak(),
        })
    }

    pub(super) fn observe(
        self: &Rc<Self>,
        id: &str,
        bytes: u64,
        state: u8,
        at: Option<Instant>,
    ) -> SharedString {
        let now = Instant::now();
        let speed = {
            let mut samples = self.samples.borrow_mut();
            if let (0, Some(at)) = (state, at) {
                let rate = samples
                    .entry(id.to_string())
                    .or_insert_with(|| TransferRate::new(bytes, at));
                rate.observe(bytes, at);
                speed_text(rate, now)
            } else {
                samples.remove(id);
                SharedString::default()
            }
        };
        if self.needs_refresh(now) {
            if !self.timer.running() {
                let weak = Rc::downgrade(self);
                self.timer.start(
                    slint::TimerMode::Repeated,
                    Duration::from_millis(500),
                    move || {
                        if let Some(speeds) = weak.upgrade() {
                            speeds.tick(Instant::now());
                        }
                    },
                );
            }
        } else if self.timer.running() {
            self.tick(now);
        }
        speed
    }

    fn needs_refresh(&self, now: Instant) -> bool {
        self.samples
            .borrow()
            .values()
            .any(|rate| rate.needs_refresh(now))
    }

    fn tick(&self, now: Instant) {
        let Some(window) = self.window.upgrade() else {
            self.clear();
            return;
        };
        let model = window.get_transfers();
        if let Some(model) = model.as_any().downcast_ref::<VecModel<TransferInfo>>() {
            let samples = self.samples.borrow();
            for index in 0..model.row_count() {
                let Some(mut row) = model.row_data(index) else {
                    continue;
                };
                if row.state != 0 {
                    continue;
                }
                if let Some(rate) = samples.get(row.id.as_str()) {
                    let speed = speed_text(rate, now);
                    if row.speed != speed {
                        row.speed = speed;
                        model.set_row_data(index, row);
                    }
                }
            }
        }
        if !self.needs_refresh(now) {
            // Keep the last real baseline for slow transfers; only stop waking
            // the UI. A new progress event can restart the timer.
            self.timer.stop();
        }
    }

    pub(super) fn clear(&self) {
        self.timer.stop();
        self.samples.borrow_mut().clear();
    }
}

fn speed_text(rate: &TransferRate, now: Instant) -> SharedString {
    rate.bytes_per_second(now)
        .map(format_bytes_per_sec)
        .unwrap_or_else(|| "—".into())
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};

    struct Backend;
    impl slint::platform::Platform for Backend {
        fn create_window_adapter(
            &self,
        ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
            Ok(MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer))
        }
    }

    #[test]
    fn speed_refresh_preserves_rows_and_stops_when_stale_finished_or_cleared() {
        std::thread::spawn(|| {
            slint::platform::set_platform(Box::new(Backend)).unwrap();
            let window = AppWindow::new().unwrap();
            let model = Rc::new(VecModel::from(vec![TransferInfo {
                id: "file".into(),
                name: "example.zip".into(),
                detail: "13 MB/29 MB".into(),
                percent: 0.45,
                state: 0,
                ..Default::default()
            }]));
            window.set_transfers(model.clone().into());
            let speeds = TransferSpeeds::new(&window);
            let now = Instant::now();
            assert_eq!(
                speeds
                    .observe("file", 0, 0, Some(now - Duration::from_secs(1)))
                    .as_str(),
                "—"
            );
            assert!(!speeds.timer.running());
            speeds.observe("file", 2048, 0, Some(now));
            assert!(speeds.timer.running());
            speeds.tick(now);
            let row = model.row_data(0).unwrap();
            assert_eq!(row.speed.as_str(), "2.0 KB/s");
            assert_eq!(
                (
                    row.name.as_str(),
                    row.detail.as_str(),
                    row.percent,
                    row.state
                ),
                ("example.zip", "13 MB/29 MB", 0.45, 0)
            );
            speeds.tick(now + Duration::from_secs(3));
            assert_eq!(model.row_data(0).unwrap().speed.as_str(), "—");
            assert!(!speeds.timer.running());
            assert_eq!(speeds.samples.borrow().len(), 1);
            speeds.observe("file", 4096, 0, Some(Instant::now()));
            assert!(speeds.timer.running());
            // Status and history remain the existing event handler's responsibility.
            for state in [1, 2, 3, 4] {
                assert!(speeds
                    .observe("file", 4096, state, Some(Instant::now()))
                    .is_empty());
                assert!(!speeds.timer.running());
            }
            assert!(speeds.observe("zmodem", 1_000_000, 0, None).is_empty());
            speeds.observe("file", 0, 0, Some(now));
            speeds.clear();
            assert!(speeds.samples.borrow().is_empty());
            assert!(!speeds.timer.running());
            assert_eq!(model.row_count(), 1);
            let weak = Rc::downgrade(&speeds);
            drop(speeds);
            assert!(weak.upgrade().is_none());
        })
        .join()
        .unwrap();
    }
}
