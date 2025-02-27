#![allow(clippy::unused_unit)]

#[macro_use]
extern crate lazy_static;

use crate::solver::*;
use rosomaxa::algorithms::gsom::Coordinate;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use wasm_bindgen::prelude::*;

mod plots;
pub use self::plots::{draw_plots, Axes};

mod solver;
pub use self::solver::run_solver;

/// Specifies a data point type for 3D chart.
#[derive(Clone)]
pub struct DataPoint3D(f64, f64, f64);
/// Specifies a matrix data type.
pub type MatrixData = HashMap<Coordinate, f64>;

lazy_static! {
    /// Keeps track of data used by the solver population.
    static ref EXPERIMENT_DATA: Mutex<ExperimentData> = Mutex::new(ExperimentData::default());
}

/// Runs experiment.
#[wasm_bindgen]
pub fn run_experiment(function_name: &str, population_type: &str, x: f64, z: f64, generations: usize) {
    let selection_size = 8;
    let logger = Arc::new(|message: &str| {
        web_sys::console::log_1(&message.into());
    });

    run_solver(function_name, population_type, selection_size, vec![x, z], generations, logger)
}

/// Clears experiment data.
#[wasm_bindgen]
pub fn clear() {
    EXPERIMENT_DATA.lock().unwrap().clear()
}

/// Gets current (last) generation.
#[wasm_bindgen]
pub fn get_generation() -> usize {
    EXPERIMENT_DATA.lock().unwrap().generation
}
