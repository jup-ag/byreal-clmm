use super::full_math::MulDiv;
use super::liquidity_math;
use super::sqrt_price_math;
use crate::error::ErrorCode;
use crate::instructions::SwapState;
use crate::libraries::swap_math;
use crate::libraries::tick_math;
use crate::states::config::FEE_RATE_DENOMINATOR_VALUE;
use crate::states::AmmConfig;
use crate::states::DynTickArrayState;
use crate::states::PoolState;
use crate::states::TickArrayBitmapExtension;
use crate::states::TickArrayState;
use crate::states::TickState;
use crate::states::TickUtils;
use crate::states::POOL_TICK_ARRAY_BITMAP_SEED;
use crate::states::TICK_ARRAY_SEED;
use anchor_lang::prelude::*;
/// Result of a swap step
#[derive(Default, Debug)]
pub struct SwapStep {
    /// The price after swapping the amount in/out, not to exceed the price target
    pub sqrt_price_next_x64: u128,
    pub amount_in: u64,
    pub amount_out: u64,
    pub fee_amount: u64,
}

/// Computes the result of swapping some amount in, or amount out, given the parameters of the swap
pub fn compute_swap_step(
    sqrt_price_current_x64: u128,
    sqrt_price_target_x64: u128,
    liquidity: u128,
    amount_remaining: u64,
    fee_rate: u32,
    is_base_input: bool,
    zero_for_one: bool,
    block_timestamp: u32,
) -> Result<SwapStep> {
    // let exact_in = amount_remaining >= 0;
    let mut swap_step = SwapStep::default();
    if is_base_input {
        // round up amount_in
        // In exact input case, amount_remaining is positive
        let amount_remaining_less_fee = (amount_remaining as u64)
            .mul_div_floor(
                (FEE_RATE_DENOMINATOR_VALUE - fee_rate).into(),
                u64::from(FEE_RATE_DENOMINATOR_VALUE),
            )
            .unwrap();

        let amount_in = calculate_amount_in_range(
            sqrt_price_current_x64,
            sqrt_price_target_x64,
            liquidity,
            zero_for_one,
            is_base_input,
            block_timestamp,
        )?;
        if amount_in.is_some() {
            swap_step.amount_in = amount_in.unwrap();
        }

        swap_step.sqrt_price_next_x64 =
            if amount_in.is_some() && amount_remaining_less_fee >= swap_step.amount_in {
                sqrt_price_target_x64
            } else {
                sqrt_price_math::get_next_sqrt_price_from_input(
                    sqrt_price_current_x64,
                    liquidity,
                    amount_remaining_less_fee,
                    zero_for_one,
                )
            };
    } else {
        let amount_out = calculate_amount_in_range(
            sqrt_price_current_x64,
            sqrt_price_target_x64,
            liquidity,
            zero_for_one,
            is_base_input,
            block_timestamp,
        )?;
        if amount_out.is_some() {
            swap_step.amount_out = amount_out.unwrap();
        }
        // In exact output case, amount_remaining is negative
        swap_step.sqrt_price_next_x64 =
            if amount_out.is_some() && amount_remaining >= swap_step.amount_out {
                sqrt_price_target_x64
            } else {
                sqrt_price_math::get_next_sqrt_price_from_output(
                    sqrt_price_current_x64,
                    liquidity,
                    amount_remaining,
                    zero_for_one,
                )
            }
    }

    // whether we reached the max possible price for the given ticks
    let max = sqrt_price_target_x64 == swap_step.sqrt_price_next_x64;
    // get the input / output amounts when target price is not reached
    if zero_for_one {
        // if max is reached for exact input case, entire amount_in is needed
        if !(max && is_base_input) {
            swap_step.amount_in = liquidity_math::get_delta_amount_0_unsigned(
                swap_step.sqrt_price_next_x64,
                sqrt_price_current_x64,
                liquidity,
                true,
            )?
        };
        // if max is reached for exact output case, entire amount_out is needed
        if !(max && !is_base_input) {
            swap_step.amount_out = liquidity_math::get_delta_amount_1_unsigned(
                swap_step.sqrt_price_next_x64,
                sqrt_price_current_x64,
                liquidity,
                false,
            )?;
        };
    } else {
        if !(max && is_base_input) {
            swap_step.amount_in = liquidity_math::get_delta_amount_1_unsigned(
                sqrt_price_current_x64,
                swap_step.sqrt_price_next_x64,
                liquidity,
                true,
            )?
        };
        if !(max && !is_base_input) {
            swap_step.amount_out = liquidity_math::get_delta_amount_0_unsigned(
                sqrt_price_current_x64,
                swap_step.sqrt_price_next_x64,
                liquidity,
                false,
            )?
        };
    }

    // For exact output case, cap the output amount to not exceed the remaining output amount
    if !is_base_input && swap_step.amount_out > amount_remaining {
        swap_step.amount_out = amount_remaining;
    }

    swap_step.fee_amount =
        if is_base_input && swap_step.sqrt_price_next_x64 != sqrt_price_target_x64 {
            // we didn't reach the target, so take the remainder of the maximum input as fee
            // swap dust is granted as fee
            u64::from(amount_remaining)
                .checked_sub(swap_step.amount_in)
                .unwrap()
        } else {
            // take pip percentage as fee
            swap_step
                .amount_in
                .mul_div_ceil(
                    fee_rate.into(),
                    (FEE_RATE_DENOMINATOR_VALUE - fee_rate).into(),
                )
                .unwrap()
        };

    Ok(swap_step)
}

