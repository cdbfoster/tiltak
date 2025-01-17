use std::array;

use crate::evaluation::parameters;
use arrayvec::ArrayVec;
use board_game_traits::{Color, GameResult, Position as PositionTrait};
use half::f16;
use pgn_traits::PgnPosition;
use rand_distr::num_traits::FromPrimitive;

use crate::evaluation::parameters::PolicyFeatures;
use crate::position::bitboard::BitBoard;
use crate::position::color_trait::{BlackTr, ColorTr, WhiteTr};
use crate::position::Role::{Cap, Flat, Wall};
use crate::position::{lookup_square_symmetries, GroupData, Piece, Position, Role};
use crate::position::{squares_iterator, Move};
use crate::position::{AbstractBoard, Direction};
use crate::position::{Direction::*, ExpMove};
use crate::position::{GroupEdgeConnection, Square};

const POLICY_BASELINE: f32 = 0.05;

pub fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + f32::exp(-x))
}

pub fn inverse_sigmoid(x: f32) -> f32 {
    assert!(x > 0.0 && x < 1.0, "Tried to inverse sigmoid {}", x);
    f32::ln(x / (1.0 - x))
}

impl<const S: usize> Position<S> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn generate_moves_with_probabilities_colortr<Us: ColorTr, Them: ColorTr>(
        &self,
        params_for_color: &[f32],
        group_data: &GroupData<S>,
        simple_moves: &mut Vec<Move<S>>,
        fcd_per_move: &mut Vec<i8>,
        moves: &mut Vec<(Move<S>, f16)>,
        feature_sets: &mut Vec<Box<[f16]>>,
        policy_feature_sets: &mut Option<Vec<PolicyFeatures<'static>>>,
    ) {
        let num_moves = simple_moves.len();

        while feature_sets.len() < num_moves {
            feature_sets
                .push(vec![f16::ZERO; parameters::num_policy_features::<S>()].into_boxed_slice());
        }

        {
            // Ugly performance hack. `policy_feature_sets` is empty,
            // but creating another vec from `into_iter::collect()` re-uses the memory,
            // even though the new vector has a different type
            let mut converted: Vec<PolicyFeatures<'_>> = policy_feature_sets
                .take()
                .unwrap()
                .into_iter()
                .map(|_| unreachable!())
                .collect();

            converted.extend(
                feature_sets
                    .iter_mut()
                    .map(|feature_set| PolicyFeatures::new::<S>(feature_set)),
            );

            self.features_for_moves(&mut converted, simple_moves, fcd_per_move, group_data);

            converted.clear();
            *policy_feature_sets = Some(converted.into_iter().map(|_| unreachable!()).collect())
        }

        moves.extend(
            simple_moves
                .drain(..)
                .zip(feature_sets)
                .map(|(mv, features)| {
                    if num_moves < 2 {
                        eprintln!(
                            "Warning: Got {} legal moves for {}",
                            num_moves,
                            self.to_fen()
                        );
                    }
                    let offset = inverse_sigmoid(1.0 / num_moves.max(2) as f32);
                    assert_eq!(features.len(), params_for_color.len());

                    const SIMD_WIDTH: usize = 8;
                    assert_eq!(features.len() % SIMD_WIDTH, 0);
                    assert_eq!(features.len(), params_for_color.len());

                    let partial_sums: [f32; SIMD_WIDTH] = features
                        .chunks_exact(SIMD_WIDTH)
                        .zip(params_for_color.chunks_exact(SIMD_WIDTH))
                        .fold([0.0; SIMD_WIDTH], |acc, (c, p)| {
                            array::from_fn(|i| acc[i] + c[i].to_f32() * p[i])
                        });

                    let total_value = partial_sums.iter().sum::<f32>() + offset;

                    features.fill(f16::ZERO);

                    (mv, f16::from_f32(sigmoid(total_value)))
                }),
        );

        fcd_per_move.clear();

        let score_sum: f32 = moves.iter().map(|(_mv, score)| score.to_f32()).sum();

        let score_factor = (1.0 - POLICY_BASELINE) / score_sum;
        for (_mv, score) in moves.iter_mut() {
            *score =
                f16::from_f32(score.to_f32() * score_factor + (POLICY_BASELINE / num_moves as f32));
        }
    }

    pub fn features_for_moves(
        &self,
        feature_sets: &mut [PolicyFeatures],
        moves: &[Move<S>],
        fcd_per_move: &mut Vec<i8>,
        group_data: &GroupData<S>,
    ) {
        assert!(feature_sets.len() >= moves.len());

        let mut immediate_win_exists = false;

        let mut highest_fcd_per_square = <AbstractBoard<i8, S>>::new_with_value(-1);
        let mut highest_fcd = -1;

        for mv in moves.iter() {
            let fcd = self.fcd_for_move(*mv);
            if fcd > highest_fcd {
                highest_fcd = fcd;
            }
            if fcd > highest_fcd_per_square[mv.origin_square()] {
                highest_fcd_per_square[mv.origin_square()] = fcd;
            }
            fcd_per_move.push(fcd);
        }

        for (features_set, (mv, &mut fcd)) in
            feature_sets.iter_mut().zip(moves.iter().zip(fcd_per_move))
        {
            self.features_for_move(features_set, mv, fcd, group_data);

            // FCD bonus for all movements
            if let ExpMove::Move(square, _, _) = mv.expand() {
                if fcd >= highest_fcd {
                    features_set.fcd_highest_board[fcd.clamp(1, 6) as usize - 1] = f16::ONE;
                } else if fcd >= highest_fcd_per_square[square] {
                    features_set.fcd_highest_stack[(fcd.clamp(-1, 4) + 1) as usize] = f16::ONE;
                } else {
                    features_set.fcd_other[(fcd.clamp(-3, 4) + 3) as usize] = f16::ONE;
                }
            }

            if has_immediate_win(features_set) {
                immediate_win_exists = true;
            }
        }
        if immediate_win_exists {
            for features_set in feature_sets.iter_mut().take(moves.len()) {
                if !has_immediate_win(features_set) {
                    features_set.decline_win[0] = f16::ONE;
                }
            }
        }
    }

    fn features_for_move(
        &self,
        policy_features: &mut PolicyFeatures,
        mv: &Move<S>,
        fcd: i8,
        group_data: &GroupData<S>,
    ) {
        match self.side_to_move() {
            Color::White => features_for_move_colortr::<WhiteTr, BlackTr, S>(
                self,
                policy_features,
                mv,
                fcd,
                group_data,
            ),
            Color::Black => features_for_move_colortr::<BlackTr, WhiteTr, S>(
                self,
                policy_features,
                mv,
                fcd,
                group_data,
            ),
        }
    }
}

