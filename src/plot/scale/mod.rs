//! Scale and guide types for ggsql visualization specifications
//!
//! This module defines scale and guide configuration for aesthetic mappings.

pub mod palettes;
mod scale_type;
mod types;

pub use scale_type::{
    Binned, Continuous, Date, DateTime, Discrete, Identity, ScaleType, ScaleTypeKind,
    ScaleTypeTrait, Time,
};
pub use types::{Guide, GuideType, OutputRange, Scale};