/// Pre calcumate amount_in or amount_out for the specified price range
/// The amount maybe overflow of u64 due to the `sqrt_price_target_x64` maybe unreasonable.
/// Therefore, this situation needs to be handled in `compute_swap_step` to recalculate the price that can be reached based on the amount.
#[cfg(not(test))]
fn calculate_amount_in_range(
    sqrt_price_current_x64: u128,
    sqrt_price_target_x64: u128,
    liquidity: u128,
    zero_for_one: bool,
    is_base_input: bool,
    _block_timestamp: u32,
) -> Result<Option<u64>> {
    if is_base_input {
        let result = if zero_for_one {
            liquidity_math::get_delta_amount_0_unsigned(
                sqrt_price_target_x64,
                sqrt_price_current_x64,
                liquidity,
                true,
            )
        } else {
            liquidity_math::get_delta_amount_1_unsigned(
                sqrt_price_current_x64,
                sqrt_price_target_x64,
                liquidity,
                true,
            )
        };

        if result.is_ok() {
            return Ok(Some(result.unwrap()));
        } else {
            if result.err().unwrap() == crate::error::ErrorCode::MaxTokenOverflow.into() {
                return Ok(None);
            } else {
                return Err(ErrorCode::SqrtPriceLimitOverflow.into());
            }
        }
    } else {
        let result = if zero_for_one {
            liquidity_math::get_delta_amount_1_unsigned(
                sqrt_price_target_x64,
                sqrt_price_current_x64,
                liquidity,
                false,
            )
        } else {
            liquidity_math::get_delta_amount_0_unsigned(
                sqrt_price_current_x64,
                sqrt_price_target_x64,
                liquidity,
                false,
            )
        };
        if result.is_ok() {
            return Ok(Some(result.unwrap()));
        } else {
            if result.err().unwrap() == crate::error::ErrorCode::MaxTokenOverflow.into() {
                return Ok(None);
            } else {
                return Err(ErrorCode::SqrtPriceLimitOverflow.into());
            }
        }
    }
}

