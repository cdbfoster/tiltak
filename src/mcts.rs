//! A strong Tak AI, based on Monte Carlo Tree Search.
//!
//! This implementation does not use full Monte Carlo rollouts, relying on a heuristic evaluation when expanding new nodes instead.

use crate::board::{Board, Move, TunableBoard};
use board_game_traits::board::{Board as BoardTrait, Color, GameResult};
use rand::Rng;
use std::ops;

const C_PUCT: Score = 1.0;

/// Type alias for winning probability, used for scoring positions.
pub type Score = f32;

/// A Monte Carlo Search Tree, containing every node that has been seen in search.
#[derive(Clone, PartialEq, Debug)]
pub struct Tree {
    pub children: Vec<(Tree, Move)>,
    pub visits: u64,
    pub total_action_value: f64,
    pub mean_action_value: Score,
    pub heuristic_score: Score,
    pub known_result: Option<GameResultForUs>,
}

// TODO: Winning percentage should be always be interpreted from the side to move's perspective

/// The simplest way to use the mcts module. Run Monte Carlo Tree Search for `nodes` nodes, returning the best move, and its estimated winning probability for the side to move.
pub fn mcts(board: Board, nodes: u64) -> (Move, Score) {
    let mut tree = Tree::new_root();
    let mut moves = vec![];
    let mut simple_moves = vec![];
    for _ in 0..nodes.max(2) {
        tree.select(
            &mut board.clone(),
            Board::VALUE_PARAMS,
            Board::POLICY_PARAMS,
            &mut simple_moves,
            &mut moves,
        );
    }
    let (mv, score) = tree.best_move(0.1);
    (mv, score)
}

/// Run mcts with specific static evaluation parameters, for optimization the parameter set.
pub fn mcts_training(
    board: Board,
    nodes: u64,
    value_params: &[f32],
    policy_params: &[f32],
) -> Vec<(Move, Score)> {
    let mut tree = Tree::new_root();
    let mut moves = vec![];
    let mut simple_moves = vec![];
    for _ in 0..nodes {
        tree.select(
            &mut board.clone(),
            value_params,
            policy_params,
            &mut simple_moves,
            &mut moves,
        );
    }
    let child_visits: u64 = tree.children.iter().map(|(child, _)| child.visits).sum();
    tree.children
        .iter()
        .map(|(child, mv)| (mv.clone(), child.visits as f32 / child_visits as f32))
        .collect()
}

impl Tree {
    pub fn new_root() -> Self {
        Tree {
            children: vec![],
            visits: 0,
            total_action_value: 0.0,
            mean_action_value: 0.5,
            heuristic_score: 0.0,
            known_result: None,
        }
    }

    /// Clones this node, and all children down to a maximum depth
    pub fn shallow_clone(&self, depth: u8) -> Self {
        Tree {
            children: if depth <= 1 {
                vec![]
            } else {
                self.children
                    .iter()
                    .map(|(child, mv)| (child.shallow_clone(depth - 1), mv.clone()))
                    .collect()
            },
            visits: self.visits,
            total_action_value: self.total_action_value,
            mean_action_value: self.mean_action_value,
            heuristic_score: self.heuristic_score,
            known_result: self.known_result,
        }
    }

    /// Print human-readable information of the search's progress.
    pub fn print_info(&self) {
        let mut best_children: Vec<&(Tree, Move)> =
            self.children.iter().map(|child| child).collect();

        best_children.sort_by_key(|(child, _)| child.visits);
        best_children.reverse();
        let parent_visits = self.visits;

        best_children.iter().take(8).for_each(|(child, mv)| {
            println!(
                "Move {}: {} visits, {:.3} mean action value, {:.3} static score, {:.3} exploration value, pv {}",
                mv, child.visits, child.mean_action_value, child.heuristic_score,
                child.exploration_value((parent_visits as Score).sqrt()),
                child.pv().map(|mv| mv.to_string() + " ").collect::<String>()
            )
        });
    }

