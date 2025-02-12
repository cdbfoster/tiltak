#[cfg(feature = "constant-tuning")]
use std::collections::HashSet;
use std::convert::TryFrom;
use std::io::{Read, Write};
#[cfg(feature = "constant-tuning")]
use std::str::FromStr;
#[cfg(feature = "constant-tuning")]
use std::sync::atomic::{self, AtomicU64};
use std::{io, time};

use board_game_traits::Position as PositionTrait;
use board_game_traits::{Color, GameResult};
use half::f16;
use pgn_traits::PgnPosition;
#[cfg(feature = "constant-tuning")]
use rayon::prelude::*;

use tiltak::evaluation::{parameters, value_eval};
use tiltak::minmax;
#[cfg(feature = "sqlite")]
use tiltak::policy_sqlite;
#[cfg(feature = "constant-tuning")]
use tiltak::position::Role;
use tiltak::position::{AbstractBoard, Direction, Komi, Move, Square, SquareCacheEntry};
use tiltak::position::{Position, Stack};
use tiltak::ptn::{Game, PtnMove};
use tiltak::search::MctsSetting;
use tiltak::{position, search};

#[cfg(test)]
mod tests;

pub mod playtak;
pub mod tei;

fn main() {
    println!("play: Play against the engine through the command line");
    println!("aimatch: Watch the engine play against a very simple minmax implementation");
    println!("analyze <size>: Analyze a given position, provided from a PTN or a simple move list");
    println!("tps <size>: Analyze a given position, provided from a tps string");
    println!("game <size>: Analyze a whole game, provided from a PTN or a simple move list");
    println!(
        "perft <size>: Generate perft numbers of a given position, provided from a tps string"
    );
    #[cfg(feature = "sqlite")]
    println!("test_policy: Test how well policy scores find immediate wins in real games");
    loop {
        let mut input = String::new();
        let bytes_read = io::stdin().read_line(&mut input).unwrap();
        if bytes_read == 0 {
            break;
        }
        let words = input.split_whitespace().collect::<Vec<_>>();
        if words.is_empty() {
            continue;
        }
        match words[0] {
            "play" => {
                let position = Position::default();
                play_human(position);
            }
            "aimatch" => {
                for i in 1..10 {
                    mcts_vs_minmax(3, 50000 * i);
                }
            }
            "analyze" => match words.get(1) {
                Some(&"4") => analyze_position_from_ptn::<4>(),
                Some(&"5") => analyze_position_from_ptn::<5>(),
                Some(&"6") => analyze_position_from_ptn::<6>(),
                Some(&"7") => analyze_position_from_ptn::<7>(),
                Some(&"8") => analyze_position_from_ptn::<8>(),
                Some(s) => println!("Unsupported size {}", s),
                None => analyze_position_from_ptn::<5>(),
            },
            "tps" => match words.get(1) {
                Some(&"4") => analyze_position_from_tps::<4>(),
                Some(&"5") => analyze_position_from_tps::<5>(),
                Some(&"6") => analyze_position_from_tps::<6>(),
                Some(&"7") => analyze_position_from_tps::<7>(),
                Some(&"8") => analyze_position_from_tps::<8>(),
                Some(s) => println!("Unsupported size {}", s),
                None => analyze_position_from_tps::<5>(),
            },
            "perft" => match words.get(1) {
                Some(&"3") => perft_from_tps::<3>(),
                Some(&"4") => perft_from_tps::<4>(),
                Some(&"5") => perft_from_tps::<5>(),
                Some(&"6") => perft_from_tps::<6>(),
                Some(&"7") => perft_from_tps::<7>(),
                Some(&"8") => perft_from_tps::<8>(),
                Some(s) => println!("Unsupported size {}", s),
                None => perft_from_tps::<5>(),
            },
            #[cfg(feature = "constant-tuning")]
            "openings" => {
                let depth = 4;
                let komi = Komi::from_str("2.0").unwrap();
                let mut positions = HashSet::new();
                let openings = generate_openings::<6>(
                    &mut Position::start_position_with_komi(komi),
                    &mut positions,
                    depth,
                );
                println!("{} openings generated, evaluating...", openings.len());

                let start_time = time::Instant::now();
                let evaled: AtomicU64 = AtomicU64::default();

                let mut evaled_openings: Vec<_> = openings
                    .into_par_iter()
                    .filter(|opening| opening.len() == depth as usize)
                    .map(|opening| {
                        let mut position = Position::start_position_with_komi(komi);
                        for mv in opening.iter() {
                            position.do_move(*mv);
                        }
                        let result = (opening, search::mcts(position, 100_000));
                        let total = evaled.fetch_add(1, atomic::Ordering::Relaxed);
                        if total % 1000 == 0 {
                            eprintln!(
                                "Evaluted {} openings in {}s",
                                total,
                                start_time.elapsed().as_secs()
                            );
                        }
                        result
                    })
                    .collect();

                evaled_openings.sort_by(|(_, (_, score1)), (_, (_, score2))| {
                    score1.partial_cmp(score2).unwrap()
                });
                for (p, (mv, s)) in evaled_openings {
                    let mut position = Position::start_position_with_komi(komi);
                    for mv in p {
                        print!("{} ", position.move_to_san(&mv));
                        position.do_move(mv);
                    }
                    print!(": ");
                    println!("{}, {}", position.move_to_san(&mv), s);
                }
                return;
            }
            #[cfg(feature = "constant-tuning")]
            "analyze_openings" => analyze_openings::<6>(Komi::default(), 500_000),
            #[cfg(feature = "sqlite")]
            "test_policy" => policy_sqlite::check_all_games(),
            "value_features" => match words.get(1) {
                Some(&"4") => print_value_features::<4>(Komi::from_half_komi(4).unwrap()), // TODO: Bad default komi
                Some(&"5") => print_value_features::<5>(Komi::from_half_komi(4).unwrap()),
                Some(&"6") => print_value_features::<6>(Komi::from_half_komi(4).unwrap()),
                Some(s) => println!("Unsupported size {}", s),
                None => print_value_features::<5>(Komi::from_half_komi(4).unwrap()),
            },
            "policy_features" => match words.get(1) {
                Some(&"4") => print_policy_features::<4>(Komi::from_half_komi(4).unwrap()), // TODO: Bad default komi
                Some(&"5") => print_policy_features::<5>(Komi::from_half_komi(4).unwrap()),
                Some(&"6") => print_policy_features::<6>(Komi::from_half_komi(4).unwrap()),
                Some(s) => println!("Unsupported size {}", s),
                None => print_policy_features::<5>(Komi::from_half_komi(4).unwrap()),
            },
            "game" => {
                println!("Enter move list or a full PTN, then press enter followed by CTRL+D");
                let mut input = String::new();

                match words.get(1) {
                    Some(&"6") => {
                        io::stdin().read_to_string(&mut input).unwrap();
                        let games = tiltak::ptn::ptn_parser::parse_ptn(&input).unwrap();
                        if games.is_empty() {
                            continue;
                        }
                        println!("Analyzing 1 game: ");

                        analyze_game::<6>(games[0].clone());
                    }
                    None | Some(&"5") => {
                        io::stdin().read_to_string(&mut input).unwrap();
                        let games = tiltak::ptn::ptn_parser::parse_ptn(&input).unwrap();
                        if games.is_empty() {
                            println!("Couldn't parse any games");
                            continue;
                        }
                        println!("Analyzing 1 game: ");

                        analyze_game::<5>(games[0].clone());
                    }
                    Some(s) => println!("Game analysis at size {} not available", s),
                }
            }
            "mem_usage" => mem_usage::<6>(),
            "bench" => bench(),
            "bench_old" => bench_old(),
            "selfplay" => mcts_selfplay(time::Duration::from_secs(10)),
            s => println!("Unknown option \"{}\"", s),
        }
    }
}

