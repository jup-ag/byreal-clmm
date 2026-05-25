use crate::error::ErrorCode as ClmmErrorCode;
use anchor_lang::{
    prelude::*,
    solana_program::{program::invoke_signed, system_instruction},
    system_program,
};

pub fn create_or_allocate_account<'a>(
    program_id: &Pubkey,
    payer: AccountInfo<'a>,
    system_program: AccountInfo<'a>,
    target_account: AccountInfo<'a>,
    siger_seed: &[&[u8]],
    space: usize,
) -> Result<()> {
    let rent = Rent::get()?;
    let current_lamports = target_account.lamports();

    #[cfg(all(feature = "localnet", feature = "enable-log"))]
    msg!(
        "create_or_allocate_account, target_account: {}, current_lamports: {}, cur_space:{}, target_space: {}",
        target_account.key.to_string(),
        current_lamports,
        target_account.data_len(),
        space
    );

    if current_lamports == 0 {
        let lamports = rent.minimum_balance(space);
        let ix = system_instruction::create_account(
            payer.key,
            target_account.key,
            lamports,
            u64::try_from(space).unwrap(),
            program_id,
        );
        anchor_lang::solana_program::program::invoke_signed(
            &ix,
            &[
                payer.clone(),
                target_account.clone(),
                system_program.clone(),
            ],
            &[siger_seed],
        )?;
    } else {
        let required_lamports = rent.minimum_balance(space).max(1).saturating_sub(current_lamports);
        if required_lamports > 0 {
            let ix = system_instruction::transfer(payer.key, target_account.key, required_lamports);
            anchor_lang::solana_program::program::invoke(
                &ix,
                &[
                    payer.clone(),
                    target_account.clone(),
                    system_program.clone(),
                ],
            )?;
        }
        let allocate_ix =
            system_instruction::allocate(target_account.key, u64::try_from(space).unwrap());
        anchor_lang::solana_program::program::invoke_signed(
            &allocate_ix,
            &[target_account.clone(), system_program.clone()],
            &[siger_seed],
        )?;

        let assign_ix = system_instruction::assign(target_account.key, program_id);
        anchor_lang::solana_program::program::invoke_signed(
            &assign_ix,
            &[target_account.clone(), system_program.clone()],
            &[siger_seed],
        )?;
    }
    Ok(())
}

/// Check if the target account space needs to be reallocated to fit the new_account_space.
/// Returns `true` if the account was reallocated.
pub fn realloc_account_if_needed<'a>(
    target_account: &AccountInfo<'a>,
    new_account_space: usize,
    rent_payer: &AccountInfo<'a>,
    system_program: &AccountInfo<'a>,
) -> Result<bool> {
    // Sanity checks
    require_keys_eq!(*target_account.owner, crate::id(), ClmmErrorCode::IllegalAccountOwner);

    let current_account_size = target_account.data.borrow().len();

    // Check if we need to reallocate space.
    if current_account_size >= new_account_space {
        return Ok(false);
    }

    // Reallocate more space.
    AccountInfo::resize(target_account, new_account_space)?;

    // If more lamports are needed, transfer them to the account.
    let rent_exempt_lamports = Rent::get().unwrap().minimum_balance(new_account_space).max(1);
    let top_up_lamports = rent_exempt_lamports.saturating_sub(target_account.to_account_info().lamports());

    if top_up_lamports > 0 {
        require_keys_eq!(*system_program.key, system_program::ID, ClmmErrorCode::InvalidAccount);

        let ix = system_instruction::transfer(rent_payer.key, target_account.key, top_up_lamports);
        invoke_signed(
            &ix,
            &[
                rent_payer.clone(),
                target_account.clone(),
                system_program.clone(),
            ],
            &[],
        )?;
    }

    Ok(true)
}

#[cfg(not(any(test, feature = "client")))]
pub fn get_recent_epoch() -> Result<u64> {
    Ok(Clock::get()?.epoch)
}

#[cfg(any(test, feature = "client"))]
pub fn get_recent_epoch() -> Result<u64> {
    use std::time::{SystemTime, UNIX_EPOCH};
    Ok(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() / (2 * 24 * 3600))
}
