#[cfg(test)]
#[path = "../../tests/unit/hyper/dynamic_selective_test.rs"]
mod dynamic_selective_test;

use super::*;
use crate::algorithms::math::{relative_distance, Remedian};
use crate::algorithms::mdp::*;
use crate::utils::{compare_floats, Random};
use crate::Timer;
use hashbrown::HashMap;
use std::cmp::Ordering;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

/// A collection of heuristic operators.
pub type HeuristicOperators<C, O, S> =
    Vec<(Arc<dyn HeuristicOperator<Context = C, Objective = O, Solution = S> + Send + Sync>, String)>;

/// Specifies a median estimator used to track medians of heuristic running time.
/// Its values can be used to penalize instant heuristic reward.
type RemedianUsize = Remedian<usize, fn(&usize, &usize) -> Ordering>;

/// An experimental dynamic selective hyper heuristic which selects inner heuristics
/// based on how they work during the search. The selection process is modeled by
/// Markov Decision Process.
pub struct DynamicSelective<C, O, S>
where
    C: HeuristicContext<Objective = O, Solution = S>,
    O: HeuristicObjective<Solution = S>,
    S: HeuristicSolution,
{
    heuristic_simulator: Simulator<SearchState>,
    initial_estimates: HashMap<SearchState, ActionEstimates<SearchState>>,
    action_registry: SearchActionRegistry<C, O, S>,
    heuristic_median: RemedianUsize,
}

impl<C, O, S> HyperHeuristic for DynamicSelective<C, O, S>
where
    C: HeuristicContext<Objective = O, Solution = S>,
    O: HeuristicObjective<Solution = S>,
    S: HeuristicSolution,
{
    type Context = C;
    type Objective = O;
    type Solution = S;

    fn search(&mut self, heuristic_ctx: &Self::Context, solutions: Vec<&Self::Solution>) -> Vec<Self::Solution> {
        let registry = &self.action_registry;
        let estimates = &self.initial_estimates;
        let median = &self.heuristic_median;

        let agents = solutions
            .into_iter()
            .map(|solution| {
                Box::new(SearchAgent {
                    heuristic_ctx,
                    original: solution,
                    registry,
                    estimates,
                    median,
                    state: match compare_to_best(heuristic_ctx, solution) {
                        Ordering::Greater => SearchState::Diverse(Default::default()),
                        _ => SearchState::BestKnown(Default::default()),
                    },
                    solution: Some(solution.deep_copy()),
                    runtime: Vec::default(),
                })
            })
            .collect();

        let (individuals, runtimes) = self
            .heuristic_simulator
            .run_episodes(agents, heuristic_ctx.environment().parallelism.clone(), |state, values| match state {
                SearchState::BestKnown { .. } => {
                    values.iter().max_by(|a, b| compare_floats(**a, **b)).cloned().unwrap_or(0.)
                }
                _ => values.iter().sum::<f64>() / values.len() as f64,
            })
            .into_iter()
            .filter_map(|agent| {
                #[allow(clippy::manual_map)]
                match agent.solution {
                    Some(solution) => Some((solution, agent.runtime)),
                    _ => None,
                }
            })
            .fold((Vec::new(), Vec::new()), |mut acc, (solution, runtime)| {
                acc.0.push(solution);
                acc.1.extend(runtime.into_iter());
                acc
            });

        runtimes.into_iter().for_each(|value| {
            self.heuristic_median.add_observation(value.as_millis() as usize);
        });

        try_exchange_estimates(&mut self.heuristic_simulator);

        individuals
    }
}

