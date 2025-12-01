#[cfg(feature = "enable-log")]
use std::convert::identity;

use crate::error::ErrorCode;
use crate::libraries::{liquidity_math, tick_math};
use crate::pool::{RewardInfo, REWARD_NUM};
use crate::states::*;
use crate::Result;
use anchor_lang::prelude::*;
use bytemuck::{AnyBitPattern, NoUninit};

#[zero_copy(unsafe)]
#[repr(C, packed)]
#[derive(Default, Debug, NoUninit, AnyBitPattern)]
pub struct TickState {
    pub tick: i32,
    /// Amount of net liquidity added (subtracted) when tick is crossed from left to right (right to left)
    pub liquidity_net: i128,
    /// The total position liquidity that references this tick
    pub liquidity_gross: u128,

    /// Fee growth per unit of liquidity on the _other_ side of this tick (relative to the current tick)
    /// only has relative meaning, not absolute — the value depends on when the tick is initialized
    pub fee_growth_outside_0_x64: u128,
    pub fee_growth_outside_1_x64: u128,

    // Reward growth per unit of liquidity like fee, array of Q64.64
    pub reward_growths_outside_x64: [u128; REWARD_NUM],
    // Unused bytes for future upgrades.
    pub padding: [u32; 13],
}

impl TickState {
    pub const LEN: usize = 4 + 16 + 16 + 16 + 16 + 16 * REWARD_NUM + 16 + 16 + 8 + 8 + 4;

    pub fn initialize(&mut self, tick: i32, tick_spacing: u16) -> Result<()> {
        if TickState::check_is_out_of_boundary(tick) {
            return err!(ErrorCode::InvalidTickIndex);
        }
        require!(
            tick % i32::from(tick_spacing) == 0,
            ErrorCode::TickAndSpacingNotMatch
        );
        self.tick = tick;
        Ok(())
    }

    /// Updates a tick and returns true if the tick was flipped from initialized to uninitialized
    pub fn update(
        &mut self,
        tick_current: i32,
        liquidity_delta: i128,
        fee_growth_global_0_x64: u128,
        fee_growth_global_1_x64: u128,
        upper: bool,
        reward_infos: &[RewardInfo; REWARD_NUM],
    ) -> Result<bool> {
        let liquidity_gross_before = self.liquidity_gross;
        let liquidity_gross_after =
            liquidity_math::add_delta(liquidity_gross_before, liquidity_delta)?;

        // Either liquidity_gross_after becomes 0 (uninitialized) XOR liquidity_gross_before
        // was zero (initialized)
        let flipped = (liquidity_gross_after == 0) != (liquidity_gross_before == 0);
        if liquidity_gross_before == 0 {
            // by convention, we assume that all growth before a tick was initialized happened _below_ the tick
            if self.tick <= tick_current {
                self.fee_growth_outside_0_x64 = fee_growth_global_0_x64;
                self.fee_growth_outside_1_x64 = fee_growth_global_1_x64;
                self.reward_growths_outside_x64 = RewardInfo::get_reward_growths(reward_infos);
            }
        }

        self.liquidity_gross = liquidity_gross_after;

        // when the lower (upper) tick is crossed left to right (right to left),
        // liquidity must be added (removed)
        self.liquidity_net = if upper {
            self.liquidity_net.checked_sub(liquidity_delta)
        } else {
            self.liquidity_net.checked_add(liquidity_delta)
        }
        .unwrap();
        Ok(flipped)
    }

    /// Transitions to the current tick as needed by price movement, returning the amount of liquidity
    /// added (subtracted) when tick is crossed from left to right (right to left)
    pub fn cross(
        &mut self,
        fee_growth_global_0_x64: u128,
        fee_growth_global_1_x64: u128,
        reward_infos: &[RewardInfo; REWARD_NUM],
    ) -> i128 {
        self.fee_growth_outside_0_x64 = fee_growth_global_0_x64
            .checked_sub(self.fee_growth_outside_0_x64)
            .unwrap();
        self.fee_growth_outside_1_x64 = fee_growth_global_1_x64
            .checked_sub(self.fee_growth_outside_1_x64)
            .unwrap();

        for i in 0..REWARD_NUM {
            if !reward_infos[i].initialized() {
                continue;
            }

            self.reward_growths_outside_x64[i] = reward_infos[i]
                .reward_growth_global_x64
                .checked_sub(self.reward_growths_outside_x64[i])
                .unwrap();
        }

        self.liquidity_net
    }

    pub fn clear(&mut self) {
        self.liquidity_net = 0;
        self.liquidity_gross = 0;
        self.fee_growth_outside_0_x64 = 0;
        self.fee_growth_outside_1_x64 = 0;
        self.reward_growths_outside_x64 = [0; REWARD_NUM];
    }

    pub fn is_initialized(self) -> bool {
        self.liquidity_gross != 0
    }

    /// Common checks for a valid tick input.
    /// A tick is valid if it lies within tick boundaries
    pub fn check_is_out_of_boundary(tick: i32) -> bool {
        tick < tick_math::MIN_TICK || tick > tick_math::MAX_TICK
    }
}

pub struct TickUtils();

impl TickUtils {
    /// Input an arbitrary tick_index, output the start_index of the tick_array it sits on
    pub fn get_array_start_index(tick_index: i32, tick_spacing: u16) -> i32 {
        let ticks_in_array = Self::tick_count(tick_spacing);
        let mut start = tick_index / ticks_in_array;

        if tick_index < 0 && tick_index % ticks_in_array != 0 {
            start = start - 1
        }

        start * ticks_in_array
    }

