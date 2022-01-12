//! Contains functionality to run evolution simulation.

use crate::prelude::*;
use crate::utils::{Quota, Timer};
use std::sync::Arc;

mod config;
pub use self::config::*;

pub mod telemetry;
pub use self::telemetry::*;

/// Defines evolution result type.
pub type EvolutionResult<S> = Result<(Vec<S>, Option<TelemetryMetrics>), String>;

/// An evolution algorithm strategy.
pub trait EvolutionStrategy {
    /// A heuristic context type.
    type Context: HeuristicContext<Objective = Self::Objective, Solution = Self::Solution>;
    /// A heuristic objective type.
    type Objective: HeuristicObjective<Solution = Self::Solution>;
    /// A solution type.
    type Solution: HeuristicSolution;

    /// Runs evolution and returns a population with solution(-s).
    fn run(self, heuristic_ctx: Self::Context) -> EvolutionResult<Self::Solution>;
}

/// A simple evolution algorithm which maintains single population.
pub struct RunSimple<E, C, O, S>
where
    E: EvolutionStrategy<Context = C, Objective = O, Solution = S> + 'static,
    C: HeuristicContext<Objective = O, Solution = S>,
    O: HeuristicObjective<Solution = S>,
    S: HeuristicSolution,
{
    config: EvolutionConfig<E, C, O, S>,
}

impl<E, C, O, S> EvolutionStrategy for RunSimple<E, C, O, S>
where
    E: EvolutionStrategy<Context = C, Objective = O, Solution = S> + 'static,
    C: HeuristicContext<Objective = O, Solution = S>,
    O: HeuristicObjective<Solution = S>,
    S: HeuristicSolution,
{
    type Context = C;
    type Objective = O;
    type Solution = S;

    fn run(self, heuristic_ctx: Self::Context) -> EvolutionResult<S> {
        let mut heuristic_ctx = heuristic_ctx;
        let mut config = self.config;

        while !should_stop(&mut heuristic_ctx, config.termination.as_ref()) {
            let generation_time = Timer::start();

            let parents = heuristic_ctx.population().select().collect();

            let offspring = config.heuristic.search(&heuristic_ctx, parents);

            let is_improved = if should_add_solution(&config.environment.quota, config.population.as_ref()) {
                heuristic_ctx.population_mut().add_all(offspring)
            } else {
                false
            };

            on_generation(
                &mut heuristic_ctx,
                &mut config.telemetry,
                config.termination.as_ref(),
                generation_time,
                is_improved,
            );
        }

        config.telemetry.on_result(&heuristic_ctx);

        let solutions = heuristic_ctx
            .population()
            .ranked()
            .map(|(solution, _)| solution.deep_copy())
            .take(config.desired_amount)
            .collect();

        Ok((solutions, config.telemetry.take_metrics()))
    }
}

/// An entity which simulates evolution process.
pub struct EvolutionSimulator<E, C, O, S, F>
where
    E: EvolutionStrategy<Context = C, Objective = O, Solution = S> + 'static,
    C: HeuristicContext<Objective = O, Solution = S>,
    O: HeuristicObjective<Solution = S>,
    S: HeuristicSolution,
    F: FnOnce(Box<dyn HeuristicPopulation<Objective = O, Individual = S>>) -> C,
{
    config: EvolutionConfig<E, C, O, S>,
    context_factory: F,
}