#[cfg(feature = "constant-tuning")]
fn analyze_openings<const S: usize>(komi: Komi, nodes: u32) {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input).unwrap();
    input
        .lines()
        .flat_map(|line| line.split(':').take(1))
        .par_bridge()
        .for_each(|line| {
            let mut position = <Position<S>>::start_position_with_komi(komi);
            for word in line
                .split_whitespace()
                .take_while(|word| !word.contains(':'))
            {
                let mv = position.move_from_san(word).unwrap();
                position.do_move(mv);
            }
            let start_time = time::Instant::now();
            let settings = search::MctsSetting::default().arena_size_for_nodes(nodes);
            let mut tree = search::MonteCarloTree::with_settings(position.clone(), settings);
            for _ in 0..nodes {
                if tree.select().is_none() {
                    eprintln!("Warning: Search stopped early due to OOM");
                    break;
                };
            }
            let pv: Vec<Move<S>> = tree.pv().take(4).collect();
            print!(
                "{}: {:.4}, {:.1}s, ",
                line.trim(),
                tree.best_move().1,
                start_time.elapsed().as_secs_f32()
            );
            for mv in pv {
                print!("{} ", position.move_to_san(&mv));
                position.do_move(mv);
            }
            println!();
        });
}

#[cfg(feature = "constant-tuning")]
fn generate_openings<const S: usize>(
    position: &mut Position<S>,
    positions: &mut HashSet<Position<S>>,
    depth: u8,
) -> Vec<Vec<Move<S>>> {
    use tiltak::position::ExpMove;

    let mut moves = vec![];
    position.generate_moves(&mut moves);
    moves.retain(|mv| matches!(mv.expand(), ExpMove::Place(Role::Flat, _)));
    moves
        .into_iter()
        .flat_map(|mv| {
            let reverse_move = position.do_move(mv);
            let mut child_lines = if position
                .symmetries()
                .iter()
                .all(|board_symmetry| !positions.contains(board_symmetry))
            {
                positions.insert(position.clone());
                if depth > 1 {
                    generate_openings(position, positions, depth - 1)
                } else {
                    vec![vec![]]
                }
            } else {
                vec![]
            };
            position.reverse_move(reverse_move);
            for child_line in child_lines.iter_mut() {
                child_line.insert(0, mv);
            }
            child_lines
        })
        .collect()
}

