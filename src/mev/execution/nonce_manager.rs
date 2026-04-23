use crate::mev::state::event_store::{EventStore, NonceReserved, StateEvent};
use crate::mev::state::snapshot::{PendingNonceSnapshot, WalletNonceSnapshot};
use ethers::types::{Address, H256, U256};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

#[derive(Debug, Clone, Copy)]
pub struct NonceReservation {
    pub address: Address,
    pub nonce: U256,
    pub generation: u64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct NonceState {
    next_nonce: U256,
    generation: u64,
}

#[derive(Debug)]
pub struct NonceManager {
    states: HashMap<Address, NonceState>,
    pending: HashMap<(Address, U256), H256>,
    order: VecDeque<(Address, U256)>,
    capacity: usize,
    event_store: Option<Arc<EventStore>>,
}

impl NonceManager {
    pub fn new(capacity: usize) -> Self {
        Self {
            states: HashMap::with_capacity(capacity.min(1024)),
            pending: HashMap::with_capacity(capacity.min(1024)),
            order: VecDeque::with_capacity(capacity),
            capacity,
            event_store: None,
        }
    }

    pub fn with_event_store(mut self, event_store: Arc<EventStore>) -> Self {
        self.event_store = Some(event_store);
        self
    }

    pub fn set_event_store(&mut self, event_store: Arc<EventStore>) {
        self.event_store = Some(event_store);
    }

    pub fn reserve(&mut self, address: Address, chain_pending_nonce: U256) -> NonceReservation {
        let state = self.states.entry(address).or_insert(NonceState {
            next_nonce: chain_pending_nonce,
            generation: 0,
        });
        if chain_pending_nonce > state.next_nonce {
            state.next_nonce = chain_pending_nonce;
            state.generation = state.generation.saturating_add(1);
        }
        let nonce = state.next_nonce;
        state.next_nonce = state.next_nonce.saturating_add(U256::one());
        state.generation = state.generation.saturating_add(1);
        let reservation = NonceReservation {
            address,
            nonce,
            generation: state.generation,
        };
        self.append_event(StateEvent::NonceReserved(NonceReserved {
            wallet: address,
            nonce,
            generation: reservation.generation,
            chain_pending_nonce,
        }));
        reservation
    }

    pub fn mark_submitted(&mut self, reservation: NonceReservation, tx_hash: H256) {
        if self.order.len() == self.capacity {
            if let Some(old) = self.order.pop_front() {
                self.pending.remove(&old);
            }
        }
        let key = (reservation.address, reservation.nonce);
        self.pending.insert(key, tx_hash);
        self.order.push_back(key);
    }

    pub fn release_unsubmitted(&mut self, reservation: NonceReservation) {
        let state = self
            .states
            .entry(reservation.address)
            .or_insert(NonceState {
                next_nonce: reservation.nonce,
                generation: reservation.generation,
            });
        if state.next_nonce == reservation.nonce.saturating_add(U256::one())
            && state.generation == reservation.generation
        {
            state.next_nonce = reservation.nonce;
        }
    }

    pub fn finalize(&mut self, address: Address, nonce: U256) {
        self.pending.remove(&(address, nonce));
    }

    pub fn restore_snapshot(&mut self, snapshots: &[WalletNonceSnapshot]) {
        self.states.clear();
        self.pending.clear();
        self.order.clear();
        for snapshot in snapshots {
            self.states.insert(
                snapshot.wallet,
                NonceState {
                    next_nonce: snapshot.next_nonce,
                    generation: snapshot.generation,
                },
            );
            for pending in &snapshot.pending {
                self.insert_pending(pending.wallet, pending.nonce, pending.tx_hash);
            }
        }
    }

    pub fn snapshot(&self) -> Vec<WalletNonceSnapshot> {
        let mut out = Vec::with_capacity(self.states.len());
        for (wallet, state) in &self.states {
            let mut pending = self
                .pending
                .iter()
                .filter_map(|((pending_wallet, nonce), tx_hash)| {
                    (*pending_wallet == *wallet).then_some(PendingNonceSnapshot {
                        wallet: *pending_wallet,
                        nonce: *nonce,
                        tx_hash: *tx_hash,
                    })
                })
                .collect::<Vec<_>>();
            pending.sort_by_key(|item| (item.wallet, item.nonce, item.tx_hash));
            out.push(WalletNonceSnapshot {
                wallet: *wallet,
                next_nonce: state.next_nonce,
                generation: state.generation,
                pending,
            });
        }
        out.sort_by_key(|item| (item.wallet, item.next_nonce));
        out
    }

    pub fn apply_nonce_reserved(
        &mut self,
        wallet: Address,
        nonce: U256,
        generation: u64,
        chain_pending_nonce: U256,
    ) {
        let next_nonce = nonce.saturating_add(U256::one()).max(chain_pending_nonce);
        let state = self.states.entry(wallet).or_insert(NonceState {
            next_nonce,
            generation,
        });
        if generation >= state.generation {
            state.next_nonce = state.next_nonce.max(next_nonce);
            state.generation = generation;
        }
    }

    pub fn reconcile_chain_pending_nonce(&mut self, wallet: Address, chain_pending_nonce: U256) {
        let state = self.states.entry(wallet).or_insert(NonceState {
            next_nonce: chain_pending_nonce,
            generation: 0,
        });
        if chain_pending_nonce > state.next_nonce {
            state.next_nonce = chain_pending_nonce;
            state.generation = state.generation.saturating_add(1);
        }
        let stale = self
            .pending
            .keys()
            .filter_map(|(pending_wallet, nonce)| {
                (*pending_wallet == wallet && *nonce < chain_pending_nonce)
                    .then_some((*pending_wallet, *nonce))
            })
            .collect::<Vec<_>>();
        for key in stale {
            self.pending.remove(&key);
        }
    }

    pub fn insert_pending(&mut self, wallet: Address, nonce: U256, tx_hash: H256) {
        if self.order.len() == self.capacity {
            if let Some(old) = self.order.pop_front() {
                self.pending.remove(&old);
            }
        }
        let key = (wallet, nonce);
        self.pending.insert(key, tx_hash);
        self.order.push_back(key);
    }

    fn append_event(&self, event: StateEvent) {
        if let Some(store) = &self.event_store {
            let _ = store.append(event);
        }
    }
}