#[cfg(test)]
fn calculate_amount_in_range(
    sqrt_price_current_x64: u128,
    sqrt_price_target_x64: u128,
    liquidity: u128,
    zero_for_one: bool,
    is_base_input: bool,
    block_timestamp: u32,
) -> Result<Option<u64>> {
    if is_base_input {
        let result = if zero_for_one {
            liquidity_math::get_delta_amount_0_unsigned(
                sqrt_price_target_x64,
                sqrt_price_current_x64,
                liquidity,
                true,
            )
        } else {
            liquidity_math::get_delta_amount_1_unsigned(
                sqrt_price_current_x64,
                sqrt_price_target_x64,
                liquidity,
                true,
            )
        };

        if block_timestamp == 0 {
            if result.is_err() {
                return Err(ErrorCode::MaxTokenOverflow.into());
            } else {
                return Ok(Some(result.unwrap()));
            }
        }
        if result.is_ok() {
            return Ok(Some(result.unwrap()));
        } else {
            if result.err().unwrap() == crate::error::ErrorCode::MaxTokenOverflow.into() {
                return Ok(None);
            } else {
                return Err(ErrorCode::SqrtPriceLimitOverflow.into());
            }
        }
    } else {
        let result = if zero_for_one {
            liquidity_math::get_delta_amount_1_unsigned(
                sqrt_price_target_x64,
                sqrt_price_current_x64,
                liquidity,
                false,
            )
        } else {
            liquidity_math::get_delta_amount_0_unsigned(
                sqrt_price_current_x64,
                sqrt_price_target_x64,
                liquidity,
                false,
            )
        };
        if result.is_ok() || block_timestamp == 0 {
            return Ok(Some(result.unwrap()));
        } else {
            if result.err().unwrap() == crate::error::ErrorCode::MaxTokenOverflow.into() {
                return Ok(None);
            } else {
                return Err(ErrorCode::SqrtPriceLimitOverflow.into());
            }
        }
    }
}

use std::collections::HashMap;

/// Enum to hold either a fixed or dynamic tick array
#[derive(Clone)]
pub enum TickArrayData {
    Fixed(TickArrayState),
    Dynamic {
        header: DynTickArrayState,
        ticks: Vec<TickState>,
    },
}

impl TickArrayData {
    /// Parse tick array data from raw bytes
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 8 {
            return None;
        }

