//! Opt-in, bounded numeric observations. No paths, titles or arbitrary text.
use crate::metrics::Observation;
use std::time::Duration;

pub(crate) const INTERVAL_MS: u32 = 5_000;
pub(crate) const MAX_SAMPLES: usize = 5_762;
pub(crate) const MAX_SECONDS: u64 = 8 * 60 * 60;

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(crate) enum Scenario {
    #[default]
    Daily,
    Idle,
    Windows,
    Update,
}
impl Scenario {
    pub(crate) fn index(self) -> usize {
        match self {
            Self::Daily => 0,
            Self::Idle => 1,
            Self::Windows => 2,
            Self::Update => 3,
        }
    }
    pub(crate) fn label(self) -> &'static str {
        crate::i18n::text(match self {
            Self::Daily => "日常使用",
            Self::Idle => "空闲",
            Self::Windows => "窗口操作",
            Self::Update => "版本更新",
        })
    }

    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Daily => "daily-use",
            Self::Idle => "idle",
            Self::Windows => "window-activity",
            Self::Update => "update",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Timestamp([u16; 6]);
impl Timestamp {
    pub(crate) fn now() -> Self {
        let time = unsafe { windows::Win32::System::SystemInformation::GetSystemTime() };
        Self([
            time.wYear,
            time.wMonth,
            time.wDay,
            time.wHour,
            time.wMinute,
            time.wSecond,
        ])
    }
    fn text(self) -> String {
        let [year, month, day, hour, minute, second] = self.0;
        format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stop {
    User,
    Closed,
    Limit,
}
#[derive(Clone, Copy)]
struct Point {
    elapsed: Duration,
    observation: Observation,
}
pub(crate) struct Recording {
    scenario: Scenario,
    start: Timestamp,
    observed: Timestamp,
    points: Vec<Point>,
    stop: Option<Stop>,
    summary: Option<Snapshot>,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Stats {
    pub valid: usize,
    pub median: f64,
    pub p95: u64,
    pub peak: u64,
    pub delta: Option<i128>,
}
#[derive(Clone, Copy)]
pub(crate) struct Snapshot {
    scenario: Scenario,
    start: Timestamp,
    observed: Timestamp,
    pub elapsed: Duration,
    pub count: usize,
    pub stopped: Option<Stop>,
    pub stats: [Option<Stats>; 5],
}
impl Recording {
    pub(crate) fn new(
        scenario: Scenario,
        time: Timestamp,
        sample: impl FnOnce() -> Observation,
    ) -> Self {
        let mut points = Vec::with_capacity(MAX_SAMPLES);
        points.push(Point {
            elapsed: Duration::ZERO,
            observation: sample(),
        });
        Self {
            scenario,
            start: time,
            observed: time,
            points,
            stop: None,
            summary: None,
        }
    }
    pub(crate) fn active(&self) -> bool {
        self.stop.is_none()
    }
    pub(crate) fn scenario(&self) -> Scenario {
        self.scenario
    }
    pub(crate) fn count(&self) -> usize {
        self.summary
            .as_ref()
            .map_or(self.points.len(), |summary| summary.count)
    }
    pub(crate) fn reason(&self) -> Option<Stop> {
        self.stop
    }
    pub(crate) fn append(&mut self, elapsed: Duration, time: Timestamp, observation: Observation) {
        if !self.active() || self.points.last().is_some_and(|p| elapsed < p.elapsed) {
            return;
        }
        if elapsed > Duration::from_secs(MAX_SECONDS) {
            self.stop(Stop::Limit);
            return;
        }
        if self.points.len() < MAX_SAMPLES {
            self.points.push(Point {
                elapsed,
                observation,
            });
            self.observed = time;
        }
        if self.points.len() == MAX_SAMPLES || elapsed.as_secs() >= MAX_SECONDS {
            self.stop(Stop::Limit);
        }
    }
    pub(crate) fn stop(&mut self, reason: Stop) {
        if self.active() {
            self.stop = Some(reason);
            // There is no resume/raw export action: retain the exact summary
            // while releasing the full sample buffer as soon as recording ends.
            self.summary = Some(self.snapshot());
            self.points = Vec::new();
        }
    }
    pub(crate) fn snapshot(&self) -> Snapshot {
        if let Some(summary) = self.summary {
            return summary;
        }
        let stats = std::array::from_fn(|index| {
            let mut values: Vec<_> = self
                .points
                .iter()
                .filter_map(|p| p.observation.values[index])
                .collect();
            if values.is_empty() {
                return None;
            }
            values.sort_unstable();
            let count = values.len();
            let median = if count % 2 == 0 {
                (values[count / 2 - 1] as f64 + values[count / 2] as f64) / 2.0
            } else {
                values[count / 2] as f64
            };
            let first = self
                .points
                .first()
                .and_then(|p| p.observation.values[index]);
            let last = self.points.last().and_then(|p| p.observation.values[index]);
            Some(Stats {
                valid: count,
                median,
                p95: values[(count * 95).div_ceil(100) - 1],
                peak: *values.last().unwrap(),
                delta: first
                    .zip(last)
                    .map(|(first, last)| last as i128 - first as i128),
            })
        });
        Snapshot {
            scenario: self.scenario,
            start: self.start,
            observed: self.observed,
            elapsed: self.points.last().unwrap().elapsed,
            count: self.points.len(),
            stopped: self.stop,
            stats,
        }
    }
}
impl Snapshot {
    pub(crate) fn display(&self) -> String {
        let (start, last) = self.times();
        let scenario = self.scenario.label();
        let mut text = crate::ui_format!(
            "场景：{scenario}\n开始（UTC）：{start}\n最新（UTC）：{last}\n已记录 {} 个样本 · {:.0} 秒\n",
            "Scenario: {scenario}\nStarted (UTC): {start}\nLast sample (UTC): {last}\n{} samples · {:.0} seconds\n",
            self.count,
            self.elapsed.as_secs_f64()
        );
        for (index, (zh, en)) in [
            ("私有提交（MiB）", "Private commit (MiB)"),
            ("工作集（MiB）", "Working set (MiB)"),
            ("句柄", "Handles"),
            ("GDI 对象", "GDI objects"),
            ("USER 对象", "USER objects"),
        ]
        .iter()
        .enumerate()
        {
            let name = if crate::i18n::language() == crate::i18n::Language::En {
                en
            } else {
                zh
            };
            if let Some(stats) = self.stats[index] {
                let scale = if index < 2 { 1024.0 * 1024.0 } else { 1.0 };
                let delta = stats.delta.map_or_else(
                    || "—".into(),
                    |delta| format!("{:+.2}", delta as f64 / scale),
                );
                text.push_str(&format!("{name}  ({}/{})\n", stats.valid, self.count));
                text.push_str(&crate::ui_format!(
                    "中位 {:.2}   P95 {:.2}   峰值 {:.2}   变化 {delta}\n",
                    "Median {:.2}   P95 {:.2}   Peak {:.2}   Change {delta}\n",
                    stats.median / scale,
                    stats.p95 as f64 / scale,
                    stats.peak as f64 / scale
                ));
            } else {
                text.push_str(&crate::ui_format!(
                    "{name}：暂不可用\n",
                    "{name}: unavailable\n"
                ));
            }
        }
        text.push_str(&crate::ui_format!(
            "\n包含诊断自身开销；请结合使用场景理解变化。",
            "\nIncludes diagnostic overhead; interpret changes in context."
        ));
        text
    }
    /// The only export formatter accepts typed numbers/enums, never UI text.
    pub(crate) fn markdown(&self) -> String {
        let state = match self.stopped {
            None => "recording-snapshot",
            Some(Stop::User) => "stopped",
            Some(Stop::Closed) => "window-closed",
            Some(Stop::Limit) => "limit-reached",
        };
        let mut text = format!(
            "# minicon-winmgr diagnostic summary\n\nVersion: {}\nScenario: {}\nStarted (UTC): {}\nLast sample (UTC): {}\nElapsed milliseconds: {}\nSamples: {}\nNominal interval milliseconds: {}\nState: {state}\n\n| Metric | Median | P95 | Peak | Last - first | Valid / total |\n|---|---:|---:|---:|---:|---:|\n",
            env!("CARGO_PKG_VERSION"),
            self.scenario.code(),
            self.start.text(),
            self.observed.text(),
            self.elapsed.as_millis(),
            self.count,
            INTERVAL_MS
        );
        for (index, name) in [
            "Private commit (bytes)",
            "Working set (bytes)",
            "Handles",
            "GDI objects",
            "USER objects",
        ]
        .iter()
        .enumerate()
        {
            if let Some(stats) = self.stats[index] {
                let delta = stats
                    .delta
                    .map_or_else(|| "unavailable".into(), |n| n.to_string());
                text.push_str(&format!(
                    "| {name} | {:.1} | {} | {} | {delta} | {} / {} |\n",
                    stats.median, stats.p95, stats.peak, stats.valid, self.count
                ));
            } else {
                text.push_str(&format!(
                    "| {name} | unavailable | unavailable | unavailable | unavailable | 0 / {} |\n",
                    self.count
                ));
            }
        }
        text.push_str("\nSelf-observation includes diagnostic overhead. Working set and private commit are different metrics. Missing values remain unavailable; deltas require both endpoints. UTC follows the system clock; elapsed time is monotonic. Timer delays may create sampling gaps. No automatic stability or leak verdict.\n");
        text
    }
    pub(crate) fn times(&self) -> (String, String) {
        (self.start.text(), self.observed.text())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const TIME: Timestamp = Timestamp([2026, 10, 16, 0, 0, 0]);
    #[test]
    fn missing_metrics_stay_missing_and_endpoints_are_not_invented() {
        let mut record = Recording::new(Scenario::Idle, TIME, || Observation {
            values: [None, Some(0), Some(5), None, None],
        });
        record.append(
            Duration::from_secs(5),
            TIME,
            Observation {
                values: [Some(100), Some(10), Some(1), None, None],
            },
        );
        record.append(
            Duration::from_secs(10),
            TIME,
            Observation {
                values: [Some(200), Some(20), Some(9), None, None],
            },
        );
        let snapshot = record.snapshot();
        let private = snapshot.stats[0].unwrap();
        assert_eq!(private.valid, 2);
        assert_eq!(private.median, 150.0);
        assert_eq!(private.p95, 200);
        assert_eq!(private.delta, None);
        assert_eq!(snapshot.stats[1].unwrap().delta, Some(20));
        assert!(snapshot.stats[3].is_none());
        let exported = snapshot.markdown();
        assert!(exported.contains("unavailable"));
        assert!(!exported.contains("USERPROFILE"));
        assert!(!exported.contains("pid"));
        assert!(exported.contains("recording-snapshot"));
    }
    #[test]
    fn capacity_duration_stop_and_backward_time_do_not_extend_a_recording() {
        let mut record = Recording::new(Scenario::Daily, TIME, Observation::default);
        for n in 1..MAX_SAMPLES + 20 {
            record.append(
                Duration::from_millis(n as u64),
                TIME,
                Observation::default(),
            );
        }
        assert_eq!(record.count(), MAX_SAMPLES);
        assert_eq!(record.points.capacity(), 0);
        assert_eq!(record.stop, Some(Stop::Limit));
        let mut record = Recording::new(Scenario::Update, TIME, Observation::default);
        record.append(Duration::from_secs(10), TIME, Observation::default());
        record.append(Duration::from_secs(5), TIME, Observation::default());
        assert_eq!(record.points.len(), 2);
        record.append(
            Duration::from_secs(MAX_SECONDS),
            TIME,
            Observation::default(),
        );
        assert!(!record.active());
        let mut record = Recording::new(Scenario::Windows, TIME, Observation::default);
        record.stop(Stop::Closed);
        record.append(Duration::from_secs(5), TIME, Observation::default());
        assert_eq!(record.count(), 1);
        assert!(record.snapshot().markdown().contains("window-closed"));
    }
    #[test]
    fn stopping_releases_samples_without_changing_summary_values() {
        let mut record = Recording::new(Scenario::Idle, TIME, || Observation {
            values: [Some(20), Some(0), Some(5), None, None],
        });
        record.append(
            Duration::from_secs(5),
            TIME,
            Observation {
                values: [Some(40), Some(10), Some(1), None, None],
            },
        );
        let before = record.snapshot();
        record.stop(Stop::User);
        let after = record.snapshot();
        assert_eq!(record.points.capacity(), 0);
        assert_eq!(record.count(), 2);
        assert_eq!(before.stats, after.stats);
        assert_eq!(before.elapsed, after.elapsed);
        let mut delayed = Recording::new(Scenario::Daily, TIME, Observation::default);
        delayed.append(
            Duration::from_secs(MAX_SECONDS + 1),
            TIME,
            Observation::default(),
        );
        assert_eq!(delayed.count(), 1);
        assert_eq!(delayed.snapshot().elapsed, Duration::ZERO);
        assert_eq!(delayed.points.capacity(), 0);
    }
}
