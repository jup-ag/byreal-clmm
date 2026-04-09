use crate::error::ErrorCode;
use crate::states::*;
use anchor_lang::prelude::*;
#[derive(Accounts)]
pub struct TransferRewardOwner<'info> {
    /// Address to be set as operation account owner.
    #[account(
        address = admin_group.reward_config_manager @ ErrorCode::NotApproved
    )]
    pub authority: Signer<'info>,

    /// amm admin group account to store admin permissions.
    #[account(
        seeds = [
            ADMIN_GROUP_SEED.as_bytes()
        ],
        bump,
    )]
    pub admin_group: Box<Account<'info, AmmAdminGroup>>,

    #[account(mut)]
    pub pool_state: AccountLoader<'info, PoolState>,
}

pub fn transfer_reward_owner<'info>(
    ctx: Context<'info, TransferRewardOwner<'info>>,
    new_owner: Pubkey,
) -> Result<()> {
    let mut pool_state = ctx.accounts.pool_state.load_mut()?;
    for reward_info in &mut pool_state.reward_infos {
        reward_info.authority = new_owner;
    }
    pool_state.owner = new_owner;
    Ok(())
}
