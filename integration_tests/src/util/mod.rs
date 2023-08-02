use fil_actor_market::State as MarketState;
use fil_actor_power::State as PowerState;
use fil_actor_reward::State as RewardState;
use fil_actors_runtime::{
    runtime::Policy, MessageAccumulator, REWARD_ACTOR_ADDR, STORAGE_MARKET_ACTOR_ADDR,
    STORAGE_POWER_ACTOR_ADDR,
};
use fvm_ipld_bitfield::BitField;
use fvm_ipld_encoding::{CborStore, RawBytes};
use fvm_shared::address::Address;
use fvm_shared::econ::TokenAmount;
use fvm_shared::sector::SectorNumber;
use fvm_shared::METHOD_SEND;

use fil_actor_miner::{
    new_deadline_info_from_offset_and_epoch, Deadline, DeadlineInfo, GetBeneficiaryReturn,
    Method as MinerMethod, MinerInfo, PowerPair, SectorOnChainInfo, State as MinerState,
};
use fil_builtin_actors_state::check::{check_state_invariants, Tree};
use num_traits::Zero;
use regex::Regex;
use vm_api::{
    util::{apply_ok, get_state, pk_addrs_from, DynBlockstore},
    VM,
};

mod workflows;
pub use workflows::*;

use crate::{MinerBalances, NetworkStats, TEST_FAUCET_ADDR};

const ACCOUNT_SEED: u64 = 93837778;

pub fn create_accounts(v: &dyn VM, count: u64, balance: &TokenAmount) -> Vec<Address> {
    create_accounts_seeded(v, count, balance, ACCOUNT_SEED, &TEST_FAUCET_ADDR)
}

pub fn create_accounts_seeded(
    v: &dyn VM,
    count: u64,
    balance: &TokenAmount,
    seed: u64,
    test_faucet_addr: &Address,
) -> Vec<Address> {
    let pk_addrs = pk_addrs_from(seed, count);
    // Send funds from faucet to pk address, creating account actor
    for pk_addr in pk_addrs.clone() {
        apply_ok(v, test_faucet_addr, &pk_addr, balance, METHOD_SEND, None::<RawBytes>);
    }
    // Normalize pk address to return id address of account actor
    pk_addrs.iter().map(|pk_addr| v.resolve_id_address(pk_addr).unwrap()).collect()
}

pub fn check_invariants(vm: &dyn VM, policy: &Policy) -> anyhow::Result<MessageAccumulator> {
    check_state_invariants(
        &vm.actor_manifest(),
        policy,
        Tree::load(&DynBlockstore::wrap(vm.blockstore()), &vm.state_root()).unwrap(),
        &vm.circulating_supply(),
        vm.epoch() - 1,
    )
}

pub fn assert_invariants(v: &dyn VM, policy: &Policy) {
    check_invariants(v, policy).unwrap().assert_empty()
}

pub fn expect_invariants(v: &dyn VM, policy: &Policy, expected_patterns: &[Regex]) {
    check_invariants(v, policy).unwrap().assert_expected(expected_patterns)
}

pub fn miner_balance(v: &dyn VM, m: &Address) -> MinerBalances {
    let st: MinerState = get_state(v, m).unwrap();
    MinerBalances {
        available_balance: st.get_available_balance(&v.balance(m)).unwrap(),
        vesting_balance: st.locked_funds,
        initial_pledge: st.initial_pledge,
        pre_commit_deposit: st.pre_commit_deposits,
    }
}

pub fn miner_info(v: &dyn VM, m: &Address) -> MinerInfo {
    let st: MinerState = get_state(v, m).unwrap();
    DynBlockstore::wrap(v.blockstore()).get_cbor(&st.info).unwrap().unwrap()
}

pub fn miner_dline_info(v: &dyn VM, m: &Address) -> DeadlineInfo {
    let st: MinerState = get_state(v, m).unwrap();
    new_deadline_info_from_offset_and_epoch(&Policy::default(), st.proving_period_start, v.epoch())
}

pub fn sector_deadline(v: &dyn VM, m: &Address, s: SectorNumber) -> (u64, u64) {
    let st: MinerState = get_state(v, m).unwrap();
    st.find_sector(&Policy::default(), &DynBlockstore::wrap(v.blockstore()), s).unwrap()
}

pub fn check_sector_active(v: &dyn VM, m: &Address, s: SectorNumber) -> bool {
    let (d_idx, p_idx) = sector_deadline(v, m, s);
    let st: MinerState = get_state(v, m).unwrap();
    st.check_sector_active(
        &Policy::default(),
        &DynBlockstore::wrap(v.blockstore()),
        d_idx,
        p_idx,
        s,
        true,
    )
    .unwrap()
}

