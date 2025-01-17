use arrayvec::ArrayVec;
use board_game_traits::{Color, Position as EvalPosition};
use half::f16;
use rand_distr::num_traits::FromPrimitive;

use crate::evaluation::parameters::ValueFeatures;
use crate::position::bitboard::BitBoard;
use crate::position::color_trait::{BlackTr, ColorTr, WhiteTr};
use crate::position::{
    line_symmetries, lookup_square_symmetries, squares_iterator, GroupData, Piece, Piece::*,
    Position, Role::*, Square,
};

pub fn static_eval_game_phase<const S: usize>(
    position: &Position<S>,
    group_data: &GroupData<S>,
    white_value_features: &mut ValueFeatures,
    black_value_features: &mut ValueFeatures,
) {
    let all_pieces = group_data.all_pieces();
    if all_pieces.count() == 0 {
        white_value_features.first_ply[0] = f16::ONE;
        return;
    } else if all_pieces.count() == 1 {
        for square in squares_iterator::<S>() {
            if position[square].top_stone().is_some() {
                white_value_features.second_ply[lookup_square_symmetries::<S>(square)] += f16::ONE;
                return;
            }
        }
        unreachable!()
    }

    let mut white_flat_count = 0;
    let mut black_flat_count = 0;

    for square in squares_iterator::<S>() {
        let stack = &position[square];
        if let Some(piece) = position[square].top_stone() {
            match piece {
                WhiteFlat => {
                    white_value_features.flat_psqt[lookup_square_symmetries::<S>(square)] +=
                        f16::ONE;
                    white_flat_count += 1;
                }
                BlackFlat => {
                    black_value_features.flat_psqt[lookup_square_symmetries::<S>(square)] +=
                        f16::ONE;
                    black_flat_count += 1;
                }
                WhiteWall => {
                    white_value_features.wall_psqt[lookup_square_symmetries::<S>(square)] +=
                        f16::ONE
                }
                BlackWall => {
                    black_value_features.wall_psqt[lookup_square_symmetries::<S>(square)] +=
                        f16::ONE
                }
                WhiteCap => {
                    white_value_features.cap_psqt[lookup_square_symmetries::<S>(square)] +=
                        f16::ONE;
                    cap_activity::<WhiteTr, BlackTr, S>(position, square, white_value_features);
                }
                BlackCap => {
                    black_value_features.cap_psqt[lookup_square_symmetries::<S>(square)] +=
                        f16::ONE;
                    cap_activity::<BlackTr, WhiteTr, S>(position, square, black_value_features);
                }
            }
            if stack.height > 1 {
                let controlling_player = piece.color();
                for (stack_index, stack_piece) in stack
                    .into_iter()
                    .enumerate()
                    .take(stack.height as usize - 1)
                {
                    // Position in the stack. Top stone is 1
                    let depth = stack.height as usize - stack_index;
                    let is_support = stack_piece.color() == controlling_player;
                    let top_role_index = match piece.role() {
                        Flat => 0,
                        Wall => 1,
                        Cap if stack.get(stack.height - 2).unwrap().color()
                            == controlling_player =>
                        {
                            2
                        }
                        Cap => 3,
                    };
                    // Separate non-psqt bonus based on the role of the top stone,
                    // and whether the stack piece is below the carry limit in the stack
                    match (is_support, depth > S + 1, controlling_player) {
                        (true, true, Color::White) => {
                            white_value_features.deep_supports_per_piece[top_role_index] += f16::ONE
                        }
                        (true, true, Color::Black) => {
                            black_value_features.deep_supports_per_piece[top_role_index] += f16::ONE
                        }
                        (true, false, Color::White) => {
                            white_value_features.shallow_supports_per_piece[top_role_index] +=
                                f16::ONE
                        }
                        (true, false, Color::Black) => {
                            black_value_features.shallow_supports_per_piece[top_role_index] +=
                                f16::ONE
                        }
                        (false, true, Color::White) => {
                            white_value_features.deep_captives_per_piece[top_role_index] += f16::ONE
                        }
                        (false, true, Color::Black) => {
                            black_value_features.deep_captives_per_piece[top_role_index] += f16::ONE
                        }
                        (false, false, Color::White) => {
                            white_value_features.shallow_captives_per_piece[top_role_index] +=
                                f16::ONE
                        }
                        (false, false, Color::Black) => {
                            black_value_features.shallow_captives_per_piece[top_role_index] +=
                                f16::ONE
                        }
                    }
                    match (is_support, controlling_player) {
                        (true, Color::White) => {
                            white_value_features.supports_psqt
                                [lookup_square_symmetries::<S>(square)] += f16::ONE
                        }
                        (true, Color::Black) => {
                            black_value_features.supports_psqt
                                [lookup_square_symmetries::<S>(square)] += f16::ONE
                        }
                        (false, Color::White) => {
                            white_value_features.captives_psqt
                                [lookup_square_symmetries::<S>(square)] -= f16::ONE
                        }
                        (false, Color::Black) => {
                            black_value_features.captives_psqt
                                [lookup_square_symmetries::<S>(square)] -= f16::ONE
                        }
                    }
                }
            }
        }
    }

    // Give the side to move a bonus/malus depending on flatstone lead
    let white_flatstone_lead = white_flat_count - black_flat_count;
    let black_flatstone_lead_komi =
        black_flat_count - white_flat_count + position.komi().half_komi() * 2;

    // Bonus/malus depending on the number of groups each side has
    let mut seen_groups: ArrayVec<bool, 257> = ArrayVec::new();
    seen_groups.push(true);
    for _ in 1..S * S + 1 {
        seen_groups.push(false);
    }

    let mut num_white_groups = 0;
    let mut num_black_groups = 0;
    for square in squares_iterator::<S>() {
        let group_id = group_data.groups[square] as usize;
        if !seen_groups[group_id] {
            seen_groups[group_id] = true;
            match position[square].top_stone().unwrap().color() {
                Color::White => num_white_groups += 1,
                Color::Black => num_black_groups += 1,
            }
        }
    }

    let opening_scale_factor = f16::from_f32(f32::min(
        f32::max((24.0 - position.half_moves_played() as f32) / 12.0, 0.0),
        1.0,
    ));
    let endgame_scale_factor = f16::from_f32(f32::min(
        f32::max((position.half_moves_played() as f32 - 24.0) / 24.0, 0.0),
        1.0,
    ));
    let middlegame_scale_factor = f16::ONE - opening_scale_factor - endgame_scale_factor;

    debug_assert!(middlegame_scale_factor <= f16::ONE);
    debug_assert!(opening_scale_factor == f16::ZERO || endgame_scale_factor == f16::ZERO);

    if position.side_to_move() == Color::White {
        let index = (white_flatstone_lead + 3).clamp(0, 6) as usize;
        white_value_features.us_to_move_opening_flatstone_lead[index] = opening_scale_factor;
        white_value_features.us_to_move_middlegame_flatstone_lead[index] = middlegame_scale_factor;
        white_value_features.us_to_move_endgame_flatstone_lead[index] = endgame_scale_factor;

        let komi_index = (black_flatstone_lead_komi + 3).clamp(0, 6) as usize;
        black_value_features.them_to_move_opening_flatstone_lead[komi_index] = opening_scale_factor;
        black_value_features.them_to_move_middlegame_flatstone_lead[komi_index] =
            middlegame_scale_factor;
        black_value_features.them_to_move_endgame_flatstone_lead[komi_index] = endgame_scale_factor;
    } else {
        let index = (white_flatstone_lead + 3).clamp(0, 6) as usize;
        white_value_features.them_to_move_opening_flatstone_lead[index] = opening_scale_factor;
        white_value_features.them_to_move_middlegame_flatstone_lead[index] =
            middlegame_scale_factor;
        white_value_features.them_to_move_endgame_flatstone_lead[index] = endgame_scale_factor;

        let komi_index = (black_flatstone_lead_komi + 3).clamp(0, 6) as usize;
        black_value_features.us_to_move_opening_flatstone_lead[komi_index] = opening_scale_factor;
        black_value_features.us_to_move_middlegame_flatstone_lead[komi_index] =
            middlegame_scale_factor;
        black_value_features.us_to_move_endgame_flatstone_lead[komi_index] = endgame_scale_factor;
    }

    // if position.side_to_move() == Color::White {
    //     white_value_features.side_to_move[0] = opening_scale_factor;
    // } else {
    //     black_value_features.side_to_move[0] = opening_scale_factor;
    // }
    // white_value_features.flatstone_lead[0] = white_flatstone_lead as f32 * opening_scale_factor;

    white_value_features.i_number_of_groups[0] =
        f16::from_i32(num_white_groups).unwrap() * opening_scale_factor;
    black_value_features.i_number_of_groups[0] =
        f16::from_i32(num_black_groups).unwrap() * opening_scale_factor;

    // if position.side_to_move() == Color::White {
    //     white_value_features.side_to_move[1] = middlegame_scale_factor;
    // } else {
    //     black_value_features.side_to_move[1] = middlegame_scale_factor;
    // }
    // white_value_features.flatstone_lead[1] = white_flatstone_lead as f32 * middlegame_scale_factor;

    white_value_features.i_number_of_groups[1] =
        f16::from_i32(num_white_groups).unwrap() * middlegame_scale_factor;
    black_value_features.i_number_of_groups[1] =
        f16::from_i32(num_black_groups).unwrap() * middlegame_scale_factor;

    // if position.side_to_move() == Color::White {
    //     white_value_features.side_to_move[2] = endgame_scale_factor;
    // } else {
    //     black_value_features.side_to_move[2] = endgame_scale_factor;
    // }
    // white_value_features.flatstone_lead[2] = white_flatstone_lead as f32 * endgame_scale_factor;

    white_value_features.i_number_of_groups[2] =
        f16::from_i32(num_white_groups).unwrap() * endgame_scale_factor;
    black_value_features.i_number_of_groups[2] =
        f16::from_i32(num_black_groups).unwrap() * endgame_scale_factor;

    for critical_square in group_data.critical_squares(Color::White) {
        critical_squares_eval::<WhiteTr, BlackTr, S>(
            position,
            group_data,
            critical_square,
            white_value_features,
        );
    }

    for critical_square in group_data.critical_squares(Color::Black) {
        critical_squares_eval::<BlackTr, WhiteTr, S>(
            position,
            group_data,
            critical_square,
            black_value_features,
        );
    }

    squares_iterator::<S>()
        .map(|sq| (sq, &position[sq]))
        .filter(|(_, stack)| stack.len() > 1)
        .for_each(|(square, stack)| {
            let top_stone = stack.top_stone().unwrap();
            let controlling_player = top_stone.color();

            // Malus for them having stones next to our stack with flat stones on top
            for neighbour in square.neighbors() {
                if let Some(neighbour_top_stone) = position[neighbour].top_stone() {
                    if top_stone.role() == Flat && neighbour_top_stone.color() != controlling_player
                    {
                        match (neighbour_top_stone.role(), top_stone.color()) {
                            (Flat, Color::White) => {
                                white_value_features.flat_next_to_our_stack[0] +=
                                    f16::from_u8(stack.len()).unwrap()
                            }
                            (Flat, Color::Black) => {
                                black_value_features.flat_next_to_our_stack[0] +=
                                    f16::from_u8(stack.len()).unwrap()
                            }
                            (Wall, Color::White) => {
                                white_value_features.wall_next_to_our_stack[0] +=
                                    f16::from_u8(stack.len()).unwrap()
                            }
                            (Wall, Color::Black) => {
                                black_value_features.wall_next_to_our_stack[0] +=
                                    f16::from_u8(stack.len()).unwrap()
                            }
                            (Cap, Color::White) => {
                                white_value_features.cap_next_to_our_stack[0] +=
                                    f16::from_u8(stack.len()).unwrap()
                            }
                            (Cap, Color::Black) => {
                                black_value_features.cap_next_to_our_stack[0] +=
                                    f16::from_u8(stack.len()).unwrap()
                            }
                        }
                    }
                }
            }
        });

    let mut num_ranks_occupied_white = 0;
    let mut num_files_occupied_white = 0;
    let mut num_ranks_occupied_black = 0;
    let mut num_files_occupied_black = 0;

    for i in 0..(S as u8) {
        let rank = BitBoard::full().rank::<S>(i);
        let file = BitBoard::full().file::<S>(i);
        line_score::<WhiteTr, BlackTr, S>(group_data, rank, i, white_value_features);
        line_score::<BlackTr, WhiteTr, S>(group_data, rank, i, black_value_features);
        line_score::<WhiteTr, BlackTr, S>(group_data, file, i, white_value_features);
        line_score::<BlackTr, WhiteTr, S>(group_data, file, i, black_value_features);
    }

    for i in 0..S as u8 {
        if !WhiteTr::road_stones(group_data).rank::<S>(i).is_empty() {
            num_ranks_occupied_white += 1;
        }
        if !BlackTr::road_stones(group_data).rank::<S>(i).is_empty() {
            num_ranks_occupied_black += 1;
        }
    }

    for i in 0..S as u8 {
        if !WhiteTr::road_stones(group_data).file::<S>(i).is_empty() {
            num_files_occupied_white += 1;
        }
        if !BlackTr::road_stones(group_data).file::<S>(i).is_empty() {
            num_files_occupied_black += 1;
        }
    }

    white_value_features.num_lines_occupied[num_ranks_occupied_white] += f16::ONE;
    white_value_features.num_lines_occupied[num_files_occupied_white] += f16::ONE;
    black_value_features.num_lines_occupied[num_ranks_occupied_black] += f16::ONE;
    black_value_features.num_lines_occupied[num_files_occupied_black] += f16::ONE;
}