    pub fn pv<'a>(&'a self) -> impl Iterator<Item = Move> + 'a {
        PV::new(self)
    }

    pub fn best_move(&self, temperature: f64) -> (Move, Score) {
        let mut rng = rand::thread_rng();
        let mut move_probabilities = vec![];
        let mut cumulative_prob = 0.0;

        for (child, mv) in self.children.iter() {
            // If a child node wins for us, ignore temperature and return it
            if child.known_result == Some(GameResultForUs::Loss) {
                return (mv.clone(), 1.0 - child.mean_action_value);
            }
            cumulative_prob += (child.visits as f64).powf(1.0 / temperature) / self.visits as f64;
            move_probabilities.push((mv, child.mean_action_value, cumulative_prob));
        }

        let p = rng.gen_range(0.0, cumulative_prob);
        for (mv, action_value, p2) in move_probabilities {
            if p2 > p {
                return (mv.clone(), 1.0 - action_value);
            }
        }
        unreachable!()
    }

    fn new_node(heuristic_score: Score) -> Self {
        Tree {
            children: vec![],
            visits: 0,
            total_action_value: 0.0,
            mean_action_value: 0.1,
            heuristic_score,
            known_result: None,
        }
    }

    /// Perform one iteration of monte carlo tree search.
    ///
    /// Moves done on the board are not reversed.
    pub fn select(
        &mut self,
        board: &mut Board,
        value_params: &[f32],
        policy_params: &[f32],
        simple_moves: &mut Vec<Move>,
        moves: &mut Vec<(Move, Score)>,
    ) -> SearchResult {
        if self.known_result.is_some() {
            self.visits += 1;
            self.total_action_value += self.mean_action_value as f64;
            SearchResult::Value(self.mean_action_value)
        } else if self.visits == 0 {
            self.expand(board, value_params)
        } else {
            debug_assert_eq!(
                self.visits,
                self.children
                    .iter()
                    .map(|(child, _)| child.visits)
                    .sum::<u64>()
                    + 1,
                "{} visits, {} total action value, {} mean action value",
                self.visits,
                self.total_action_value,
                self.mean_action_value
            );
            // Only generate child moves on the 2nd visit
            if self.visits == 1 {
                self.init_children(&board, simple_moves, policy_params, moves);
            }

            let visits_sqrt = (self.visits as Score).sqrt();

            assert_ne!(
                self.children.len(),
                0,
                "No legal moves on board\n{:?}",
                board
            );

            let mut best_exploration_value = 0.0;
            let mut best_child_node_index = 0;

            for (i, (child, _)) in self.children.iter().enumerate() {
                if child.known_result == Some(GameResultForUs::Win)
                    || child.known_result == Some(GameResultForUs::Loss)
                {
                    // Immediately choose the move if it wins
                    if child.known_result == Some(GameResultForUs::Loss) {
                        best_child_node_index = i;
                        break;
                    }
                // Otherwise, it loses, and it is never picked
                } else {
                    let child_exploration_value = child.exploration_value(visits_sqrt);
                    if child_exploration_value >= best_exploration_value {
                        best_child_node_index = i;
                        best_exploration_value = child_exploration_value;
                    }
                }
            }

            let (child, mv) = self.children.get_mut(best_child_node_index).unwrap();

            // If we chose a child that is known to be lost for us,
            // *every* child is lost for us.
            // This node will never be selected again
            // Re-score it, and propagate the score change
            if child.known_result == Some(GameResultForUs::Win) {
                let result_to_propagate = SearchResult::Decisive(
                    self.visits,
                    self.visits as f64 - self.total_action_value,
                    GameResultForUs::Loss,
                );
                self.known_result = Some(GameResultForUs::Loss);
                self.visits = 1;
                self.total_action_value = 0.0;
                self.mean_action_value = 0.0;
                result_to_propagate
            } else {
                board.do_move(mv.clone());
                let result = !child.select(board, value_params, policy_params, simple_moves, moves);

                // If a child node is discovered to be winning for us, this node is also a forced win
                // The result from selecting the child does not matter. This node will never be selected again,
                // so re-score it from scratch and propagate this score change
                if child.known_result == Some(GameResultForUs::Loss) {
                    self.known_result = Some(GameResultForUs::Win);
                    let result_to_propagate = SearchResult::Decisive(
                        self.visits,
                        self.total_action_value,
                        GameResultForUs::Win,
                    );
                    self.visits = 1;
                    self.mean_action_value = 1.0;
                    self.total_action_value = 1.0;
                    return result_to_propagate;
                }
                self.visits += 1;
                match result {
                    SearchResult::Decisive(nodes, action_value, result_for_us) => {
                        self.visits -= nodes;
                        self.total_action_value -= action_value;
                        if result_for_us == GameResultForUs::Win {
                            self.total_action_value += 1.0;
                        }
                    }
                    SearchResult::Value(result) => {
                        self.total_action_value += result as f64;
                    }
                }

                self.mean_action_value = (self.total_action_value / self.visits as f64) as f32;
                result
            }
        }
    }

    // Never inline, for profiling purposes
    #[inline(never)]
    fn expand(&mut self, board: &mut Board, params: &[f32]) -> SearchResult {
        debug_assert!(self.children.is_empty());

        if let Some(game_result) = board.game_result() {
            let game_result_for_us = match (game_result, board.side_to_move()) {
                (GameResult::Draw, _) => GameResultForUs::Draw,
                (GameResult::WhiteWin, Color::Black) => GameResultForUs::Loss, // The side to move has lost
                (GameResult::BlackWin, Color::White) => GameResultForUs::Loss, // The side to move has lost
                (GameResult::WhiteWin, Color::White) => GameResultForUs::Win, // The side to move has lost
                (GameResult::BlackWin, Color::Black) => GameResultForUs::Win, // The side to move has lost
            };
            self.known_result = Some(game_result_for_us);
            self.visits = 1;

            let score = game_result_for_us.score();
            self.mean_action_value = score;
            self.total_action_value = score as f64;

            // Since a decisive result was found on the first iteration,
            // no nodes must be re-written further up in the tree.
            return SearchResult::Value(score);
        }

        let mut static_eval = cp_to_win_percentage(board.static_eval_with_params(params));
        if board.side_to_move() == Color::Black {
            static_eval = 1.0 - static_eval;
        }
        self.visits = 1;
        self.total_action_value = static_eval as f64;
        self.mean_action_value = static_eval;
        SearchResult::Value(static_eval)
    }

    /// Do not initialize children in the expansion phase, for better fperformance
    /// Never inline, for profiling purposes
    #[inline(never)]
    fn init_children(
        &mut self,
        board: &Board,
        simple_moves: &mut Vec<Move>,
        policy_params: &[f32],
        moves: &mut Vec<(Move, Score)>,
    ) {
        board.generate_moves_with_params(policy_params, simple_moves, moves);
        self.children.reserve_exact(moves.len());
        for (mv, heuristic_score) in moves.drain(..) {
            self.children
                .push((Tree::new_node(heuristic_score), mv.clone()));
        }
    }

    #[inline]
    fn exploration_value(&self, parent_visits_sqrt: Score) -> Score {
        (1.0 - self.mean_action_value)
            + C_PUCT * self.heuristic_score * parent_visits_sqrt / (1 + self.visits) as Score
    }
}