fn mcts_selfplay(max_time: time::Duration) {
    let mut position = <Position<5>>::default();
    let mut moves = vec![];

    let mut white_elapsed = time::Duration::default();
    let mut black_elapsed = time::Duration::default();

    while position.game_result().is_none() {
        let start_time = time::Instant::now();
        let (best_move, score) =
            search::play_move_time::<5>(position.clone(), max_time, MctsSetting::default());

        match position.side_to_move() {
            Color::White => white_elapsed += start_time.elapsed(),
            Color::Black => black_elapsed += start_time.elapsed(),
        }

        position.do_move(best_move);
        moves.push(best_move);
        println!(
            "{:6}: {:.3}, {:.1}s",
            best_move.to_string(),
            score,
            start_time.elapsed().as_secs_f32()
        );
        io::stdout().flush().unwrap();
    }

    println!(
        "{:.1} used by white, {:.1} for black",
        white_elapsed.as_secs_f32(),
        black_elapsed.as_secs_f32()
    );

    print!("\n[");
    for mv in moves.iter() {
        print!("\"{:?}\", ", mv);
    }
    println!("]");

    for (ply, mv) in moves.iter().enumerate() {
        if ply % 2 == 0 {
            print!("{}. {:?} ", ply / 2 + 1, mv);
        } else {
            println!("{:?}", mv);
        }
    }
    println!();

    println!("\n{:?}\nResult: {:?}", position, position.game_result());
}

fn mcts_vs_minmax(minmax_depth: u16, mcts_nodes: u64) {
    println!("Minmax depth {} vs mcts {} nodes", minmax_depth, mcts_nodes);
    let mut position = <Position<5>>::default();
    let mut moves = vec![];
    while position.game_result().is_none() {
        let num_moves = moves.len();
        if num_moves > 10 && (1..5).all(|i| moves[num_moves - i] == moves[num_moves - i - 4]) {
            break;
        }
        match position.side_to_move() {
            Color::Black => {
                let (best_move, score) = search::mcts::<5>(position.clone(), mcts_nodes);
                position.do_move(best_move);
                moves.push(best_move);
                println!("{:6}: {:.3}", best_move.to_string(), score);
                io::stdout().flush().unwrap();
            }

            Color::White => {
                let (best_move, score) = minmax::minmax(&mut position, minmax_depth);
                position.do_move(best_move.unwrap());
                moves.push(best_move.unwrap());
                print!("{:6}: {:.2}, ", best_move.unwrap().to_string(), score);
                io::stdout().flush().unwrap();
            }
        }
    }
    print!("\n[");
    for mv in moves.iter() {
        print!("\"{:?}\", ", mv);
    }
    println!("]");

    for (ply, mv) in moves.iter().enumerate() {
        if ply % 2 == 0 {
            print!("{}. {:?} ", ply / 2 + 1, mv);
        } else {
            println!("{:?}", mv);
        }
    }
    println!();

    println!("\n{:?}\nResult: {:?}", position, position.game_result());
}

