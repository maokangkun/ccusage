//! dsh (DeepSeek Harness) adapter: reads per-step token usage from local dsh
//! session event logs.

use csusage_adapter_common::filter_loaded_entries_by_date;
use csusage_core::*;

mod loader;
mod paths;
mod report;

use crate::{
    PricingMap, Result, cli::AgentCommandArgs, print_json_or_jq, print_usage_table, sort_summaries,
    wants_json,
};

pub use loader::load_entries;
pub use report::{report_from_rows, summarize_entries};

/// Runs a dsh usage report for the requested report kind.
pub fn run(args: AgentCommandArgs) -> Result<()> {
    let shared = args.shared;
    let pricing = PricingMap::load_with_overrides(
        shared.offline,
        crate::log_level() != Some(0),
        shared.pricing_overrides.iter(),
    );
    let mut entries = load_entries(&shared, &pricing)?;
    filter_loaded_entries_by_date(&mut entries, &shared);
    let mut rows = summarize_entries(&entries, args.kind)?;
    sort_summaries(&mut rows, &shared.order, |row| {
        csusage_core::summary_period(row)
    });
    if wants_json(&shared) {
        return print_json_or_jq(
            report_from_rows(&rows, args.kind),
            shared.jq.as_deref(),
            shared.no_cost,
        );
    }
    print_usage_table(
        "DSH Token Usage Report",
        csusage_core::first_column(args.kind),
        &rows,
        &shared,
        false,
        None,
    )?;
    Ok(())
}