        if &data[0..8] == DynTickArrayState::DISCRIMINATOR {
            // Dynamic tick array
            if data.len() < DynTickArrayState::HEADER_LEN {
                return None;
            }
            let header_bytes = &data[8..(DynTickArrayState::HEADER_LEN)];
            let header: DynTickArrayState = *bytemuck::from_bytes(header_bytes);
            let ticks_bytes = &data[DynTickArrayState::HEADER_LEN..];
            let ticks: Vec<TickState> = bytemuck::try_cast_slice(ticks_bytes).ok()?.to_vec();
            Some(TickArrayData::Dynamic { header, ticks })
        } else if &data[0..8] == TickArrayState::DISCRIMINATOR {
            // Fixed tick array
            let tick_array = TickArrayState::try_deserialize(&mut data.to_vec().as_slice()).ok()?;
            Some(TickArrayData::Fixed(tick_array))
        } else {
            None
        }
    }

    /// Get the start tick index
    pub fn start_tick_index(&self) -> i32 {
        match self {
            TickArrayData::Fixed(ta) => ta.start_tick_index,
            TickArrayData::Dynamic { header, .. } => header.start_tick_index,
        }
    }

    /// Get liquidity_net for a given tick
    pub fn get_tick_liquidity_net(&self, tick_index: i32, tick_spacing: u16) -> Option<i128> {
        match self {
            TickArrayData::Fixed(ta) => {
                let offset = ta.get_tick_offset_in_array(tick_index, tick_spacing).ok()?;
                Some(ta.ticks[offset].liquidity_net)
            }
            TickArrayData::Dynamic { header, ticks } => {
                let i = header
                    .get_tick_index_in_array(tick_index, tick_spacing)
                    .ok()?;
                Some(ticks[i as usize].liquidity_net)
            }
        }
    }

    /// Find next initialized tick in the given direction within this array
    pub fn next_initialized_tick(
        &self,
        current_tick: i32,
        tick_spacing: u16,
        zero_for_one: bool,
        allow_first: bool,
    ) -> Option<i32> {
        const TICK_ARRAY_SIZE: i32 = 60;

        match self {
            TickArrayData::Fixed(ta) => {
                let mut ta_mut = ta.clone();
                if !allow_first {
                    if let Ok(Some(ts)) =
                        ta_mut.next_initialized_tick(current_tick, tick_spacing, zero_for_one)
                    {
                        return Some(ts.tick);
                    }
                } else {
                    if let Ok(ts) = ta_mut.first_initialized_tick(zero_for_one) {
                        return Some(ts.tick);
                    }
                }
                None
            }
            TickArrayData::Dynamic { header, .. } => {
                let start = header.start_tick_index;
                let mut found_pos: Option<usize> = None;

                if !allow_first
                    && TickUtils::get_array_start_index(current_tick, tick_spacing) == start
                {
                    let mut offset_in_array =
                        ((current_tick - start) / (tick_spacing as i32)) as i32;
                    if zero_for_one {
                        while offset_in_array >= 0 {
                            if header.tick_offset_index[offset_in_array as usize] > 0 {
                                found_pos = Some(offset_in_array as usize);
                                break;
                            }
                            offset_in_array -= 1;
                        }
                    } else {
                        offset_in_array += 1;
                        while offset_in_array < TICK_ARRAY_SIZE {
                            if header.tick_offset_index[offset_in_array as usize] > 0 {
                                found_pos = Some(offset_in_array as usize);
                                break;
                            }
                            offset_in_array += 1;
                        }
                    }
                }

                if found_pos.is_none() && allow_first {
                    if zero_for_one {
                        for i in (0..TICK_ARRAY_SIZE as usize).rev() {
                            if header.tick_offset_index[i] > 0 {
                                found_pos = Some(i);
                                break;
                            }
                        }
                    } else {
                        for i in 0..TICK_ARRAY_SIZE as usize {
                            if header.tick_offset_index[i] > 0 {
                                found_pos = Some(i);
                                break;
                            }
                        }
                    }
                }

                found_pos.map(|off| start + (off as i32) * (tick_spacing as i32))
            }
        }
    }
}

/// Result of a swap computation
#[derive(Debug, Clone)]
pub struct SwapQuoteResult {
    pub amount_in: u64,
    pub amount_out: u64,
    pub fee_amount: u64,
    pub fee_rate: u32,
    pub price_impact_pct: f64,
}

/// Get the tick array PDA address for a given start index
pub fn get_tick_array_address(pool_key: Pubkey, start_index: i32, program_id: Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[
            TICK_ARRAY_SEED.as_bytes(),
            pool_key.as_ref(),
            &start_index.to_be_bytes(),
        ],
        &program_id,
    )
    .0
}

/// Get the bitmap extension PDA address
pub fn get_bitmap_extension_address(pool_key: Pubkey, program_id: Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[POOL_TICK_ARRAY_BITMAP_SEED.as_bytes(), pool_key.as_ref()],
        &program_id,
    )
    .0
}

/// Get all tick array addresses needed for swap simulation using bitmap navigation
pub fn get_all_tick_array_addresses(
    pool_state: &PoolState,
    pool_key: Pubkey,
    bitmap_extension: &Option<TickArrayBitmapExtension>,
    program_id: Pubkey,
) -> Vec<Pubkey> {
    use std::collections::BTreeSet;

    let mut start_indexes: BTreeSet<i32> = BTreeSet::new();

    // Collect tick arrays in both directions using bitmap
    for zero_for_one in [true, false] {
        if let Ok((_, mut start)) =
            pool_state.get_first_initialized_tick_array(bitmap_extension, zero_for_one)
        {
            start_indexes.insert(start);
            for _ in 1..10 {
                match pool_state.next_initialized_tick_array_start_index(
                    bitmap_extension,
                    start,
                    zero_for_one,
                ) {
                    Ok(Some(next)) => {
                        start_indexes.insert(next);
                        start = next;
                    }
                    _ => break,
                }
            }
        }
    }

    // Fallback to naive neighbors if nothing collected
    if start_indexes.is_empty() {
        let tick_spacing = pool_state.tick_spacing as u16;
        let current_tick = pool_state.tick_current;
        let current_start_index = TickUtils::get_array_start_index(current_tick, tick_spacing);
        start_indexes.insert(current_start_index);

        const TICK_ARRAY_SIZE: i32 = 60;
        for i in 1..=12 {
            let offset = (TICK_ARRAY_SIZE * i as i32) * i32::from(tick_spacing);
            start_indexes.insert(current_start_index.saturating_sub(offset));
            start_indexes.insert(current_start_index.saturating_add(offset));
        }
    }

    start_indexes
        .into_iter()
        .map(|s| get_tick_array_address(pool_key, s, program_id))
        .collect()
}

