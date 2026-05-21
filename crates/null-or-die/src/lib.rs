pub mod api {
    pub use null_or_die_rssp::*;
}

pub fn run() -> Result<(), String> {
    null_or_die_cli::run()
}

pub use null_or_die_core::{
    BiasCfg, BiasEstimate, BiasEstimateWithPlot, BiasKernel, BiasPlotData, BiasRuntime,
    BiasStreamCfg, BiasStreamEvent, GraphOrientation, KernelTarget, guess_paradigm,
    slot_abbreviation, slot_expansion,
};