/// A game result from one side's perspective
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GameResultForUs {
    Win,
    Loss,
    Draw,
}

impl ops::Not for GameResultForUs {
    type Output = Self;

    fn not(self) -> Self::Output {
        match self {
            GameResultForUs::Win => GameResultForUs::Loss,
            GameResultForUs::Loss => GameResultForUs::Win,
            GameResultForUs::Draw => GameResultForUs::Draw,
        }
    }
}

impl GameResultForUs {
    fn score(self) -> Score {
        match self {
            GameResultForUs::Win => 1.0,
            GameResultForUs::Loss => 0.0,
            GameResultForUs::Draw => 0.5,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SearchResult {
    Value(Score),
    Decisive(u64, f64, GameResultForUs),
}

impl ops::Not for SearchResult {
    type Output = SearchResult;

    fn not(self) -> Self::Output {
        match self {
            SearchResult::Value(score) => SearchResult::Value(1.0 - score),
            SearchResult::Decisive(nodes, total_action_value, result) => {
                SearchResult::Decisive(nodes, nodes as f64 - total_action_value, !result)
            }
        }
    }
}

struct PV<'a> {
    tree: &'a Tree,
}

impl<'a> PV<'a> {
    fn new(tree: &'a Tree) -> PV<'a> {
        PV { tree }
    }
}

impl<'a> Iterator for PV<'a> {
    type Item = Move;

    fn next(&mut self) -> Option<Self::Item> {
        self.tree
            .children
            .iter()
            .max_by_key(|(child, _)| {
                if child.known_result == Some(GameResultForUs::Loss) {
                    u64::MAX
                } else {
                    child.visits
                }
            })
            .map(|(child, mv)| {
                self.tree = child;
                mv.clone()
            })
    }
}

/// Convert a static evaluation in centipawns to a winning probability between 0.0 and 1.0.
pub fn cp_to_win_percentage(cp: f32) -> Score {
    1.0 / (1.0 + Score::exp(-cp as Score))
}