    /// Get tick's offset in tick array, tick must be include in tick array， otherwise throw an error
    pub fn get_tick_offset_in_tick_array(
        start_tick_index: i32,
        tick_index: i32,
        tick_spacing: u16,
    ) -> Result<usize> {
        require!(tick_index >= start_tick_index, ErrorCode::InvalidTickIndex);

        let offset_in_array = ((tick_index - start_tick_index) / i32::from(tick_spacing)) as usize;
        Ok(offset_in_array)
    }

    pub fn check_is_valid_start_index(tick_index: i32, tick_spacing: u16) -> bool {
        if TickState::check_is_out_of_boundary(tick_index) {
            if tick_index > tick_math::MAX_TICK {
                return false;
            }
            let min_start_index = Self::get_array_start_index(tick_math::MIN_TICK, tick_spacing);
            return tick_index == min_start_index;
        }
        tick_index % Self::tick_count(tick_spacing) == 0
    }

    pub fn tick_count(tick_spacing: u16) -> i32 {
        TICK_ARRAY_SIZE * i32::from(tick_spacing)
    }

    /// check that the tick_array_start_index is correct for the given tick_index and tick_spacing
    pub fn check_tick_array_start_index(
        tick_array_start_index: i32,
        tick_index: i32,
        tick_spacing: u16,
    ) -> Result<()> {
        require!(
            tick_index >= tick_math::MIN_TICK,
            ErrorCode::TickLowerOverflow
        );
        require!(
            tick_index <= tick_math::MAX_TICK,
            ErrorCode::TickUpperOverflow
        );
        require_eq!(0, tick_index % i32::from(tick_spacing));
        let expect_start_index = TickUtils::get_array_start_index(tick_index, tick_spacing);
        require_eq!(tick_array_start_index, expect_start_index);
        Ok(())
    }

    /// Common checks for valid tick inputs.
    ///
    pub fn check_ticks_order(tick_lower_index: i32, tick_upper_index: i32) -> Result<()> {
        require!(
            tick_lower_index < tick_upper_index,
            ErrorCode::TickInvalidOrder
        );
        Ok(())
    }

    // Calculates the fee growths inside of tick_lower and tick_upper based on their positions relative to tick_current.
    /// `fee_growth_inside = fee_growth_global - fee_growth_below(lower) - fee_growth_above(upper)`
    ///
    pub fn get_fee_growth_inside(
        tick_lower: &TickState,
        tick_upper: &TickState,
        tick_current: i32,
        fee_growth_global_0_x64: u128,
        fee_growth_global_1_x64: u128,
    ) -> (u128, u128) {
        // calculate fee growth below
        let (fee_growth_below_0_x64, fee_growth_below_1_x64) = if tick_current >= tick_lower.tick {
            (
                tick_lower.fee_growth_outside_0_x64,
                tick_lower.fee_growth_outside_1_x64,
            )
        } else {
            (
                fee_growth_global_0_x64
                    .checked_sub(tick_lower.fee_growth_outside_0_x64)
                    .unwrap(),
                fee_growth_global_1_x64
                    .checked_sub(tick_lower.fee_growth_outside_1_x64)
                    .unwrap(),
            )
        };

        // Calculate fee growth above
        let (fee_growth_above_0_x64, fee_growth_above_1_x64) = if tick_current < tick_upper.tick {
            (
                tick_upper.fee_growth_outside_0_x64,
                tick_upper.fee_growth_outside_1_x64,
            )
        } else {
            (
                fee_growth_global_0_x64
                    .checked_sub(tick_upper.fee_growth_outside_0_x64)
                    .unwrap(),
                fee_growth_global_1_x64
                    .checked_sub(tick_upper.fee_growth_outside_1_x64)
                    .unwrap(),
            )
        };
        let fee_growth_inside_0_x64 = fee_growth_global_0_x64
            .wrapping_sub(fee_growth_below_0_x64)
            .wrapping_sub(fee_growth_above_0_x64);
        let fee_growth_inside_1_x64 = fee_growth_global_1_x64
            .wrapping_sub(fee_growth_below_1_x64)
            .wrapping_sub(fee_growth_above_1_x64);

        (fee_growth_inside_0_x64, fee_growth_inside_1_x64)
    }

    // Calculates the reward growths inside of tick_lower and tick_upper based on their positions relative to tick_current.
    pub fn get_reward_growths_inside(
        tick_lower: &TickState,
        tick_upper: &TickState,
        tick_current_index: i32,
        reward_infos: &[RewardInfo; REWARD_NUM],
    ) -> [u128; REWARD_NUM] {
        let mut reward_growths_inside = [0; REWARD_NUM];

        for i in 0..REWARD_NUM {
            if !reward_infos[i].initialized() {
                continue;
            }

            let reward_growths_below = if tick_current_index >= tick_lower.tick {
                tick_lower.reward_growths_outside_x64[i]
            } else {
                reward_infos[i]
                    .reward_growth_global_x64
                    .checked_sub(tick_lower.reward_growths_outside_x64[i])
                    .unwrap()
            };

            let reward_growths_above = if tick_current_index < tick_upper.tick {
                tick_upper.reward_growths_outside_x64[i]
            } else {
                reward_infos[i]
                    .reward_growth_global_x64
                    .checked_sub(tick_upper.reward_growths_outside_x64[i])
                    .unwrap()
            };
            reward_growths_inside[i] = reward_infos[i]
                .reward_growth_global_x64
                .wrapping_sub(reward_growths_below)
                .wrapping_sub(reward_growths_above);

            #[cfg(feature = "enable-log")]
            msg!(
            "get_reward_growths_inside,i:{},reward_growth_global:{},reward_growth_below:{},reward_growth_above:{}, reward_growth_inside:{}",
            i,
            identity(reward_infos[i].reward_growth_global_x64),
            reward_growths_below,
            reward_growths_above,
            reward_growths_inside[i]
        );
        }

        reward_growths_inside
    }
}