fn print_value_features<const S: usize>(komi: Komi) {
    let mut params: Vec<f16> = <Position<S>>::value_params(komi)
        .iter()
        .map(|p| f16::from_f32(*p))
        .collect();
    let num: usize = params.len() / 2;
    let (white_coefficients, black_coefficients) = params.split_at_mut(num);

    let white_value_features = parameters::ValueFeatures::new::<S>(white_coefficients);
    let white_value_features_string = format!("{:?}", white_value_features);

    let black_value_features = parameters::ValueFeatures::new::<S>(black_coefficients);
    let black_value_features_string = format!("{:?}", black_value_features);

    println!("White features:");
    for line in white_value_features_string.split("],") {
        let (name, values) = line.split_once(": ").unwrap();
        println!("{:40}: {}],", name, values);
    }
    println!();
    println!("Black features:");
    for line in black_value_features_string.split("],") {
        let (name, values) = line.split_once(": ").unwrap();
        println!("{:40}: {}],", name, values);
    }
}

fn print_policy_features<const S: usize>(komi: Komi) {
    let mut params: Vec<f16> = <Position<S>>::policy_params(komi)
        .iter()
        .map(|p| f16::from_f32(*p))
        .collect();

    let policy_features = parameters::PolicyFeatures::new::<S>(&mut params);
    let policy_features_string = format!("{:?}", policy_features);

    println!("White features:");
    for line in policy_features_string.split("],") {
        let (name, values) = line.split_once(": ").unwrap();
        println!("{:48}: {}],", name, values);
    }
}

fn analyze_position_from_ptn<const S: usize>() {
    println!("Enter move list or a full PTN, then press enter followed by CTRL+D");

    let mut input = String::new();
    io::stdin().read_to_string(&mut input).unwrap();
    let games: Vec<Game<Position<S>>> = tiltak::ptn::ptn_parser::parse_ptn(&input).unwrap();
    if games.is_empty() {
        println!("Couldn't parse any games");
        return;
    }

    let mut position: Position<S> = games[0].start_position.clone();

    for PtnMove { mv, .. } in games[0].moves.clone() {
        position.do_move(mv);
    }
    analyze_position(&position)
}

fn analyze_position_from_tps<const S: usize>() {
    println!("Enter TPS");
    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();
    let position = <Position<S>>::from_fen_with_komi(&input, Komi::try_from(2.0).unwrap()).unwrap();
    analyze_position(&position)
}