impl<C, O, S> DynamicSelective<C, O, S>
where
    C: HeuristicContext<Objective = O, Solution = S>,
    O: HeuristicObjective<Solution = S>,
    S: HeuristicSolution,
{
    /// Creates a new instance of `DynamicSelective` heuristic.
    pub fn new(operators: HeuristicOperators<C, O, S>, random: Arc<dyn Random + Send + Sync>) -> Self {
        let operator_estimates = (0..operators.len())
            .map(|heuristic_idx| (SearchAction::Search { heuristic_idx }, 0.))
            .collect::<HashMap<_, _>>();

        let operator_estimates = ActionEstimates::from(operator_estimates);

        Self {
            heuristic_simulator: Simulator::new(
                Box::new(MonteCarlo::new(0.1)),
                Box::new(EpsilonWeighted::new(0.1, random)),
            ),
            initial_estimates: vec![
                (SearchState::BestKnown(Default::default()), operator_estimates.clone()),
                (SearchState::Diverse(Default::default()), operator_estimates),
                (SearchState::BestMajorImprovement(Default::default()), Default::default()),
                (SearchState::BestMinorImprovement(Default::default()), Default::default()),
                (SearchState::DiverseImprovement(Default::default()), Default::default()),
                (SearchState::Stagnated(Default::default()), Default::default()),
            ]
            .into_iter()
            .collect(),
            heuristic_median: RemedianUsize::new(11, |a, b| a.cmp(b)),
            action_registry: SearchActionRegistry { heuristics: operators },
        }
    }
}

#[derive(Default, Clone)]
struct MedianRatio {
    pub ratio: f64,
}

impl Hash for MedianRatio {
    fn hash<H: Hasher>(&self, state: &mut H) {
        0.hash(state)
    }
}

impl PartialEq for MedianRatio {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for MedianRatio {}

impl MedianRatio {
    pub fn eval(&self, value: f64) -> f64 {
        let ratio = self.ratio.clamp(0.5, 2.);

        match (ratio, compare_floats(value, 0.)) {
            (ratio, _) if ratio < 1.001 => value,
            (ratio, Ordering::Equal) => -2. * ratio,
            (ratio, Ordering::Less) => value * ratio,
            (ratio, Ordering::Greater) => value / ratio,
        }
    }
}

#[derive(PartialEq, Eq, Hash, Clone)]
enum SearchState {
    /// A state with the best known solution.
    BestKnown(MedianRatio),
    /// A state with diverse (not the best known) solution.
    Diverse(MedianRatio),
    /// A state with new best known solution found (major improvement).
    BestMajorImprovement(MedianRatio),
    /// A state with new best known solution found (minor improvement).
    BestMinorImprovement(MedianRatio),
    /// A state with improved diverse solution.
    DiverseImprovement(MedianRatio),
    /// A state with equal or degraded solution.
    Stagnated(MedianRatio),
}

impl State for SearchState {
    type Action = SearchAction;