fn has_immediate_win(policy_features: &PolicyFeatures) -> bool {
    [
        policy_features.place_to_win[0],
        policy_features.place_our_critical_square[0],
        policy_features.move_onto_critical_square[0],
        policy_features.move_onto_critical_square[1],
        policy_features.spread_that_connects_groups_to_win[0],
    ]
    .into_iter()
    .any(|p| p != f16::ZERO)
}

struct MovementSynopsis<const S: usize> {
    origin: Square<S>,
    destination: Square<S>,
}

fn our_last_placement<const S: usize>(position: &Position<S>) -> Option<(Role, Square<S>)> {
    position
        .moves()
        .get(position.moves().len().overflowing_sub(2).0)
        .and_then(|mv| match mv.expand() {
            ExpMove::Place(role, square) => Some((role, square)),
            ExpMove::Move(_, _, _) => None,
        })
}

fn their_last_placement<const S: usize>(position: &Position<S>) -> Option<(Role, Square<S>)> {
    position
        .moves()
        .get(position.moves().len().overflowing_sub(1).0)
        .and_then(|mv| match mv.expand() {
            ExpMove::Place(role, square) => Some((role, square)),
            ExpMove::Move(_, _, _) => None,
        })
}

fn our_last_movement<const S: usize>(position: &Position<S>) -> Option<MovementSynopsis<S>> {
    get_movement_in_history(position, 2)
}

fn their_last_movement<const S: usize>(position: &Position<S>) -> Option<MovementSynopsis<S>> {
    get_movement_in_history(position, 1)
}

fn get_movement_in_history<const S: usize>(
    position: &Position<S>,
    i: usize,
) -> Option<MovementSynopsis<S>> {
    position
        .moves()
        .get(position.moves().len().overflowing_sub(i).0)
        .and_then(|mv| match mv.expand() {
            ExpMove::Place(_, _) => None,
            ExpMove::Move(origin, direction, stack_movement) => Some(MovementSynopsis {
                origin,
                destination: origin
                    .jump_direction(direction, stack_movement.len() as u8)
                    .unwrap(),
            }),
        })
}

