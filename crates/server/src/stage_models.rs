//! Which model each model-calling stage of a run ran on, as the run itself recorded it (#584).
//!
//! The client report's "How this was done" page lists every model stage the run used with the
//! model it ran on. That list must come from the run's own record, never from today's defaults
//! or settings, so a re-export of a saved run years later still names what actually ran.
//!
//! The per-call record is [`crate::stage_telemetry::StageUsage::models`] (the API backend's
//! served model, or the CLI backend's requested one) and [`crate::stage_telemetry::StageUsage::
//! modes`] (batch or live). That telemetry is operator-only and carries cost figures, so it is
//! never read by an exporter. This module is the client-facing copy of the part a client may
//! read: per stage, the model ids and the modes, and nothing else. The pipeline writes it on the
//! scan ledger at the same points it writes the telemetry ([`RecordedStageModels::from_telemetry`]
//! and [`RecordedStageModels::absorb`]), and the export reads only this.
//!
//! A ledger written before this record existed has `None`; the export then falls back to the
//! single-model fields of [`crate::onboard::ScanProvenance`] and says "not recorded" for every
//! stage it cannot name (`report_export::model_stages`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::stage_telemetry::{Stage, StageTelemetry};

/// One model stage of a run: the models its calls ran on and the modes they ran in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageModelRecord {
    pub stage: Stage,
    /// Calls per model, keyed as [`crate::stage_telemetry::StageUsage::models`] keys them (a CLI
    /// call's key ends ` (requested)`).
    pub models: BTreeMap<String, u64>,
    /// Calls per mode (`batch`, `live`). Empty when the telemetry the record was copied from did
    /// not record modes.
    #[serde(default)]
    pub modes: BTreeMap<String, u64>,
}

/// Every model stage a run made at least one call in, in pipeline order ([`Stage::ALL`]). An
/// empty list is a run that made no model call.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedStageModels {
    pub stages: Vec<StageModelRecord>,
    /// Findings the fix stage asked the model for that ended on the rule's standard fix (the
    /// model's fix never passed the self-check, the spend limit stopped it or every call failed).
    /// `None` when the fix stage made no call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub standard_fix_fallbacks: Option<u64>,
}

impl RecordedStageModels {
    /// The client-facing part of a telemetry snapshot: each stage's models and modes, and the
    /// fix stage's fallbacks.
    pub fn from_telemetry(telemetry: &StageTelemetry) -> Self {
        let stages: Vec<StageModelRecord> = telemetry
            .stages
            .iter()
            .filter(|s| s.usage.calls > 0)
            .map(|s| StageModelRecord {
                stage: s.stage,
                models: s.usage.models.clone(),
                modes: s.usage.modes.clone(),
            })
            .collect();
        let fix_ran = stages.iter().any(|s| s.stage == Stage::FixGeneration);
        let standard_fix_fallbacks = fix_ran.then(|| {
            telemetry.fix_outcome.map_or(0, |o| {
                o.gave_up
                    .saturating_add(o.cap_stopped)
                    .saturating_add(o.transport_failed)
            })
        });
        Self {
            stages,
            standard_fix_fallbacks,
        }
    }

    /// Fold a later stage's telemetry (the group review, the duplicate check) into this record:
    /// a stage the fresh snapshot carries replaces the same stage here, a new one is added, and
    /// the list stays in pipeline order.
    pub fn absorb(&mut self, fresh: &StageTelemetry) {
        let fresh = Self::from_telemetry(fresh);
        for record in fresh.stages {
            match self.stages.iter_mut().find(|s| s.stage == record.stage) {
                Some(have) => *have = record,
                None => self.stages.push(record),
            }
        }
        self.stages
            .sort_by_key(|s| Stage::ALL.iter().position(|a| *a == s.stage));
        if fresh.standard_fix_fallbacks.is_some() {
            self.standard_fix_fallbacks = fresh.standard_fix_fallbacks;
        }
    }

    /// The record of `stage`, when the run made a call in it.
    pub fn stage(&self, stage: Stage) -> Option<&StageModelRecord> {
        self.stages.iter().find(|s| s.stage == stage)
    }
}

#[cfg(test)]
mod tests;
