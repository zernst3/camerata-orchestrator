//! Render tests for the scan screen, grouped into the modules below. Helpers shared between groups are
//! `pub(super)` in their group and glob-imported here.

use super::*;

mod confirmed_and_removals;
mod finding_context_and_banners;
mod kind_sort_group_header;
use finding_context_and_banners::*;
use kind_sort_group_header::*;