pub fn check_sector_faulty(
    v: &dyn VM,
    m: &Address,
    d_idx: u64,
    p_idx: u64,
    s: SectorNumber,
) -> bool {
    let st: MinerState = get_state(v, m).unwrap();
    let bs = &DynBlockstore::wrap(v.blockstore());
    let deadlines = st.load_deadlines(bs).unwrap();
    let deadline = deadlines.load_deadline(&Policy::default(), bs, d_idx).unwrap();
    let partition = deadline.load_partition(bs, p_idx).unwrap();
    partition.faults.get(s)
}

pub fn deadline_state(v: &dyn VM, m: &Address, d_idx: u64) -> Deadline {
    let st: MinerState = get_state(v, m).unwrap();
    let bs = &DynBlockstore::wrap(v.blockstore());
    let deadlines = st.load_deadlines(bs).unwrap();
    deadlines.load_deadline(&Policy::default(), bs, d_idx).unwrap()
}

pub fn sector_info(v: &dyn VM, m: &Address, s: SectorNumber) -> SectorOnChainInfo {
    let st: MinerState = get_state(v, m).unwrap();
    st.get_sector(&DynBlockstore::wrap(v.blockstore()), s).unwrap().unwrap()
}

pub fn miner_power(v: &dyn VM, m: &Address) -> PowerPair {
    let st: PowerState = get_state(v, &STORAGE_POWER_ACTOR_ADDR).unwrap();
    let claim = st.get_claim(&DynBlockstore::wrap(v.blockstore()), m).unwrap().unwrap();
    PowerPair::new(claim.raw_byte_power, claim.quality_adj_power)
}

pub fn get_beneficiary(v: &dyn VM, from: &Address, m_addr: &Address) -> GetBeneficiaryReturn {
    apply_ok(
        v,
        from,
        m_addr,
        &TokenAmount::zero(),
        MinerMethod::GetBeneficiary as u64,
        None::<RawBytes>,
    )
    .deserialize()
    .unwrap()
}

pub fn make_bitfield(bits: &[u64]) -> BitField {
    BitField::try_from_bits(bits.iter().copied()).unwrap()
}

pub fn bf_all(bf: BitField) -> Vec<u64> {
    bf.bounded_iter(Policy::default().addressed_sectors_max).unwrap().collect()
}

pub mod invariant_failure_patterns {
    use lazy_static::lazy_static;
    use regex::Regex;

    lazy_static! {
        pub static ref REWARD_STATE_EPOCH_MISMATCH: Regex =
            Regex::new("^reward state epoch \\d+ does not match prior_epoch\\+1 \\d+$").unwrap();
    }
}

pub fn get_network_stats(vm: &dyn VM) -> NetworkStats {
    let power_state: PowerState = get_state(vm, &STORAGE_POWER_ACTOR_ADDR).unwrap();
    let reward_state: RewardState = get_state(vm, &REWARD_ACTOR_ADDR).unwrap();
    let market_state: MarketState = get_state(vm, &STORAGE_MARKET_ACTOR_ADDR).unwrap();

    NetworkStats {
        total_raw_byte_power: power_state.total_raw_byte_power,
        total_bytes_committed: power_state.total_bytes_committed,
        total_quality_adj_power: power_state.total_quality_adj_power,
        total_qa_bytes_committed: power_state.total_qa_bytes_committed,
        total_pledge_collateral: power_state.total_pledge_collateral,
        this_epoch_raw_byte_power: power_state.this_epoch_raw_byte_power,
        this_epoch_quality_adj_power: power_state.this_epoch_quality_adj_power,
        this_epoch_pledge_collateral: power_state.this_epoch_pledge_collateral,
        miner_count: power_state.miner_count,
        miner_above_min_power_count: power_state.miner_above_min_power_count,
        this_epoch_reward: reward_state.this_epoch_reward,
        this_epoch_reward_smoothed: reward_state.this_epoch_reward_smoothed,
        this_epoch_baseline_power: reward_state.this_epoch_baseline_power,
        total_storage_power_reward: reward_state.total_storage_power_reward,
        total_client_locked_collateral: market_state.total_client_locked_collateral,
        total_provider_locked_collateral: market_state.total_provider_locked_collateral,
        total_client_storage_fee: market_state.total_client_storage_fee,
    }
}