fn features_for_move_colortr<Us: ColorTr, Them: ColorTr, const S: usize>(
    position: &Position<S>,
    policy_features: &mut PolicyFeatures,
    mv: &Move<S>,
    fcd: i8,
    group_data: &GroupData<S>,
) {
    // If it's the first move, give every move equal probability
    if position.half_moves_played() < 2 {
        return;
    }

    let our_flatcount = Us::flats(group_data).count() as i8;
    let their_flatcount = Them::flats(group_data).count() as i8;

    let our_flatcount_after_move = our_flatcount + fcd;

    match mv.expand() {
        ExpMove::Place(role, square) => {
            let our_flat_lead_after_move = our_flatcount_after_move - their_flatcount;

            // Apply special bonuses if the game ends on this move
            if Us::stones_left(position) + Us::caps_left(position) == 1
                || group_data.all_pieces().count() as usize == S * S - 1
            {
                check_flat_win::<Us, S>(
                    position,
                    our_flatcount_after_move,
                    their_flatcount,
                    policy_features,
                );
            }
            // Bonuses if our opponent can finish on flats next turn
            else if Them::stones_left(position) + Them::caps_left(position) == 1
                || group_data.all_pieces().count() as usize == S * S - 2
            {
                check_flat_win_next_move::<Us, S>(
                    position,
                    our_flatcount_after_move,
                    their_flatcount,
                    policy_features,
                );
            }
            // TODO: These two bonuses don't take komi into account, but they should
            else if Us::stones_left(position) == 2 && Us::caps_left(position) == 0 {
                policy_features.two_flats_left[0] = f16::ONE;
                policy_features.two_flats_left[1] = f16::from(our_flat_lead_after_move);
            } else if Us::stones_left(position) == 3 && Us::caps_left(position) == 0 {
                policy_features.three_flats_left[0] = f16::ONE;
                policy_features.three_flats_left[1] = f16::from(our_flat_lead_after_move);
            }

            let their_open_critical_squares =
                Them::critical_squares(group_data) & (!group_data.all_pieces());

            // Apply PSQT
            match (role, position.side_to_move()) {
                (Flat, Color::White) => {
                    policy_features.flat_psqt_white[lookup_square_symmetries::<S>(square)] =
                        f16::ONE
                }
                (Flat, Color::Black) => {
                    policy_features.flat_psqt_black[lookup_square_symmetries::<S>(square)] =
                        f16::ONE
                }
                (Wall, Color::White) => {
                    policy_features.wall_psqt_white[lookup_square_symmetries::<S>(square)] =
                        f16::ONE
                }
                (Wall, Color::Black) => {
                    policy_features.wall_psqt_black[lookup_square_symmetries::<S>(square)] =
                        f16::ONE
                }
                (Cap, Color::White) => {
                    policy_features.cap_psqt_white[lookup_square_symmetries::<S>(square)] = f16::ONE
                }
                (Cap, Color::Black) => {
                    policy_features.cap_psqt_black[lookup_square_symmetries::<S>(square)] = f16::ONE
                }
            }

            let role_id = match role {
                Flat => 0,
                Wall => 1,
                Cap => 2,
            };

            for &line in BitBoard::lines_for_square::<S>(square).iter() {
                let our_line_score = (Us::road_stones(group_data) & line).count();
                let their_line_score = (Them::road_stones(group_data) & line).count();
                policy_features.our_road_stones_in_line[S * role_id + our_line_score as usize] +=
                    f16::ONE;
                policy_features.their_road_stones_in_line
                    [S * role_id + their_line_score as usize] += f16::ONE;
            }

            // If square is next to a group
            let mut our_unique_neighbour_groups: ArrayVec<(Square<S>, u8), 4> = ArrayVec::new();
            let mut their_unique_neighbour_groups: ArrayVec<(Square<S>, u8), 4> = ArrayVec::new();
            for neighbour in square.neighbors().filter(|sq| !position[*sq].is_empty()) {
                let neighbour_group_id = group_data.groups[neighbour];
                if Us::piece_is_ours(position[neighbour].top_stone().unwrap()) {
                    if our_unique_neighbour_groups
                        .iter()
                        .all(|(_sq, id)| *id != neighbour_group_id)
                    {
                        our_unique_neighbour_groups.push((neighbour, neighbour_group_id));
                    }
                } else if their_unique_neighbour_groups
                    .iter()
                    .all(|(_sq, id)| *id != neighbour_group_id)
                {
                    their_unique_neighbour_groups.push((neighbour, neighbour_group_id));
                }
            }

            if our_unique_neighbour_groups.len() > 1 {
                let total_neighbours_group_size: f32 = our_unique_neighbour_groups
                    .iter()
                    .map(|(_, group_id)| group_data.amount_in_group[*group_id as usize].0 as f32)
                    .sum();

                policy_features.merge_two_groups_base[role_id] = f16::ONE;
                // Divide by 10, as large values confuse the tuner
                policy_features.merge_two_groups_linear[role_id] =
                    f16::from_f32(total_neighbours_group_size / 10.0);
            }

            if their_unique_neighbour_groups.len() > 1 {
                let total_neighbours_group_size: f32 = their_unique_neighbour_groups
                    .iter()
                    .map(|(_, group_id)| group_data.amount_in_group[*group_id as usize].0 as f32)
                    .sum();

                policy_features.block_merger_base[role_id] = f16::ONE;
                // Divide by 10, as large values confuse the tuner
                policy_features.block_merger_linear[role_id] =
                    f16::from_f32(total_neighbours_group_size / 10.0);
            }
            if our_unique_neighbour_groups.len() == 1 {
                let group_id = our_unique_neighbour_groups[0].1;
                let amount_in_group = group_data.amount_in_group[group_id as usize].0 as f32;

                policy_features.extend_single_group_base[role_id] = f16::ONE;
                // Divide by 10, as large values confuse the tuner
                policy_features.extend_single_group_linear[role_id] =
                    f16::from_f32(amount_in_group / 10.0);

                // Apply a separate bonus if the piece expands the group to a new line
                if squares_iterator::<S>()
                    .filter(|sq| group_data.groups[*sq] == group_id)
                    .all(|sq| sq.file() != square.file())
                    || squares_iterator::<S>()
                        .filter(|sq| group_data.groups[*sq] == group_id)
                        .all(|sq| sq.rank() != square.rank())
                {
                    policy_features.extend_single_group_to_new_line_base[role_id] = f16::ONE;
                    policy_features.extend_single_group_to_new_line_linear[role_id] =
                        f16::from_f32(amount_in_group / 10.0);
                }
            }

            if role == Flat || role == Cap {
                if Us::is_critical_square(group_data, square) {
                    policy_features.place_our_critical_square[0] += f16::ONE;
                } else if !their_open_critical_squares.is_empty() {
                    if their_open_critical_squares == BitBoard::empty().set_square(square) {
                        policy_features.place_their_critical_square[0] += f16::ONE;
                    } else {
                        policy_features.ignore_their_critical_square[0] += f16::ONE;
                    }
                }

                // If square is next to a road stone laid on our last turn
                if let Some((last_role, last_square)) = our_last_placement(position) {
                    if last_role == Flat || last_role == Cap {
                        if square.neighbors().any(|neigh| neigh == last_square) {
                            policy_features.next_to_our_last_stone[0] = f16::ONE;
                        } else if (square.rank() as i8 - last_square.rank() as i8).abs() == 1
                            && (square.file() as i8 - last_square.file() as i8).abs() == 1
                        {
                            policy_features.diagonal_to_our_last_stone[0] = f16::ONE;
                        }
                    }
                }

                // If square is next to a road stone laid on their last turn
                if let Some((last_role, last_square)) = their_last_placement(position) {
                    if last_role == Flat {
                        if square.neighbors().any(|neigh| neigh == last_square) {
                            policy_features.next_to_their_last_stone[0] = f16::ONE;
                        } else if (square.rank() as i8 - last_square.rank() as i8).abs() == 1
                            && (square.file() as i8 - last_square.file() as i8).abs() == 1
                        {
                            policy_features.diagonal_to_their_last_stone[0] = f16::ONE;
                        }
                    }
                }

                // Bonus for attacking a flatstone in a rank/file where we are strong
                for neighbour in square.neighbors() {
                    if position[neighbour].top_stone() == Some(Them::flat_piece()) {
                        let our_road_stones = Us::road_stones(group_data)
                            .rank::<S>(neighbour.rank())
                            .count()
                            + Us::road_stones(group_data)
                                .file::<S>(neighbour.file())
                                .count();
                        if our_road_stones >= 2 {
                            policy_features.attack_strong_flats[0] +=
                                f16::from(our_road_stones - 1);
                        }
                    }
                }
            }

            if role == Wall {
                if !their_open_critical_squares.is_empty() {
                    if their_open_critical_squares == BitBoard::empty().set_square(square) {
                        policy_features.place_their_critical_square[1] += f16::ONE;
                    } else {
                        policy_features.ignore_their_critical_square[0] += f16::ONE;
                    }
                }
            } else if role == Cap {
                if Us::is_critical_square(group_data, square) {
                    policy_features.place_our_critical_square[0] += f16::ONE;
                } else if !their_open_critical_squares.is_empty() {
                    if their_open_critical_squares == BitBoard::empty().set_square(square) {
                        policy_features.place_their_critical_square[2] += f16::ONE;
                    } else {
                        policy_features.ignore_their_critical_square[0] += f16::ONE;
                    }
                }
            }
            if role == Wall || role == Cap {
                for (direction, neighbour) in square.direction_neighbors() {
                    // If square blocks an extension of two flats
                    if position[neighbour]
                        .top_stone()
                        .map(Them::is_road_stone)
                        .unwrap_or_default()
                        && neighbour
                            .go_direction(direction)
                            .and_then(|sq| position[sq].top_stone())
                            .map(Them::is_road_stone)
                            .unwrap_or_default()
                    {
                        policy_features.blocking_stone_blocks_extensions_of_two_flats[0] +=
                            f16::ONE;
                    }

                    if position[neighbour].len() > 2
                        && Them::piece_is_ours(position[neighbour].top_stone().unwrap())
                    {
                        let stack = position[neighbour];
                        let top_stone = stack.top_stone().unwrap();
                        let mut captives = 0;
                        let mut reserves = 0;
                        for piece in stack.into_iter().take(stack.len() as usize - 1) {
                            if Us::piece_is_ours(piece) {
                                captives += 1;
                            } else {
                                reserves += 1;
                            }
                        }
                        let index = top_stone.role().disc() * 2;
                        match role {
                            Flat => unreachable!(),
                            Wall => {
                                policy_features.attack_strong_stack_with_wall[index] +=
                                    f16::from_i32(captives).unwrap();
                                policy_features.attack_strong_stack_with_wall[index + 1] +=
                                    f16::from_i32(reserves).unwrap();
                            }
                            Cap => {
                                policy_features.attack_strong_stack_with_cap[index] +=
                                    f16::from_i32(captives).unwrap();
                                policy_features.attack_strong_stack_with_cap[index + 1] +=
                                    f16::from_i32(reserves).unwrap();
                            }
                        }

                        if let Some(MovementSynopsis {
                            origin: _,
                            destination,
                        }) = their_last_movement(position)
                        {
                            if neighbour == destination {
                                policy_features.attack_last_movement[0] +=
                                    f16::from_i32(captives).unwrap();
                                policy_features.attack_last_movement[1] +=
                                    f16::from_i32(reserves).unwrap();
                            }
                        }
                    }
                }
            }

            // Bonus for placing on the square left behind by their movement
            if let Some(MovementSynopsis {
                origin,
                destination: _,
            }) = their_last_movement(position)
            {
                if square == origin {
                    policy_features.place_last_movement[role_id] += f16::ONE;
                }
            }
        }

        ExpMove::Move(square, direction, stack_movement) => {
            let role_id = match position[square].top_stone().unwrap().role() {
                Flat => 0,
                Wall => 1,
                Cap => 2,
            };
            match position.side_to_move() {
                Color::White => policy_features.move_role_bonus_white[role_id] += f16::ONE,
                Color::Black => policy_features.move_role_bonus_black[role_id] += f16::ONE,
            }

            if stack_movement.len() == 1
                && stack_movement.get_first().pieces_to_take == 1
                && position[square].len() == 1
            {
                if let Some(piece) = position[square.go_direction(direction).unwrap()].top_stone() {
                    match (piece.role(), piece.color() == Us::color()) {
                        (Flat, true) => policy_features.simple_self_capture[role_id] = f16::ONE,
                        (Flat, false) => policy_features.simple_capture[role_id] = f16::ONE,
                        (Wall, true) => policy_features.simple_self_capture[3] = f16::ONE,
                        (Wall, false) => policy_features.simple_capture[3] = f16::ONE,
                        _ => unreachable!(),
                    }
                } else {
                    policy_features.simple_movement[role_id] = f16::ONE;
                }
            }

            let mut destination_square =
                if stack_movement.get_first().pieces_to_take == position[square].len() {
                    square.go_direction(direction).unwrap()
                } else {
                    square
                };

            // Bonus for moving the piece we placed on our last turn
            if let Some((role, last_square)) = our_last_placement(position) {
                if square == last_square && !position[destination_square].is_empty() {
                    policy_features.move_last_placement[role.disc()] += f16::ONE;
                }
            }

            let mut captures_our_critical_square = None;
            let mut captures_their_critical_square = None;
            let mut loses_their_critical_square = None;

            // The groups that become connected through this move
            let mut our_groups_joined = <ArrayVec<u8, 10>>::new();
            let mut their_piece_left_on_previous_square = false;
            // Edge connections created by this move
            let mut group_edge_connection = GroupEdgeConnection::default();

            // The groups where the move causes us to lose flats
            let mut our_groups_affected = <ArrayVec<u8, S>>::new();
            let mut our_squares_affected = <ArrayVec<Square<S>, S>>::new();
            let mut stack_recaptured_with = None;

            // Number of squares given to them
            let mut their_pieces = 0;
            // Number of squares captured by us, that were previously held by them
            let mut their_pieces_captured = 0;
            let mut num_squares_covered = group_data.all_pieces().count();

            // Special case for when we spread the whole stack
            if position[square].len() == stack_movement.get_first().pieces_to_take {
                num_squares_covered -= 1;
                let top_stone: Piece = position[square].top_stone.unwrap();
                if top_stone.is_road_piece() {
                    our_squares_affected.push(square);

                    if spread_damages_our_group::<S, Us>(position, square, direction) {
                        our_groups_affected.push(group_data.groups[square]);
                    }
                }
            }

            // This iterator skips the first square if we move the whole stack
            for piece in position
                .top_stones_left_behind_by_move(square, &stack_movement)
                .flatten()
            {
                let destination_stack = &position[destination_square];

                if Us::piece_is_ours(piece) {
                    if Us::is_critical_square(group_data, destination_square)
                        && piece.is_road_piece()
                    {
                        captures_our_critical_square = Some(destination_square);
                    }
                    if Them::is_critical_square(group_data, destination_square) {
                        captures_their_critical_square = Some(destination_square);
                    }
                    if let Some(MovementSynopsis {
                        origin: _,
                        destination: last_capture,
                    }) = their_last_movement(position)
                    {
                        if destination_square == last_capture {
                            stack_recaptured_with = Some(piece.role());
                        }
                    }
                } else {
                    their_pieces += 1;
                    if Them::is_critical_square(group_data, destination_square) {
                        // TODO: Filling their critical square needs a malus
                        loses_their_critical_square = Some(destination_square);
                    }
                }

                if Us::piece_is_ours(piece) && piece.is_road_piece() {
                    let mut neighbour_group_ids = <ArrayVec<u8, S>>::new();

                    for neighbour in Square::neighbors(destination_square) {
                        if destination_square != square
                            && destination_square.go_direction(direction.reverse())
                                == Some(neighbour)
                        {
                            continue;
                        }
                        if let Some(neighbour_piece) = position[neighbour].top_stone() {
                            if Us::piece_is_ours(neighbour_piece) && neighbour_piece.is_road_piece()
                            {
                                neighbour_group_ids.push(group_data.groups[neighbour]);
                            }
                        }
                    }

                    // If our stack spread doesn't form one continuous group,
                    // "disconnect" from previous groups
                    if their_piece_left_on_previous_square
                        && our_groups_joined
                            .iter()
                            .all(|g| !neighbour_group_ids.contains(g))
                    {
                        our_groups_joined.clear();
                        group_edge_connection = GroupEdgeConnection::default();
                    }
                    group_edge_connection =
                        group_edge_connection | destination_square.group_edge_connection();

                    for group_id in neighbour_group_ids {
                        if !our_groups_joined.contains(&group_id) {
                            our_groups_joined.push(group_id);
                        }
                    }
                    their_piece_left_on_previous_square = false;
                } else {
                    their_piece_left_on_previous_square = true;
                    // We may have joined this group on the previous iteration
                    // If so, remove it, since the group is now affected
                    our_groups_joined.retain(|id| *id != group_data.groups[destination_square]);
                }

                // Bonus for moving our cap to a strong line
                // Extra bonus if it lands next to our critical square
                if piece == Us::cap_piece() {
                    let destination_line = match direction {
                        North => Us::road_stones(group_data).rank::<S>(destination_square.rank()),
                        West => Us::road_stones(group_data).file::<S>(destination_square.file()),
                        East => Us::road_stones(group_data).file::<S>(destination_square.file()),
                        South => Us::road_stones(group_data).rank::<S>(destination_square.rank()),
                    };
                    let road_piece_count = destination_line.count() as usize;
                    if road_piece_count > 2 {
                        policy_features.move_cap_onto_strong_line[road_piece_count - 3] += f16::ONE;
                        if destination_square
                            .neighbors()
                            .any(|n| Us::is_critical_square(group_data, n))
                        {
                            policy_features.move_cap_onto_strong_line_with_critical_square
                                [road_piece_count - 3] += f16::ONE;
                        }
                    }
                }

                if let Some(destination_top_stone) = destination_stack.top_stone() {
                    // When a stack gets captured, give a linear bonus or malus depending on
                    // whether it's captured by us or them
                    if piece.color() != destination_top_stone.color() {
                        if Us::piece_is_ours(piece) {
                            policy_features.stack_captured_by_movement[0] +=
                                f16::from(destination_stack.len());
                            their_pieces_captured += 1;
                        } else {
                            policy_features.stack_captured_by_movement[0] -=
                                f16::from(destination_stack.len());
                            our_squares_affected.push(destination_square);

                            if destination_square != square
                                || spread_damages_our_group::<S, Us>(
                                    position,
                                    destination_square,
                                    direction,
                                )
                            {
                                our_groups_affected.push(group_data.groups[destination_square]);
                            }
                        }
                    }
                    if Us::piece_is_ours(destination_top_stone) && piece.role() == Wall {
                        our_squares_affected.push(destination_square);
                        our_groups_affected.push(group_data.groups[destination_square]);
                    }

                    for &line in BitBoard::lines_for_square::<S>(destination_square).iter() {
                        let our_road_stones = (line & Us::road_stones(group_data)).count() as usize;
                        let color_factor = if Us::piece_is_ours(piece) { 1.0 } else { -1.0 };
                        if our_road_stones > 2 {
                            if piece.role() == Cap {
                                policy_features.stack_capture_in_strong_line_cap
                                    [our_road_stones - 3] +=
                                    f16::from_f32(color_factor * destination_stack.len() as f32);
                            } else {
                                policy_features.stack_capture_in_strong_line
                                    [our_road_stones - 3] +=
                                    f16::from_f32(color_factor * destination_stack.len() as f32);
                            }
                        }
                    }
                } else {
                    num_squares_covered += 1;
                }

                destination_square = destination_square
                    .go_direction(direction)
                    .unwrap_or(destination_square);
            }

            // Check for board fill on this move and the next
            if num_squares_covered == S as u8 * S as u8 && loses_their_critical_square.is_none() {
                // TODO: Maybe add separate policy features for this?
                // It's possible that the spread that board fills also makes them a road
                check_flat_win::<Us, S>(
                    position,
                    our_flatcount_after_move,
                    their_flatcount,
                    policy_features,
                );
            } else if num_squares_covered == S as u8 * S as u8 - 1 {
                check_flat_win_next_move::<Us, S>(
                    position,
                    our_flatcount_after_move,
                    their_flatcount,
                    policy_features,
                );
            }

            if their_pieces == 0 {
                policy_features.pure_spread[0] = f16::ONE;
            } else {
                policy_features.pure_spread[1] = f16::ONE;
            }

            // Continue spreading the stack (the piece, that is) we spread last turn, if any
            if let Some(MovementSynopsis {
                origin: _,
                destination,
            }) = our_last_movement(position)
            {
                if destination == square {
                    policy_features.continue_spread[role_id] = f16::ONE;
                }
            }

            // Recapture the stack they moved on their last move
            if let Some(role) = stack_recaptured_with {
                if their_pieces == 0 {
                    policy_features.recapture_stack_pure[role as u16 as usize] = f16::ONE;
                } else {
                    policy_features.recapture_stack_impure[role as u16 as usize] = f16::ONE;
                }
            }

            let their_open_critical_squares =
                Them::critical_squares(group_data) & (!group_data.all_pieces());

            if !their_open_critical_squares.is_empty() {
                if their_pieces_captured == 0 && captures_their_critical_square.is_none() {
                    // Move ignores their critical threat, but might win for us
                    policy_features.ignore_their_critical_square[1] += f16::ONE
                } else {
                    // Move captures at least one stack, which might save us
                    policy_features.place_their_critical_square[3] +=
                        f16::from_i32(their_pieces_captured).unwrap();
                }
            }

            // Bonus for moving onto a critical square
            if let Some(critical_square) = captures_our_critical_square {
                // Start with a very simple check for throwing onto a straight road
                let our_road_stones = Us::road_stones(group_data);
                if our_road_stones.file::<S>(critical_square.file()).count() == S as u8 - 1
                    && (direction == East || direction == West)
                    || our_road_stones.rank::<S>(critical_square.rank()).count() == S as u8 - 1
                        && (direction == North || direction == South)
                {
                    // Only this option is a guaranteed win:
                    policy_features.move_onto_critical_square[0] += f16::ONE;
                } else {
                    // Check if reaching the critical square still wins, in case our
                    // stack spread lost some of our flats
                    let mut edge_connection = critical_square.group_edge_connection();
                    for neighbour in critical_square.neighbors() {
                        if let Some(neighbour_piece) = position[neighbour].top_stone() {
                            if Us::piece_is_ours(neighbour_piece) {
                                let group_id = group_data.groups[neighbour];
                                if our_groups_affected.iter().all(|g| *g != group_id) {
                                    edge_connection = edge_connection
                                        | group_data.amount_in_group[group_id as usize].1;
                                }
                            }
                        }
                    }

                    if edge_connection.is_winning() {
                        // Only this option is a guaranteed win:
                        policy_features.move_onto_critical_square[0] += f16::ONE;
                    }
                    // If the critical square has two neighbours of the same group,
                    // and neither the origin square nor the critical square is a wall,
                    // at least one of the spreads onto the critical square will be a road win
                    else if our_squares_affected.len() == 1
                        && critical_square
                            .neighbors()
                            .any(|sq| sq == our_squares_affected[0])
                        && critical_square
                            .neighbors()
                            .filter(|sq| {
                                group_data.groups[*sq] == group_data.groups[our_squares_affected[0]]
                            })
                            .count()
                            > 1
                        && position[critical_square].top_stone().map(Piece::role) != Some(Wall)
                    {
                        policy_features.move_onto_critical_square[1] += f16::ONE
                    } else {
                        policy_features.move_onto_critical_square[2] += f16::ONE
                    }
                }
            }

            for group_id in our_groups_joined {
                if !our_groups_affected.contains(&group_id) {
                    group_edge_connection =
                        group_edge_connection | group_data.amount_in_group[group_id as usize].1;
                }
            }

            if group_edge_connection.is_winning() {
                policy_features.spread_that_connects_groups_to_win[0] = f16::ONE;
            }
        }
    }
}

