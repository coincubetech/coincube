use super::{utils::LOOK_AHEAD_LIMIT, SqliteConn};
use crate::database::{ChangeReservation, ReservationError};
use coincube_core::{chain::ChainId, descriptors::CoincubeDescriptor};
use miniscript::bitcoin::{bip32::ChildNumber, secp256k1};
use rusqlite::TransactionBehavior;

impl SqliteConn {
    pub(crate) fn reserve_change(
        &mut self,
        chain: ChainId,
        descriptor: &CoincubeDescriptor,
        secp: &secp256k1::Secp256k1<secp256k1::VerifyOnly>,
    ) -> Result<ChangeReservation, ReservationError> {
        use ReservationError::{Exhausted, IdentityMismatch, Storage};
        // Every SQLite connection/process contends on the same writer lock, including
        // set_derivation_index. A connection-creation mutex alone would not suffice.
        // This connection must request durable commit even if an embedding caller
        // previously changed its synchronous setting. The index is never returned
        // before SQLite confirms the commit.
        self.conn
            .pragma_update(None, "synchronous", "FULL")
            .map_err(|_| Storage)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| Storage)?;
        let identities: Vec<(String, String)> = super::utils::db_tx_query(
            &tx,
            "SELECT chain, network FROM tip",
            rusqlite::params![],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|_| Storage)?;
        if identities
            != vec![(
                chain.dir_name().to_owned(),
                chain.bitcoin_network().to_string(),
            )]
        {
            return Err(IdentityMismatch);
        }
        // Do not use the inherited DbWallet decoder: malformed descriptors there panic.
        let wallets: Vec<(i64, String, u32, u32)> = super::utils::db_tx_query(
            &tx, "SELECT id, main_descriptor, deposit_derivation_index, change_derivation_index FROM wallets",
            rusqlite::params![], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        ).map_err(|_| Storage)?;
        let (id, stored_descriptor, receive, change) = match wallets.as_slice() {
            [wallet] => wallet,
            _ => return Err(IdentityMismatch),
        };
        if *id != 1 || *stored_descriptor != descriptor.to_string() {
            return Err(IdentityMismatch);
        }
        let next = change.checked_add(1).ok_or(Exhausted)?;
        let index = ChildNumber::from_normal_idx(next).map_err(|_| Exhausted)?;
        let highest = (*receive).max(*change);
        // Refuse before writing if either branch's existing window, or the new
        // window, would cross into hardened derivation. No wraparound or panic.
        for last in [highest, next] {
            let lookahead = last.checked_add(LOOK_AHEAD_LIMIT - 1).ok_or(Exhausted)?;
            ChildNumber::from_normal_idx(lookahead).map_err(|_| Exhausted)?;
        }
        tx.execute(
            "UPDATE wallets SET change_derivation_index = ?1 WHERE id = 1",
            [next],
        )
        .map_err(|_| Storage)?;
        if next > highest {
            let lookahead = next + LOOK_AHEAD_LIMIT - 1;
            let child = ChildNumber::from_normal_idx(lookahead).map_err(|_| Exhausted)?;
            let receive_address = descriptor
                .receive_descriptor()
                .derive(child, secp)
                .address(chain.bitcoin_network());
            let change_address = descriptor
                .change_descriptor()
                .derive(child, secp)
                .address(chain.bitcoin_network());
            tx.execute("INSERT INTO addresses (receive_address, change_address, derivation_index) VALUES (?1, ?2, ?3)",
                rusqlite::params![receive_address.to_string(), change_address.to_string(), lookahead])
                .map_err(|_| Storage)?;
        }
        tx.commit().map_err(|_| Storage)?;
        Ok(ChangeReservation {
            chain,
            descriptor: descriptor.clone(),
            index,
        })
    }
}
