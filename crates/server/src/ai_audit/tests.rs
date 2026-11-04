//! Tests for the AI audit, grouped into the modules below. Helpers shared between groups are
//! `pub(super)` in their group and glob-imported here.

use super::*;
use crate::llm::{LlmPort, LlmRequest, LlmResponse};
use crate::onboard::MergedLocation;

mod audit_repo;
mod deep_routing_regressions;
mod fix_generation;
mod merge;
mod poll_stack_batch;
mod severity_rule_cases;
mod verify_findings;
use audit_repo::*;
use fix_generation::*;
use poll_stack_batch::*;
use verify_findings::*;