/// Get tick arrays needed for a specific swap direction
pub fn get_swap_tick_arrays(
    pool_state: &PoolState,
    pool_key: Pubkey,
    bitmap_extension: &Option<TickArrayBitmapExtension>,
    zero_for_one: bool,
    program_id: Pubkey,
) -> Vec<Pubkey> {
    let mut addrs: Vec<Pubkey> = Vec::new();

    // Bitmap-guided discovery from the first initialized tick array
    if let Ok((_, first_start)) =
        pool_state.get_first_initialized_tick_array(bitmap_extension, zero_for_one)
    {
        addrs.push(get_tick_array_address(pool_key, first_start, program_id));
        let mut cur = first_start;
        for _ in 1..=10 {
            match pool_state.next_initialized_tick_array_start_index(
                bitmap_extension,
                cur,
                zero_for_one,
            ) {
                Ok(Some(next)) => {
                    addrs.push(get_tick_array_address(pool_key, next, program_id));
                    cur = next;
                }
                _ => break,
            }
        }
        return addrs;
    }

    // Fallback: adjacent offsets from current array
    const TICK_ARRAY_SIZE: i32 = 60;
    let tick_spacing = pool_state.tick_spacing as u16;
    let current_tick = pool_state.tick_current;
    let current_start_index = TickUtils::get_array_start_index(current_tick, tick_spacing);
    addrs.push(get_tick_array_address(
        pool_key,
        current_start_index,
        program_id,
    ));

    for i in 1..=10 {
        let offset = (TICK_ARRAY_SIZE * i as i32) * i32::from(tick_spacing);
        let s = if zero_for_one {
            current_start_index.saturating_sub(offset)
        } else {
            current_start_index.saturating_add(offset)
        };
        addrs.push(get_tick_array_address(pool_key, s, program_id));
    }

    addrs
}

/// Check if decay fee is enabled
pub fn is_decay_fee_enabled(pool_state: &PoolState) -> bool {
    pool_state.decay_fee_flag & (1 << 0) != 0
}

/// Check if decay fee is enabled for selling mint0
pub fn is_decay_fee_on_sell_mint0(pool_state: &PoolState) -> bool {
    pool_state.decay_fee_flag & (1 << 1) != 0
}

/// Check if decay fee is enabled for selling mint1
pub fn is_decay_fee_on_sell_mint1(pool_state: &PoolState) -> bool {
    pool_state.decay_fee_flag & (1 << 2) != 0
}