fn check_flat_win_next_move<Us: ColorTr, const S: usize>(
    position: &Position<S>,
    our_flatcount_after_move: i8,
    their_flatcount: i8,
    policy_features: &mut PolicyFeatures<'_>,
) {
    if Us::color() == Color::White {
        match position
            .komi()
            .game_result_with_flatcounts(our_flatcount_after_move, their_flatcount + 1)
        {
            GameResult::WhiteWin => policy_features.place_to_allow_opponent_to_end[2] = f16::ONE,
            GameResult::BlackWin => policy_features.place_to_allow_opponent_to_end[0] = f16::ONE,
            GameResult::Draw => policy_features.place_to_allow_opponent_to_end[1] = f16::ONE,
        }
    } else {
        match position
            .komi()
            .game_result_with_flatcounts(their_flatcount + 1, our_flatcount_after_move)
        {
            GameResult::WhiteWin => policy_features.place_to_allow_opponent_to_end[0] = f16::ONE,
            GameResult::BlackWin => policy_features.place_to_allow_opponent_to_end[2] = f16::ONE,
            GameResult::Draw => policy_features.place_to_allow_opponent_to_end[1] = f16::ONE,
        }
    }
}

fn check_flat_win<Us: ColorTr, const S: usize>(
    position: &Position<S>,
    our_flatcount_after_move: i8,
    their_flatcount: i8,
    policy_features: &mut PolicyFeatures<'_>,
) {
    if Us::color() == Color::White {
        match position
            .komi()
            .game_result_with_flatcounts(our_flatcount_after_move, their_flatcount)
        {
            GameResult::WhiteWin => policy_features.place_to_win[0] = f16::ONE,
            GameResult::BlackWin => policy_features.place_to_loss[0] = f16::ONE,
            GameResult::Draw => policy_features.place_to_draw[0] = f16::ONE,
        }
    } else {
        match position
            .komi()
            .game_result_with_flatcounts(their_flatcount, our_flatcount_after_move)
        {
            GameResult::WhiteWin => policy_features.place_to_loss[0] = f16::ONE,
            GameResult::BlackWin => policy_features.place_to_win[0] = f16::ONE,
            GameResult::Draw => policy_features.place_to_draw[0] = f16::ONE,
        }
    }
}

