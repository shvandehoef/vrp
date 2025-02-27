use rosomaxa::evolution::TelemetryMode;
use rosomaxa::example::*;
use rosomaxa::population::*;
use rosomaxa::prelude::*;
use std::ops::Deref;
use std::sync::Arc;

mod objectives;
pub use self::objectives::*;

mod proxies;
pub use self::proxies::*;

mod state;
pub use self::state::*;

/// Runs the solver to minimize objective function with given name.
pub fn run_solver(
    function_name: &str,
    population_type: &str,
    selection_size: usize,
    init_solution: Vec<f64>,
    generations: usize,
    logger: InfoLogger,
) {
    let fitness_fn = get_fitness_fn_by_name(function_name);
    let random = Arc::new(DefaultRandom::default());

    let noise_op = VectorHeuristicOperatorMode::JustNoise(Noise::new(1., (-0.1, 0.1), random));
    let delta_op = VectorHeuristicOperatorMode::JustDelta(-0.1..0.1);

    let (solutions, _) = Solver::default()
        .use_dynamic_heuristic_only()
        .with_logger(logger.clone())
        .with_init_solutions(vec![init_solution])
        .with_operator(noise_op, "noise", 1.)
        .with_operator(delta_op, "delta", 0.2)
        .with_termination(None, Some(generations), None, None)
        .with_fitness_fn(fitness_fn)
        .with_context_factory(Box::new({
            let logger = logger.clone();
            let population_type = population_type.to_string();
            move |objective, environment| {
                let population =
                    get_population(&population_type, objective.clone(), environment.clone(), selection_size);
                let telemetry_mode =
                    TelemetryMode::OnlyLogging { logger, log_best: 100, log_population: 500, dump_population: false };
                VectorContext::new(objective, population, telemetry_mode, environment)
            }
        }))
        .solve()
        .expect("no solutions");

    let (individual, fitness) = solutions.first().expect("empty solutions");

    logger.deref()(&format!("solution: {:?}, fitness: {}", individual, fitness));
}

fn get_population(
    population_type: &str,
    objective: Arc<VectorObjective>,
    environment: Arc<Environment>,
    selection_size: usize,
) -> Box<VectorPopulation> {
    match population_type {
        "greedy" => Box::new(ProxyPopulation::new(Greedy::new(objective, 1, None))),
        "elitism" => {
            Box::new(ProxyPopulation::new(Elitism::new(objective, environment.random.clone(), 2, selection_size)))
        }
        "rosomaxa" => Box::new(ProxyPopulation::new(
            Rosomaxa::new(objective, environment, RosomaxaConfig::new_with_defaults(selection_size))
                .expect("cannot create rosomaxa with default configuration"),
        )),
        _ => unreachable!(),
    }
}
