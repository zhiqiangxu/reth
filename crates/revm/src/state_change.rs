use reth_consensus_common::calc;
use reth_interfaces::executor::{BlockExecutionError, BlockValidationError};
use reth_primitives::{
    constants::SYSTEM_ADDRESS, revm::env::fill_tx_env_with_beacon_root_contract_call, Address,
    ChainSpec, Header, Withdrawal, B256, U256,
};
use revm::{primitives::ResultAndState, Database, DatabaseCommit, EVM};
use std::{collections::HashMap, fmt::Debug};

/// Collect all balance changes at the end of the block.
///
/// Balance changes might include the block reward, uncle rewards, withdrawals, or irregular
/// state changes (DAO fork).
#[allow(clippy::too_many_arguments)]
#[inline]
pub fn post_block_balance_increments(
    chain_spec: &ChainSpec,
    block_number: u64,
    block_difficulty: U256,
    beneficiary: Address,
    block_timestamp: u64,
    total_difficulty: U256,
    ommers: &[Header],
    withdrawals: Option<&[Withdrawal]>,
) -> HashMap<Address, u128> {
    let mut balance_increments = HashMap::new();

    // Add block rewards if they are enabled.
    if let Some(base_block_reward) =
        calc::base_block_reward(chain_spec, block_number, block_difficulty, total_difficulty)
    {
        // Ommer rewards
        for ommer in ommers {
            *balance_increments.entry(ommer.beneficiary).or_default() +=
                calc::ommer_reward(base_block_reward, block_number, ommer.number);
        }

        // Full block reward
        *balance_increments.entry(beneficiary).or_default() +=
            calc::block_reward(base_block_reward, ommers.len());
    }

    // process withdrawals
    insert_post_block_withdrawals_balance_increments(
        chain_spec,
        block_timestamp,
        withdrawals,
        &mut balance_increments,
    );

    balance_increments
}

/// Applies the pre-block call to the EIP-4788 beacon block root contract, using the given block,
/// [ChainSpec], EVM.
///
/// If cancun is not activated or the block is the genesis block, then this is a no-op, and no
/// state changes are made.
#[inline]
pub fn apply_beacon_root_contract_call<DB: Database + DatabaseCommit>(
    chain_spec: &ChainSpec,
    block_timestamp: u64,
    block_number: u64,
    block_parent_beacon_block_root: Option<B256>,
    evm: &mut EVM<DB>,
) -> Result<(), BlockExecutionError>
where
    <DB as Database>::Error: Debug,
{
    if chain_spec.is_cancun_active_at_timestamp(block_timestamp) {
        // if the block number is zero (genesis block) then the parent beacon block root must
        // be 0x0 and no system transaction may occur as per EIP-4788
        if block_number == 0 {
            if block_parent_beacon_block_root != Some(B256::ZERO) {
                return Err(BlockValidationError::CancunGenesisParentBeaconBlockRootNotZero.into())
            }
        } else {
            let parent_beacon_block_root = block_parent_beacon_block_root.ok_or(
                BlockExecutionError::from(BlockValidationError::MissingParentBeaconBlockRoot),
            )?;

            // get previous env
            let previous_env = evm.env.clone();

            // modify env for pre block call
            fill_tx_env_with_beacon_root_contract_call(&mut evm.env, parent_beacon_block_root);

            let ResultAndState { mut state, .. } = match evm.transact() {
                Ok(res) => res,
                Err(e) => {
                    evm.env = previous_env;
                    return Err(BlockExecutionError::from(BlockValidationError::EVM {
                        hash: Default::default(),
                        message: format!("{e:?}"),
                    }))
                }
            };

            state.remove(&SYSTEM_ADDRESS);
            state.remove(&evm.env.block.coinbase);

            let db = evm.db().expect("db to not be moved");
            db.commit(state);

            // re-set the previous env
            evm.env = previous_env;
        }
    }
    Ok(())
}

/// Returns a map of addresses to their balance increments if the Shanghai hardfork is active at the
/// given timestamp.
///
/// Zero-valued withdrawals are filtered out.
#[inline]
pub fn post_block_withdrawals_balance_increments(
    chain_spec: &ChainSpec,
    block_timestamp: u64,
    withdrawals: &[Withdrawal],
) -> HashMap<Address, u128> {
    let mut balance_increments = HashMap::with_capacity(withdrawals.len());
    insert_post_block_withdrawals_balance_increments(
        chain_spec,
        block_timestamp,
        Some(withdrawals),
        &mut balance_increments,
    );
    balance_increments
}

/// Applies all withdrawal balance increments if shanghai is active at the given timestamp to the
/// given `balance_increments` map.
///
/// Zero-valued withdrawals are filtered out.
#[inline]
pub fn insert_post_block_withdrawals_balance_increments(
    chain_spec: &ChainSpec,
    block_timestamp: u64,
    withdrawals: Option<&[Withdrawal]>,
    balance_increments: &mut HashMap<Address, u128>,
) {
    // Process withdrawals
    if chain_spec.is_shanghai_active_at_timestamp(block_timestamp) {
        if let Some(withdrawals) = withdrawals {
            for withdrawal in withdrawals {
                if withdrawal.amount > 0 {
                    *balance_increments.entry(withdrawal.address).or_default() +=
                        withdrawal.amount_wei();
                }
            }
        }
    }
}
