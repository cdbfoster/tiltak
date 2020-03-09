use crate::board::{Board, Move};
use crate::tune::gradient_descent::TunableBoard;
use board_game_traits::board::{Board as BoardTrait, Color, GameResult};
use rand::Rng;

const C_PUCT: Score = 3.0;

pub type Score = f32;

#[derive(Clone, PartialEq, Debug)]
pub(crate) struct Tree {
    pub children: Vec<(Tree, Move)>,
    pub visits: u64,
    pub total_action_value: Score,
    pub mean_action_value: Score,
    pub heuristic_score: Score,
    pub is_terminal: bool,
}

// TODO: Winning percentage should be always be interpreted from the side to move's perspective

/// The module's main function. Run Monte Carlo Tree Search for `nodes` nodes.
/// Returns the best move, and its estimated winning probability for the side to move.
pub fn mcts(board: Board, nodes: u64) -> (Move, Score) {
    let mut tree = Tree::new_root();
    let mut moves = vec![];
    let mut simple_moves = vec![];
    for _ in 0..nodes.max(2) {
        tree.select(
            &mut board.clone(),
            Board::PARAMS,
            &mut simple_moves,
            &mut moves,
        );
    }
    let (mv, score) = tree.best_move(0.1);
    (mv, 1.0 - score)
}

pub(crate) fn mcts_training(
    board: Board,
    nodes: u64,
    params: &[f32],
    temperature: f64,
) -> (Move, Score) {
    let mut tree = Tree::new_root();
    let mut moves = vec![];
    let mut simple_moves = vec![];
    for _ in 0..nodes {
        tree.select(&mut board.clone(), params, &mut simple_moves, &mut moves);
    }
    tree.best_move(temperature)
}

impl Tree {
    pub(crate) fn new_root() -> Self {
        Tree {
            children: vec![],
            visits: 0,
            total_action_value: 0.0,
            mean_action_value: 0.5,
            heuristic_score: 0.0,
            is_terminal: false,
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
            is_terminal: self.is_terminal,
        }
    }

    pub fn print_info(&self) {
        let mut best_children: Vec<(Tree, Move)> = self.shallow_clone(3).children;
        best_children.sort_by_key(|(child, _)| child.visits);
        best_children.reverse();
        let parent_visits = self.visits;

        best_children.iter().take(8).for_each(|(child, mv)| {
            println!(
                "Move {}: {} visits, {:.3} mean action value, {:.3} static score, {:.3} exploration value, best reply {:?}",
                mv, child.visits, child.mean_action_value, child.heuristic_score,
                child.exploration_value((parent_visits as Score).sqrt()),
                if child.children.is_empty() { "".to_string() } else { format!("{:?}", child.best_move(0.1).0) }
            )
        });
    }

    pub fn best_move(&self, temperature: f64) -> (Move, Score) {
        let mut rng = rand::thread_rng();
        let mut move_probabilities = vec![];
        let mut cumulative_prob = 0.0;

        for (child, mv) in self.children.iter() {
            cumulative_prob += (child.visits as f64).powf(1.0 / temperature) / self.visits as f64;
            move_probabilities.push((mv, child.mean_action_value, cumulative_prob));
        }

        let p = rng.gen_range(0.0, cumulative_prob);
        for (mv, action_value, p2) in move_probabilities {
            if p2 > p {
                return (mv.clone(), action_value);
            }
        }
        unreachable!()
    }

    fn new_node(heuristic_score: Score) -> Self {
        Tree {
            children: vec![],
            visits: 0,
            total_action_value: 0.0,
            mean_action_value: 0.5,
            heuristic_score,
            is_terminal: false,
        }
    }

    pub fn select(
        &mut self,
        board: &mut Board,
        params: &[f32],
        simple_moves: &mut Vec<Move>,
        moves: &mut Vec<(Move, Score)>,
    ) -> Score {
        if self.is_terminal {
            self.visits += 1;
            self.total_action_value += self.mean_action_value;
            self.mean_action_value
        } else if self.visits == 0 {
            self.expand(board, params)
        } else {
            // Only generate child moves on the 2nd visit
            if self.visits == 1 {
                self.init_children(&board, simple_moves, moves);
            }

            let visits_sqrt = (self.visits as Score).sqrt();

            assert_ne!(
                self.children.len(),
                0,
                "No legal moves on board\n{:?}",
                board
            );

            let mut best_exploration_value = self.children[0].0.exploration_value(visits_sqrt);
            let mut best_child_node_index = 0;

            for (i, (child, _)) in self.children.iter().enumerate() {
                let child_exploration_value = child.exploration_value(visits_sqrt);
                if child_exploration_value >= best_exploration_value {
                    best_child_node_index = i;
                    best_exploration_value = child_exploration_value;
                }
            }

            let (child, mv) = self.children.get_mut(best_child_node_index).unwrap();

            board.do_move(mv.clone());
            let result = 1.0 - child.select(board, params, simple_moves, moves);
            self.visits += 1;
            self.total_action_value += result;
            self.mean_action_value = self.total_action_value / self.visits as Score;
            result
        }
    }

    // Never inline, for profiling purposes
    #[inline(never)]
    fn expand(&mut self, board: &mut Board, params: &[f32]) -> Score {
        debug_assert!(self.children.is_empty());

        if let Some(game_result) = board.game_result() {
            let result = match game_result {
                GameResult::Draw => 0.5,
                GameResult::WhiteWin => 0.0, // The side to move has lost
                GameResult::BlackWin => 0.0, // The side to move has lost
            };
            self.is_terminal = true;
            self.visits += 1;
            self.mean_action_value = result;
            self.total_action_value += result;
            return result;
        }

        let mut static_eval = cp_to_win_percentage(board.static_eval_with_params(params));
        if board.side_to_move() == Color::Black {
            static_eval = 1.0 - static_eval;
        }
        self.visits += 1;
        self.total_action_value = static_eval;
        self.mean_action_value = static_eval;
        static_eval
    }

    /// Do not initialize children in the expansion phase, for better fperformance
    /// Never inline, for profiling purposes
    #[inline(never)]
    fn init_children(
        &mut self,
        board: &Board,
        simple_moves: &mut Vec<Move>,
        moves: &mut Vec<(Move, Score)>,
    ) {
        board.generate_moves_with_probabilities(simple_moves, moves);
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

pub fn cp_to_win_percentage(cp: f32) -> Score {
    1.0 / (1.0 + Score::powf(10.0, -cp as Score / 3.0))
}