/// Calculate decay fee rate based on current timestamp
/// Returns fee rate in hundredths of a bip (10^-6)
pub fn get_decay_fee_rate(pool_state: &PoolState, current_timestamp: u64) -> u32 {
    if !is_decay_fee_enabled(pool_state) {
        return 0u32;
    }

    // Not open yet
    if current_timestamp < pool_state.open_time {
        return 0u32;
    }

    // Check for zero interval to avoid division by zero
    if pool_state.decay_fee_decrease_interval == 0 {
        return 0u32;
    }

    let interval_count =
        (current_timestamp - pool_state.open_time) / pool_state.decay_fee_decrease_interval as u64;
    let decay_fee_decrease_rate = pool_state.decay_fee_decrease_rate as u64 * 10_000;

    // 10^6 (FEE_RATE_DENOMINATOR_VALUE)
    let hundredths_of_a_bip = 1_000_000u64;
    let mut rate = hundredths_of_a_bip;

    // Fast power calculation: (1 - x)^c
    {
        let mut exp = interval_count;
        let mut base = hundredths_of_a_bip.saturating_sub(decay_fee_decrease_rate);

        while exp > 0 {
            if exp % 2 == 1 {
                rate = rate.mul_div_ceil(base, hundredths_of_a_bip).unwrap();
            }
            base = base.mul_div_ceil(base, hundredths_of_a_bip).unwrap();
            exp /= 2;
        }
    }

    // Convert from percentage to hundredths of a bip
    rate = rate
        .mul_div_ceil(pool_state.decay_fee_init_fee_rate as u64, 100u64)
        .unwrap();

    rate as u32
}

/// Get effective fee rate considering both base fee and decay fee
pub fn get_effective_fee_rate(
    pool_state: &PoolState,
    amm_config: &AmmConfig,
    zero_for_one: bool,
    current_timestamp: u64,
) -> u32 {
    let mut fee_rate = amm_config.trade_fee_rate;

    if is_decay_fee_enabled(pool_state) {
        let mut decay_fee_rate = 0u32;

        if zero_for_one && is_decay_fee_on_sell_mint0(pool_state) {
            decay_fee_rate = get_decay_fee_rate(pool_state, current_timestamp);
        } else if !zero_for_one && is_decay_fee_on_sell_mint1(pool_state) {
            decay_fee_rate = get_decay_fee_rate(pool_state, current_timestamp);
        }

        // Use decay fee if it's higher than the base fee
        if decay_fee_rate > fee_rate {
            fee_rate = decay_fee_rate;
        }
    }

    fee_rate
}

/// Find the next initialized tick in the given direction
pub fn find_next_initialized_tick(
    current_tick: i32,
    zero_for_one: bool,
    tick_spacing: u16,
    tick_arrays: &HashMap<Pubkey, TickArrayData>,
    tick_array_addrs: &[Pubkey],
) -> Result<i32> {
    if tick_array_addrs.is_empty() {
        // Fallback to arithmetic next grid
        let step = i32::from(tick_spacing);
        return Ok(if zero_for_one {
            ((current_tick / step) - 1) * step
        } else {
            ((current_tick / step) + 1) * step
        });
    }

    // Find current array
    let current_start = TickUtils::get_array_start_index(current_tick, tick_spacing);
    let mut idx = 0usize;
    let mut matched_current = false;

    for (i, addr) in tick_array_addrs.iter().enumerate() {
        if let Some(tick_array) = tick_arrays.get(addr) {
            if tick_array.start_tick_index() == current_start {
                idx = i;
                matched_current = true;
                break;
            }
        }
    }

    // Helper to search within an array
    let search_in_array = |addr: &Pubkey, cur_tick: i32, allow_first: bool| -> Option<i32> {
        let tick_array = tick_arrays.get(addr)?;
        tick_array.next_initialized_tick(cur_tick, tick_spacing, zero_for_one, allow_first)
    };

    // Search logic
    if matched_current {
        if let Some(t) = search_in_array(&tick_array_addrs[idx], current_tick, false) {
            return Ok(t);
        }
        let iter: Box<dyn Iterator<Item = &Pubkey>> = if zero_for_one {
            Box::new(tick_array_addrs[..idx].iter().rev())
        } else {
            Box::new(tick_array_addrs[idx + 1..].iter())
        };
        for addr in iter {
            if let Some(t) = search_in_array(addr, current_tick, true) {
                return Ok(t);
            }
        }
    } else {
        let iter: Box<dyn Iterator<Item = &Pubkey>> = if zero_for_one {
            Box::new(tick_array_addrs.iter().rev())
        } else {
            Box::new(tick_array_addrs.iter())
        };
        for addr in iter {
            if let Some(t) = search_in_array(addr, current_tick, true) {
                return Ok(t);
            }
        }
    }

    // Fallback
    let step = i32::from(tick_spacing);
    Ok(if zero_for_one {
        ((current_tick / step) - 1) * step
    } else {
        ((current_tick / step) + 1) * step
    })
}