fn analyze_position<const S: usize>(position: &Position<S>) {
    println!("TPS {}", position.to_fen());
    println!("{:?}", position);
    println!("Komi: {}", position.komi());

    // Change which sets of eval parameters to use in search
    // Can be different from the komi used to determine the game result at terminal nodes
    let eval_komi = position.komi();

    assert_eq!(position.game_result(), None, "Cannot analyze finished game");

    let group_data = position.group_data();

    let mut coefficients = vec![f16::ZERO; parameters::num_value_features::<S>()];
    let coefficients_mid_index = coefficients.len() / 2;

    let (white_coefficients, black_coefficients) =
        coefficients.split_at_mut(coefficients_mid_index);

    {
        let mut white_value_features = parameters::ValueFeatures::new::<S>(white_coefficients);
        let mut black_value_features = parameters::ValueFeatures::new::<S>(black_coefficients);
        value_eval::static_eval_game_phase::<S>(
            position,
            &group_data,
            &mut white_value_features,
            &mut black_value_features,
        );
    }
    for (feature, param) in white_coefficients.iter_mut().zip(
        <Position<S>>::value_params(eval_komi)
            .iter()
            .take(coefficients_mid_index),
    ) {
        *feature *= f16::from_f32(*param);
    }

    for (feature, param) in black_coefficients.iter_mut().zip(
        <Position<S>>::value_params(eval_komi)
            .iter()
            .skip(coefficients_mid_index),
    ) {
        *feature *= f16::from_f32(*param);
    }

    let mut mixed_coefficients: Vec<f16> = white_coefficients
        .iter()
        .zip(black_coefficients.iter())
        .map(|(white, black)| *white - *black)
        .collect();

    let white_value_features = parameters::ValueFeatures::new::<S>(white_coefficients);
    let white_value_features_string = format!("{:?}", white_value_features);

    let black_value_features = parameters::ValueFeatures::new::<S>(black_coefficients);
    let black_value_features_string = format!("{:?}", black_value_features);

    let mixed_value_features = parameters::ValueFeatures::new::<S>(&mut mixed_coefficients);
    let mixed_value_features_string = format!("{:?}", mixed_value_features);

    println!("White features:");
    for line in white_value_features_string.split("],") {
        let (name, values) = line.split_once(": ").unwrap();
        println!("{:32}: {}],", name, values);
    }
    println!();
    println!("Black features:");
    for line in black_value_features_string.split("],") {
        let (name, values) = line.split_once(": ").unwrap();
        println!("{:32}: {}],", name, values);
    }
    println!();
    println!("Mixed features:");
    for line in mixed_value_features_string.split("],") {
        let (name, values) = line.split_once(": ").unwrap();
        println!("{:32}: {}],", name, values);
    }

    let mut simple_moves = vec![];
    let mut moves = vec![];
    let mut fcd_per_move = vec![];

    position.generate_moves_with_probabilities(
        &position.group_data(),
        &mut simple_moves,
        &mut moves,
        &mut fcd_per_move,
        &mut vec![],
        <Position<S>>::policy_params(eval_komi),
        &mut Some(vec![]),
    );
    moves.sort_by(|(_mv, score1), (_, score2)| score1.partial_cmp(score2).unwrap().reverse());

    let mut feature_sets =
        vec![vec![f16::ZERO; parameters::num_policy_features::<S>()]; moves.len()];
    let mut policy_feature_sets: Vec<_> = feature_sets
        .iter_mut()
        .map(|feature_set| parameters::PolicyFeatures::new::<S>(feature_set))
        .collect();

    let simple_moves: Vec<Move<S>> = moves.iter().map(|(mv, _)| *mv).collect();

    position.features_for_moves(
        &mut policy_feature_sets,
        &simple_moves,
        &mut fcd_per_move,
        &group_data,
    );

    println!("Top 10 heuristic moves:");
    for ((mv, score), features) in moves.iter().zip(feature_sets).take(10) {
        println!("{}: {:.3}%", mv, score.to_f32() * 100.0);
        for feature in features {
            print!("{:.1}, ", feature);
        }
        println!();
    }
    let settings: MctsSetting<S> = search::MctsSetting::default()
        .arena_size(2_u32.pow(31))
        .exclude_moves(vec![])
        .add_value_params(<Position<S>>::value_params_2komi().into())
        .add_policy_params(<Position<S>>::policy_params_2komi().into());
    let start_time = time::Instant::now();

    let mut tree = search::MonteCarloTree::with_settings(position.clone(), settings);
    for i in 1.. {
        if tree.select().is_none() {
            println!("Search stopped due to OOM");
            break;
        };
        if i % 100_000 == 0 {
            let params = <Position<S>>::value_params(eval_komi);

            let mut features: Vec<f16> = vec![f16::ZERO; params.len()];
            position.static_eval_features(&mut features);
            let static_eval: f32 = features
                .iter()
                .zip(params)
                .map(|(a, b)| a.to_f32() * b)
                .sum::<f32>()
                * position.side_to_move().multiplier() as f32;
            println!(
                "{} visits, eval: {:.2}%, Wilem-style eval: {:+.2}, static eval: {:.4}, static winning probability: {:.2}%, {:.2}s",
                tree.visits(),
                tree.mean_action_value() * 100.0,
                tree.mean_action_value() * 2.0 - 1.0,
                static_eval,
                search::cp_to_win_percentage(static_eval) * 100.0,
                start_time.elapsed().as_secs_f64()
            );
            tree.print_info();
            let (mv, value) = tree.best_move();
            println!("Best move: ({}, {})", mv, value);
        }
    }
}