    fn reward(&self) -> f64 {
        match &self {
            SearchState::BestKnown(median_ratio) => median_ratio.eval(0.),
            SearchState::Diverse(median_ratio) => median_ratio.eval(0.),
            SearchState::BestMajorImprovement(median_ratio) => median_ratio.eval(1000.),
            SearchState::BestMinorImprovement(median_ratio) => median_ratio.eval(100.),
            SearchState::DiverseImprovement(median_ratio) => median_ratio.eval(10.),
            SearchState::Stagnated(median_ratio) => median_ratio.eval(-1.),
        }
    }
}

#[derive(PartialEq, Eq, Hash, Clone)]
enum SearchAction {
    /// An action which performs the search.
    Search { heuristic_idx: usize },
}

struct SearchActionRegistry<C, O, S>
where
    C: HeuristicContext<Objective = O, Solution = S>,
    O: HeuristicObjective<Solution = S>,
    S: HeuristicSolution,
{
    pub heuristics: HeuristicOperators<C, O, S>,
}

struct SearchAgent<'a, C, O, S>
where
    C: HeuristicContext<Objective = O, Solution = S>,
    O: HeuristicObjective<Solution = S>,
    S: HeuristicSolution,
{
    heuristic_ctx: &'a C,
    registry: &'a SearchActionRegistry<C, O, S>,
    estimates: &'a HashMap<SearchState, ActionEstimates<SearchState>>,
    median: &'a RemedianUsize,
    state: SearchState,
    original: &'a S,
    solution: Option<S>,
    runtime: Vec<Duration>,
}

impl<'a, C, O, S> Agent<SearchState> for SearchAgent<'a, C, O, S>
where
    C: HeuristicContext<Objective = O, Solution = S>,
    O: HeuristicObjective<Solution = S>,
    S: HeuristicSolution,
{
    fn get_state(&self) -> &SearchState {
        &self.state
    }

    fn get_actions(&self, state: &SearchState) -> ActionEstimates<SearchState> {
        self.estimates[state].clone()
    }

    fn take_action(&mut self, action: &<SearchState as State>::Action) {
        let (new_solution, duration) = match action {
            SearchAction::Search { heuristic_idx } => {
                let solution = self.solution.as_ref().unwrap();
                let (heuristic, _) = &self.registry.heuristics[*heuristic_idx];

                Timer::measure_duration(|| heuristic.search(self.heuristic_ctx, solution))
            }
        };

        let objective = self.heuristic_ctx.objective();

        let compare_to_old = objective.total_order(&new_solution, self.original);
        let compare_to_best = compare_to_best(self.heuristic_ctx, &new_solution);

        let ratio = MedianRatio {
            ratio: self.median.approx_median().map_or(1., |median| {
                if median == 0 {
                    1.
                } else {
                    duration.as_millis() as f64 / median as f64
                }
            }),
        };

        self.state = match (compare_to_old, compare_to_best) {
            (_, Ordering::Less) => {
                let is_significant_change = self.heuristic_ctx.population().ranked().next().map_or(
                    self.heuristic_ctx.statistics().improvement_1000_ratio < 0.01,
                    |(best, _)| {
                        let distance = relative_distance(
                            objective.objectives().map(|o| o.fitness(best)),
                            objective.objectives().map(|o| o.fitness(&new_solution)),
                        );
                        distance > 0.01
                    },
                );

                if is_significant_change {
                    SearchState::BestMajorImprovement(ratio)
                } else {
                    SearchState::BestMinorImprovement(ratio)
                }
            }
            (Ordering::Less, _) => SearchState::DiverseImprovement(ratio),
            (_, _) => SearchState::Stagnated(ratio),
        };

        self.solution = Some(new_solution);
        self.runtime.push(duration)
    }
}

fn try_exchange_estimates(heuristic_simulator: &mut Simulator<SearchState>) {
    let (best_known_max, diverse_state_max) = {
        let state_estimates = heuristic_simulator.get_state_estimates();
        (
            state_estimates.get(&SearchState::BestKnown(Default::default())).and_then(|state| state.max_estimate()),
            state_estimates.get(&SearchState::Diverse(Default::default())).and_then(|state| state.max_estimate()),
        )
    };

    let is_best_known_stagnation =
        best_known_max.map_or(false, |(_, max)| compare_floats(max, 0.) != Ordering::Greater);
    let is_diverse_improvement =
        diverse_state_max.map_or(false, |(_, max)| compare_floats(max, 0.) == Ordering::Greater);

    if is_best_known_stagnation && is_diverse_improvement {
        let estimates =
            heuristic_simulator.get_state_estimates().get(&SearchState::Diverse(Default::default())).unwrap().clone();
        heuristic_simulator.set_action_estimates(SearchState::BestKnown(Default::default()), estimates);
    }
}

fn compare_to_best<C, O, S>(heuristic_ctx: &C, solution: &S) -> Ordering
where
    C: HeuristicContext<Objective = O, Solution = S>,
    O: HeuristicObjective<Solution = S>,
    S: HeuristicSolution,
{
    heuristic_ctx
        .population()
        .ranked()
        .next()
        .map(|(best_known, _)| heuristic_ctx.objective().total_order(solution, best_known))
        .unwrap_or(Ordering::Less)
}
