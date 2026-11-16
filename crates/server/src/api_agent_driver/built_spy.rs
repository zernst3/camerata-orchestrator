//! A test-only record of every driver that was built with a run's controls: who it is (its
//! read-log label) and which read log and spend meter it holds. A test builds a lead and a
//! child through the real factories and asks whether they hold the run's handles.

use std::sync::Mutex;

use super::{ApiAgentDriver, ReadLog, SpendMeter};

pub(crate) struct Built {
    pub label: String,
    pub read_log: ReadLog,
    pub spend: SpendMeter,
}

static BUILT: Mutex<Vec<Built>> = Mutex::new(Vec::new());

pub(super) fn record(driver: &ApiAgentDriver) {
    BUILT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(Built {
            label: driver.agent_label.clone(),
            read_log: driver.read_log(),
            spend: driver.spend_meter(),
        });
}

/// The labels of the built drivers holding `log`, each paired with whether it also holds `meter`.
pub(crate) fn holding(log: &ReadLog, meter: &SpendMeter) -> Vec<(String, bool)> {
    BUILT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .filter(|b| b.read_log.same_log(log))
        .map(|b| (b.label.clone(), b.spend.same_meter(meter)))
        .collect()
}