fn cap_activity<Us: ColorTr, Them: ColorTr, const S: usize>(
    position: &Position<S>,
    square: Square<S>,
    our_value_features: &mut ValueFeatures,
) {
    let stack = position[square];
    let height_index = stack.height.min(3) as usize - 1;

    // Malus if our capstone's line towards the center is blocked
    if square.neighbors().any(|neighbour| {
        lookup_square_symmetries::<S>(neighbour) > lookup_square_symmetries::<S>(square)
            && position[neighbour].top_stone().map(Piece::role) == Some(Cap)
    }) {
        our_value_features.sidelined_cap[height_index] += f16::ONE
    }

    let is_soft_cap = stack
        .get(stack.height.overflowing_sub(2).0)
        .map(Them::piece_is_ours)
        == Some(true);
    if square.neighbors().all(|neighbour| {
        matches!(
            position[neighbour].top_stone(),
            Some(WhiteCap) | Some(BlackCap) | None
        )
    }) {
        our_value_features.fully_isolated_cap[height_index] += f16::ONE
    } else if square.neighbors().all(|neighbour| {
        if let Some(neighbour_top_stone) = position[neighbour].top_stone() {
            if neighbour_top_stone == Them::wall_piece() {
                is_soft_cap
            } else {
                neighbour_top_stone != Them::flat_piece()
            }
        } else {
            true
        }
    }) {
        our_value_features.semi_isolated_cap[height_index] += f16::ONE
    }
}