/// For a spread that starts from this square, determine if the spread does not damage the group it's part of,
/// for example because of a citadel
fn spread_damages_our_group<const S: usize, Us: ColorTr>(
    position: &Position<S>,
    square: Square<S>,
    direction: Direction,
) -> bool {
    let behind_square = square.go_direction(direction.reverse());

    !direction
        .orthogonal_directions()
        .into_iter()
        .filter(|orthogonal| square.go_direction(*orthogonal).is_some())
        .any(|orthogonal| {
            let flank_square = square.go_direction(orthogonal).unwrap();
            let opposite_flank = square.go_direction(orthogonal.reverse());

            position[flank_square]
                .top_stone()
                .is_some_and(Us::is_road_stone)
                && position[flank_square.go_direction(direction).unwrap()]
                    .top_stone()
                    .is_some_and(Us::is_road_stone)
                && (opposite_flank.is_none() // This is probably not fully correct, it assumes the connection to the edge will be restored because the next piece dropped is ours
                || behind_square.is_none() // Ditto
                || !position[opposite_flank.unwrap()]
                    .top_stone()
                    .is_some_and(Us::is_road_stone))
                && (behind_square.is_none()
                    || !position[behind_square.unwrap()]
                        .top_stone()
                        .is_some_and(Us::is_road_stone)
                    || position[behind_square.unwrap().go_direction(orthogonal).unwrap()]
                        .top_stone()
                        .is_some_and(Us::is_road_stone))
        })
}
