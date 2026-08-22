//! Supplemental documentation modules that ship alongside the API
//! reference.

// Document bodies live in `docs/` as Markdown and are pulled in with
// `include_str!` so this file stays a thin router.

/// Notes on the intentional differences and the known corner cases
/// between erl_pp and OTP `epp`.
#[doc = include_str!("../docs/otp-differences.md")]
pub mod otp_differences {}

/// Task-oriented recipes for building preprocessor drivers.
#[doc = include_str!("../docs/recipes.md")]
pub mod recipes {}
