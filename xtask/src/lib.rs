//! One fail-closed quality policy for local and CI execution.
pub mod ci;
pub mod contracts;
pub mod coupling;
pub mod driver;
pub mod environment;
pub mod inventory;
pub mod ledger;
pub mod macro_policy;
pub mod measurements;
pub mod metrics;
mod module_bindings;
pub mod modules;
mod quote_identity;
pub mod scan;
pub mod suppression;
pub mod type_aggregate;