/// Give bonus for our critical squares
fn critical_squares_eval<Us: ColorTr, Them: ColorTr, const S: usize>(
    position: &Position<S>,
    group_data: &GroupData<S>,
    critical_square: Square<S>,
    our_value_features: &mut ValueFeatures,
) {
    let top_stone = position[critical_square].top_stone;
    let top_stone_role = top_stone.map(Piece::role);
    if top_stone.is_none() {
        our_value_features.critical_squares[0] += f16::ONE;
    } else if top_stone == Some(Us::wall_piece()) {
        our_value_features.critical_squares[1] += f16::ONE;
    } else if top_stone == Some(Them::flat_piece()) {
        our_value_features.critical_squares[2] += f16::ONE;
    }
    // Their capstone or wall
    else {
        our_value_features.critical_squares[3] += f16::ONE
    }

    let rank = critical_square.rank();
    let file = critical_square.file();

    let capstone_square_in_line = {
        let capstone_in_rank = BitBoard::full().rank::<S>(rank) & Us::caps(group_data);
        let capstone_in_file = BitBoard::full().file::<S>(file) & Us::caps(group_data);
        capstone_in_rank
            .occupied_square()
            .or(capstone_in_file.occupied_square())
    };

    // Bonuses when our capstone can spread to the critical square
    // TODO: Don't give bonuses if walls/caps block the spread
    if let Some(capstone_square) = capstone_square_in_line {
        let distance =
            file.abs_diff(capstone_square.file()) + rank.abs_diff(capstone_square.rank());
        let cap_stack = position[capstone_square];
        let is_hard_cap = cap_stack
            .get(cap_stack.len().saturating_sub(2))
            .is_some_and(Us::piece_is_ours);
        let num_high_supports = cap_stack
            .into_iter()
            .skip((cap_stack.len() as usize).saturating_sub(S + 1))
            .filter(|piece| Us::piece_is_ours(*piece))
            .count() as u8
            - 1;
        if top_stone_role != Some(Cap) && distance <= cap_stack.len() {
            let has_pure_spread =
                distance <= num_high_supports && (top_stone_role != Some(Wall) || is_hard_cap);
            if has_pure_spread {
                if position.side_to_move() == Us::color() {
                    our_value_features.critical_square_cap_attack[0] += f16::ONE;
                } else {
                    our_value_features.critical_square_cap_attack[1] += f16::ONE;
                }
            } else if position.side_to_move() == Us::color() {
                our_value_features.critical_square_cap_attack[2] += f16::ONE;
            } else {
                our_value_features.critical_square_cap_attack[3] += f16::ONE;
            }
        }
        if distance == 1 && top_stone_role != Some(Cap) {
            our_value_features.critical_square_cap_attack[4] += f16::ONE;
            our_value_features.critical_square_cap_attack[5] +=
                f16::from_u8(num_high_supports).unwrap();
        }
    }
}

fn line_score<Us: ColorTr, Them: ColorTr, const S: usize>(
    group_data: &GroupData<S>,
    line: BitBoard,
    i: u8,
    value_features: &mut ValueFeatures,
) {
    let road_pieces_in_line = (Us::road_stones(group_data) & line).count() as usize;
    let index = road_pieces_in_line + line_symmetries::<S>()[i as usize] * S;

    if !(Them::blocking_stones(group_data) & line).is_empty() {
        value_features.line_control_their_blocking_piece[index] += f16::ONE;
    } else if !((Us::walls(group_data) | Them::flats(group_data)) & line).is_empty() {
        value_features.line_control_other[index] += f16::ONE;
    } else {
        value_features.line_control_empty[index] += f16::ONE;
    }
}