impl<E, C, O, S, F> EvolutionSimulator<E, C, O, S, F>
where
    E: EvolutionStrategy<Context = C, Objective = O, Solution = S> + 'static,
    C: HeuristicContext<Objective = O, Solution = S>,
    O: HeuristicObjective<Solution = S>,
    S: HeuristicSolution,
    F: FnOnce(Box<dyn HeuristicPopulation<Objective = O, Individual = S>>) -> C,
{
    /// Creates a new instance of `EvolutionSimulator`.
    pub fn new(config: EvolutionConfig<E, C, O, S>, context_factory: F) -> Result<Self, String> {
        if config.initial.operators.is_empty() {
            return Err("at least one initial method has to be specified".to_string());
        }

        Ok(Self { config, context_factory })
    }

    /// Runs evolution for given `problem` using evolution `config`.
    /// Returns populations filled with solutions.
    pub fn run(self) -> EvolutionResult<S> {
        let mut config = self.config;

        config.telemetry.log("preparing initial solution(-s)");

        std::mem::take(&mut config.initial.individuals)
            .into_iter()
            .zip(0_usize..)
            .take(config.initial.max_size)
            .for_each(|(solution, idx)| {
                if should_add_solution(&config.environment.quota, config.population.as_ref()) {
                    config.telemetry.on_initial(&solution, idx, config.initial.max_size, Timer::start());
                    config.population.add(solution);
                } else {
                    config.telemetry.log(format!("skipping provided initial solution {}", idx).as_str())
                }
            });

        let mut heuristic_ctx = (self.context_factory)(config.population);

        let weights = config.initial.operators.iter().map(|(_, weight)| *weight).collect::<Vec<_>>();

        let initial_time = Timer::start();
        let _ = (heuristic_ctx.population().size()..config.initial.max_size).try_for_each(|idx| {
            let item_time = Timer::start();

            let is_overall_termination = config.termination.is_termination(&mut heuristic_ctx);
            let is_initial_quota_reached = config.termination.estimate(&heuristic_ctx) > config.initial.quota;

            if is_initial_quota_reached || is_overall_termination {
                config.telemetry.log(
                    format!(
                        "stop building initial solutions due to initial quota reached ({}) or overall termination ({}).",
                        is_initial_quota_reached, is_overall_termination
                    )
                        .as_str(),
                );
                return Err(());
            }

            let operator_idx = if idx < config.initial.operators.len() {
                idx
            } else {
                config.environment.random.weighted(weights.as_slice())
            };

            // TODO consider initial quota limit
            let solution = config.initial.operators[operator_idx].0.create(&heuristic_ctx);

            if should_add_solution(&heuristic_ctx.environment().quota, heuristic_ctx.population()) {
                config.telemetry.on_initial(&solution, idx, config.initial.max_size, item_time);
                heuristic_ctx.population_mut().add(solution);
            } else {
                config.telemetry.log(format!("skipping built initial solution {}", idx).as_str())
            }

            Ok(())
        });

        if heuristic_ctx.population().size() > 0 {
            on_generation(&mut heuristic_ctx, &mut config.telemetry, config.termination.as_ref(), initial_time, true);
        } else {
            config.telemetry.log("created an empty population");
        }

        config.evolution_strategy.run(heuristic_ctx)
    }
}

fn should_stop<C, O, S>(heuristic_ctx: &mut C, termination: &(dyn Termination<Context = C, Objective = O>)) -> bool
where
    C: HeuristicContext<Objective = O, Solution = S>,
    O: HeuristicObjective<Solution = S>,
    S: HeuristicSolution,
{
    let is_terminated = termination.is_termination(heuristic_ctx);
    let is_quota_reached = heuristic_ctx.environment().quota.as_ref().map_or(false, |q| q.is_reached());

    is_terminated || is_quota_reached
}

fn should_add_solution<O, S>(
    quota: &Option<Arc<dyn Quota + Send + Sync>>,
    population: &(dyn HeuristicPopulation<Objective = O, Individual = S>),
) -> bool
where
    O: HeuristicObjective<Solution = S>,
    S: HeuristicSolution,
{
    let is_quota_reached = quota.as_ref().map_or(false, |quota| quota.is_reached());
    let is_population_empty = population.size() == 0;

    // NOTE when interrupted, population can return solution with worse primary objective fitness values as first
    is_population_empty || !is_quota_reached
}

fn on_generation<C, O, S>(
    heuristic_ctx: &mut C,
    telemetry: &mut Telemetry<C, O, S>,
    termination: &(dyn Termination<Context = C, Objective = O>),
    generation_time: Timer,
    is_improved: bool,
) where
    C: HeuristicContext<Objective = O, Solution = S>,
    O: HeuristicObjective<Solution = S>,
    S: HeuristicSolution,
{
    let termination_estimate = termination.estimate(heuristic_ctx);

    let statistics = telemetry.on_generation(heuristic_ctx, termination_estimate, generation_time, is_improved);
    heuristic_ctx.population_mut().on_generation(&statistics);
}