/// Compute a swap quote off-chain
///
/// # Arguments
/// * `pool_state` - The pool state
/// * `amm_config` - The AMM configuration
/// * `zero_for_one` - Swap direction (true = token0 -> token1)
/// * `amount_specified` - Input or output amount depending on is_base_input
/// * `is_base_input` - true for exact input, false for exact output
/// * `sqrt_price_limit_x64` - Optional price limit
/// * `current_timestamp` - Current unix timestamp for decay fee calculation
/// * `tick_arrays` - Map of tick array addresses to their TickArrayData
pub fn compute_swap_quote(
    pool_state: &PoolState,
    amm_config: &AmmConfig,
    zero_for_one: bool,
    amount_specified: u64,
    is_base_input: bool,
    sqrt_price_limit_x64: Option<u128>,
    current_timestamp: u64,
    pool_key: Pubkey,
    program_id: Pubkey,
    tick_arrays: &HashMap<Pubkey, TickArrayData>,
) -> Result<SwapQuoteResult> {
    use crate::libraries::{MAX_SQRT_PRICE_X64, MIN_SQRT_PRICE_X64};

    let sqrt_price_limit = sqrt_price_limit_x64.unwrap_or_else(|| {
        if zero_for_one {
            MIN_SQRT_PRICE_X64 + 1
        } else {
            MAX_SQRT_PRICE_X64 - 1
        }
    });

    // Initialize swap state
    let mut state = SwapState {
        amount_specified_remaining: amount_specified,
        amount_calculated: 0,
        sqrt_price_x64: pool_state.sqrt_price_x64,
        tick: pool_state.tick_current,
        fee_growth_global_x64: 0,
        protocol_fee: 0,
        fund_fee: 0,
        liquidity: pool_state.liquidity,
        fee_amount: 0,
    };

    // Get effective fee rate
    let fee_rate = get_effective_fee_rate(pool_state, amm_config, zero_for_one, current_timestamp);

    // Get tick arrays for this direction
    let tick_array_addrs: Vec<Pubkey> = tick_arrays.keys().cloned().collect();

    // Simulate swap
    const MAX_TICK_ARRAY_CROSSINGS: usize = 10;
    let mut tick_crossings = 0;
    let initial_price = pool_state.sqrt_price_x64;

    while state.amount_specified_remaining != 0
        && state.sqrt_price_x64 != sqrt_price_limit
        && tick_crossings < MAX_TICK_ARRAY_CROSSINGS
    {
        // Find next initialized tick
        let next_tick = find_next_initialized_tick(
            state.tick,
            zero_for_one,
            pool_state.tick_spacing as u16,
            tick_arrays,
            &tick_array_addrs,
        )?;

        let sqrt_price_next = tick_math::get_sqrt_price_at_tick(next_tick)
            .map_err(|_| error!(ErrorCode::SqrtPriceX64))?;

        let target_price = if (zero_for_one && sqrt_price_next < sqrt_price_limit)
            || (!zero_for_one && sqrt_price_next > sqrt_price_limit)
        {
            sqrt_price_limit
        } else {
            sqrt_price_next
        };

        // Compute swap step
        let step = swap_math::compute_swap_step(
            state.sqrt_price_x64,
            target_price,
            state.liquidity,
            state.amount_specified_remaining,
            fee_rate,
            is_base_input,
            zero_for_one,
            current_timestamp as u32,
        )?;

        // Update state
        state.sqrt_price_x64 = step.sqrt_price_next_x64;
        state.fee_amount += step.fee_amount;

        if is_base_input {
            state.amount_specified_remaining = state
                .amount_specified_remaining
                .saturating_sub(step.amount_in + step.fee_amount);
            state.amount_calculated = state.amount_calculated.saturating_add(step.amount_out);
        } else {
            state.amount_specified_remaining = state
                .amount_specified_remaining
                .saturating_sub(step.amount_out);
            state.amount_calculated = state
                .amount_calculated
                .saturating_add(step.amount_in + step.fee_amount);
        }

        // Update tick/liquidity if crossed
        if state.sqrt_price_x64 == sqrt_price_next {
            let tick_spacing = pool_state.tick_spacing as u16;
            let start = TickUtils::get_array_start_index(next_tick, tick_spacing);
            let addr = get_tick_array_address(pool_key, start, program_id);

            if let Some(tick_array) = tick_arrays.get(&addr) {
                if let Some(mut liq_net) =
                    tick_array.get_tick_liquidity_net(next_tick, tick_spacing)
                {
                    if zero_for_one {
                        liq_net = -liq_net;
                    }
                    state.liquidity = liquidity_math::add_delta(state.liquidity, liq_net)
                        .map_err(|_| error!(ErrorCode::LiquidityAddValueErr))?;
                }
            }

            state.tick = if zero_for_one {
                next_tick - 1
            } else {
                next_tick
            };
            tick_crossings += 1;
        } else {
            state.tick = tick_math::get_tick_at_sqrt_price(state.sqrt_price_x64)
                .map_err(|_| error!(ErrorCode::SqrtPriceX64))?;
        }
    }

    // Calculate price impact
    let price_impact_pct = if initial_price > 0 {
        let price_change = if state.sqrt_price_x64 > initial_price {
            state.sqrt_price_x64 - initial_price
        } else {
            initial_price - state.sqrt_price_x64
        };
        (price_change as f64 / initial_price as f64) * 100.0
    } else {
        0.0
    };

    Ok(SwapQuoteResult {
        amount_in: if is_base_input {
            amount_specified - state.amount_specified_remaining
        } else {
            state.amount_calculated
        },
        amount_out: if is_base_input {
            state.amount_calculated
        } else {
            amount_specified - state.amount_specified_remaining
        },
        fee_amount: state.fee_amount,
        fee_rate,
        price_impact_pct,
    })
}