fn perft_from_tps<const S: usize>() {
    println!("Enter TPS (or leave empty for initial)");
    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();
    let mut position = if input.trim().is_empty() {
        <Position<S>>::default()
    } else {
        <Position<S>>::from_fen(&input).unwrap()
    };
    perft(&mut position);
}

fn perft<const S: usize>(position: &mut Position<S>) {
    for depth in 0.. {
        let start_time = time::Instant::now();
        let result = position.bulk_perft(depth);
        println!(
            "{}: {}, {:.2}s, {:.1} Mnps",
            depth,
            result,
            start_time.elapsed().as_secs_f32(),
            result as f32 / start_time.elapsed().as_micros() as f32
        );
    }
}

fn analyze_game<const S: usize>(game: Game<Position<S>>) {
    let mut position = game.start_position.clone();
    let mut ply_number = 2;
    for PtnMove { mv, .. } in game.moves {
        position.do_move(mv);
        if let Some(game_result) = position.game_result() {
            let result_string = match game_result {
                GameResult::WhiteWin => "1-0",
                GameResult::BlackWin => "0-1",
                GameResult::Draw => "1/2-1/2",
            };
            if ply_number % 2 == 0 {
                print!("{}. {} {}", ply_number / 2, mv, result_string);
                io::stdout().flush().unwrap();
            } else {
                println!("{}... {} {}", ply_number / 2, mv, result_string);
            }
        } else {
            let (best_move, score) = search::mcts(position.clone(), 1_000_000);
            if ply_number % 2 == 0 {
                print!(
                    "{}. {} {{{:.2}%, best reply {}}} ",
                    ply_number / 2,
                    position.move_to_san(&mv),
                    (1.0 - score) * 100.0,
                    best_move
                );
                io::stdout().flush().unwrap();
            } else {
                println!(
                    "{}... {} {{{:.2}%, best reply {}}}",
                    ply_number / 2,
                    position.move_to_san(&mv),
                    (1.0 - score) * 100.0,
                    best_move
                );
            }
        }
        ply_number += 1;
    }
}

/// Play a game against the engine through stdin
fn play_human(mut position: Position<5>) {
    match position.game_result() {
        None => {
            use board_game_traits::Color::*;
            println!("Position:\n{:?}", position);
            // If black, play as human
            if position.side_to_move() == Black {
                println!("Type your move in algebraic notation (c3):");

                let reader = io::stdin();
                let mut input_str = "".to_string();
                let mut legal_moves = vec![];
                position.generate_moves(&mut legal_moves);
                // Loop until user enters a valid move
                loop {
                    input_str.clear();
                    reader
                        .read_line(&mut input_str)
                        .expect("Failed to read line");

                    match position.move_from_san(input_str.trim()) {
                        Ok(val) => {
                            if legal_moves.contains(&val) {
                                break;
                            }
                            println!("Move {:?} is illegal! Legal moves: {:?}", val, legal_moves);
                            println!("Try again: ");
                        }

                        Err(error) => {
                            println!("{}, try again.", error);
                        }
                    }
                }
                let c_move = position.move_from_san(input_str.trim()).unwrap();
                position.do_move(c_move);
            } else {
                let (best_move, score) = search::mcts::<5>(position.clone(), 1_000_000);

                println!("Computer played {:?} with score {}", best_move, score);
                position.do_move(best_move);
            }
            play_human(position);
        }

        Some(GameResult::WhiteWin) => println!("White won! Board:\n{:?}", position),
        Some(GameResult::BlackWin) => println!("Black won! Board:\n{:?}", position),
        Some(GameResult::Draw) => println!("The game was drawn! Board:\n{:?}", position),
    }
}