#[cfg(test)]
mod swap_math_test {
    use crate::libraries::tick_math;

    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn compute_swap_step_test(
            sqrt_price_current_x64 in tick_math::MIN_SQRT_PRICE_X64..tick_math::MAX_SQRT_PRICE_X64,
            sqrt_price_target_x64 in tick_math::MIN_SQRT_PRICE_X64..tick_math::MAX_SQRT_PRICE_X64,
            liquidity in 1..u32::MAX as u128,
            amount_remaining in 1..u64::MAX,
            fee_rate in 1..FEE_RATE_DENOMINATOR_VALUE/2,
            is_base_input in proptest::bool::ANY,
        ) {
            prop_assume!(sqrt_price_current_x64 != sqrt_price_target_x64);

            let zero_for_one = sqrt_price_current_x64 > sqrt_price_target_x64;
            let swap_step = compute_swap_step(
                sqrt_price_current_x64,
                sqrt_price_target_x64,
                liquidity,
                amount_remaining,
                fee_rate,
                is_base_input,
                zero_for_one,
                1,
            ).unwrap();

            let amount_in = swap_step.amount_in;
            let amount_out = swap_step.amount_out;
            let sqrt_price_next_x64 = swap_step.sqrt_price_next_x64;
            let fee_amount = swap_step.fee_amount;

            let amount_used = if is_base_input {
                amount_in + fee_amount
            } else {
                amount_out
            };

            if sqrt_price_next_x64 != sqrt_price_target_x64 {
                assert!(amount_used == amount_remaining);
            } else {
                assert!(amount_used <= amount_remaining);
            }
            let price_lower = sqrt_price_current_x64.min(sqrt_price_target_x64);
            let price_upper = sqrt_price_current_x64.max(sqrt_price_target_x64);
            assert!(sqrt_price_next_x64 >= price_lower);
            assert!(sqrt_price_next_x64 <= price_upper);
        }
    }
}