fn bench() {
    println!("Starting benchmark");
    const NODES: u32 = 5_000_000;
    let start_time = time::Instant::now();

    let position = <Position<6>>::default();
    let settings = search::MctsSetting::default().arena_size_for_nodes(NODES);
    let mut tree = search::MonteCarloTree::with_settings(position, settings);
    let mut last_iteration_start_time = time::Instant::now();
    for n in 1..=NODES {
        tree.select().unwrap();
        if n % 500_000 == 0 {
            let knps = 500.0 / last_iteration_start_time.elapsed().as_secs_f32();
            last_iteration_start_time = time::Instant::now();
            println!(
                "n={}, {:.2}s, {:.1} knps",
                n,
                start_time.elapsed().as_secs_f32(),
                knps
            );
        }
    }

    let (mv, score) = tree.best_move();
    let knps = 5000.0 / start_time.elapsed().as_secs_f32();

    println!(
        "{}: {:.2}%, {:.2}s, {:.1} knps",
        mv,
        score * 100.0,
        start_time.elapsed().as_secs_f32(),
        knps,
    );
}

fn bench_old() {
    const NODES: u64 = 1_000_000;
    let start_time = time::Instant::now();
    {
        let position = <Position<5>>::default();

        let (_move, score) = search::mcts::<5>(position, NODES);
        print!("{:.3}, ", score);
    }

    {
        let mut position = Position::default();

        do_moves_and_check_validity(&mut position, &["d3", "c3", "c4", "1d3<", "1c4+", "Sc4"]);

        let (_move, score) = search::mcts::<5>(position, NODES);
        print!("{:.3}, ", score);
    }
    {
        let mut position = Position::default();

        do_moves_and_check_validity(
            &mut position,
            &[
                "c2", "c3", "d3", "b3", "c4", "1c2-", "1d3<", "1b3>", "1c4+", "Cc2", "a1", "1c2-",
                "a2",
            ],
        );

        let (_move, score) = search::mcts::<5>(position, NODES);
        println!("{:.3}", score);
    }
    let time_taken = start_time.elapsed();
    println!(
        "{} nodes in {} ms, {:.1} knps",
        NODES * 3,
        time_taken.as_millis(),
        NODES as f64 * 3.0 / (1000.0 * time_taken.as_secs_f64())
    );
}

/// Print memory usage of various data types in the project, for debugging purposes
fn mem_usage<const S: usize>() {
    use std::mem;
    println!(
        "{}s tak board: {} bytes",
        S,
        mem::size_of::<position::Position<S>>()
    );
    println!("Tak board cell: {} bytes", mem::size_of::<Stack>());
    println!("Tak move: {} bytes", mem::size_of::<Move<S>>());
    println!("MCTS edge {}s: {} bytes", S, search::edge_mem_usage::<S>());
    println!("MCTS node {}s: {} bytes", S, search::node_mem_usage::<S>());
    println!("f16: {} bytes", mem::size_of::<f16>());
    println!(
        "Zobrist keys 5s: {} bytes",
        mem::size_of::<position::ZobristKeys<5>>()
    );
    println!(
        "Zobrist keys 6s: {} bytes",
        mem::size_of::<position::ZobristKeys<6>>()
    );
    println!(
        "Direction {} bytes, optional direction {} bytes",
        mem::size_of::<Direction>(),
        mem::size_of::<Option<Direction>>()
    );
    println!(
        "{}s square {} bytes, square cache entry: {} bytes, square cache table {} bytes",
        S,
        mem::size_of::<Square<S>>(),
        mem::size_of::<SquareCacheEntry<S>>(),
        mem::size_of::<AbstractBoard<SquareCacheEntry<6>, 6>>(),
    );
}

fn do_moves_and_check_validity(position: &mut Position<5>, move_strings: &[&str]) {
    let mut moves = vec![];
    for mv_san in move_strings.iter() {
        let mv = position.move_from_san(mv_san).unwrap();
        position.generate_moves(&mut moves);
        assert!(
            moves.contains(&mv),
            "Move {} was not among legal moves: {:?}\n{:?}",
            position.move_to_san(&mv),
            moves,
            position
        );
        position.do_move(mv);
        moves.clear();
    }
}
