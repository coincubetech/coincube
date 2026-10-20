use super::{utils::LOOK_AHEAD_LIMIT, SqliteConn};
use crate::database::{ChangeReservation, ReservationError};
use coincube_core::{
    chain::ChainId, descriptors::CoincubeDescriptor, wallet_descriptor::ImportedWalletDescriptor,
};
use miniscript::bitcoin::{self, bip32::ChildNumber, secp256k1};
use rusqlite::TransactionBehavior;

impl SqliteConn {
    pub(crate) fn reserve_imported_receive(
        &mut self,
        chain: ChainId,
        descriptor: &ImportedWalletDescriptor,
    ) -> Result<(bitcoin::Address, ChildNumber), ReservationError> {
        self.allocate_imported_branch(chain, descriptor, false, None)?
            .ok_or(ReservationError::Storage)
    }

    pub(crate) fn commit_imported_change_if_next(
        &mut self,
        chain: ChainId,
        descriptor: &ImportedWalletDescriptor,
        index: ChildNumber,
    ) -> Result<bool, ReservationError> {
        if index.is_hardened() {
            return Err(ReservationError::Exhausted);
        }
        Ok(self
            .allocate_imported_branch(chain, descriptor, true, Some(index))?
            .is_some())
    }

    fn allocate_imported_branch(
        &mut self,
        chain: ChainId,
        descriptor: &ImportedWalletDescriptor,
        change_branch: bool,
        expected: Option<ChildNumber>,
    ) -> Result<Option<(bitcoin::Address, ChildNumber)>, ReservationError> {
        use ReservationError::{Exhausted, IdentityMismatch, Storage};
        if chain != descriptor.chain() || chain.is_blake2b() {
            return Err(IdentityMismatch);
        }
        self.conn
            .pragma_update(None, "synchronous", "FULL")
            .map_err(|_| Storage)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| Storage)?;
        let tip: (String, String) = tx
            .query_row("SELECT chain, network FROM tip", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .map_err(|_| IdentityMismatch)?;
        if tip
            != (
                chain.dir_name().to_owned(),
                chain.bitcoin_network().to_string(),
            )
        {
            return Err(IdentityMismatch);
        }
        let rows: Vec<(i64, String, String, String, u32, u32, u32)> = super::utils::db_tx_query(
            &tx,
            "SELECT id, wallet_kind, import_chain, imported_receive_descriptor,
                    deposit_derivation_index, change_derivation_index, import_initial_lookahead
             FROM wallets",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .map_err(|_| IdentityMismatch)?;
        let [(id, kind, import_chain, receive, receive_index, change_index, lookahead)] =
            rows.as_slice()
        else {
            return Err(IdentityMismatch);
        };
        let stored_change: String = tx
            .query_row(
                "SELECT imported_change_descriptor FROM wallets WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .map_err(|_| IdentityMismatch)?;
        if *id != 1
            || kind != "imported"
            || import_chain != chain.dir_name()
            || receive != &descriptor.receive_descriptor().to_string()
            || stored_change != descriptor.change_descriptor().to_string()
            || !(1..=10_000).contains(lookahead)
        {
            return Err(IdentityMismatch);
        }
        let current = if change_branch {
            *change_index
        } else {
            *receive_index
        };
        let next = current.checked_add(1).ok_or(Exhausted)?;
        let child = ChildNumber::from_normal_idx(next).map_err(|_| Exhausted)?;
        if expected.is_some_and(|candidate| candidate != child) {
            return Ok(None);
        }
        let highest = (*receive_index).max(*change_index);
        let mapped_index = next
            .max(highest)
            .checked_add(*lookahead - 1)
            .ok_or(Exhausted)?;
        ChildNumber::from_normal_idx(mapped_index).map_err(|_| Exhausted)?;
        let branch = if change_branch {
            descriptor.change_descriptor()
        } else {
            descriptor.receive_descriptor()
        };
        let address = branch
            .at_derivation_index(next)
            .map_err(|_| Exhausted)?
            .address(chain.bitcoin_network())
            .map_err(|_| Storage)?;
        tx.execute(
            if change_branch {
                "UPDATE wallets SET change_derivation_index = ?1 WHERE id = 1"
            } else {
                "UPDATE wallets SET deposit_derivation_index = ?1 WHERE id = 1"
            },
            [next],
        )
        .map_err(|_| Storage)?;
        if next > highest {
            let mapped_receive = descriptor
                .receive_descriptor()
                .at_derivation_index(mapped_index)
                .map_err(|_| Exhausted)?
                .address(chain.bitcoin_network())
                .map_err(|_| Storage)?;
            let mapped_change = descriptor
                .change_descriptor()
                .at_derivation_index(mapped_index)
                .map_err(|_| Exhausted)?
                .address(chain.bitcoin_network())
                .map_err(|_| Storage)?;
            tx.execute(
                "INSERT INTO addresses (receive_address, change_address, derivation_index) VALUES (?1, ?2, ?3)",
                rusqlite::params![mapped_receive.to_string(), mapped_change.to_string(), mapped_index],
            ).map_err(|_| Storage)?;
        }
        tx.commit().map_err(|_| Storage)?;
        Ok(Some((address, child)))
    }

    pub(crate) fn reserve_change(
        &mut self,
        chain: ChainId,
        descriptor: &CoincubeDescriptor,
        secp: &secp256k1::Secp256k1<secp256k1::VerifyOnly>,
    ) -> Result<ChangeReservation, ReservationError> {
        self.allocate_change(chain, descriptor, secp, None)?
            .ok_or(ReservationError::Storage)
    }

    pub(crate) fn commit_change_if_next(
        &mut self,
        chain: ChainId,
        descriptor: &CoincubeDescriptor,
        secp: &secp256k1::Secp256k1<secp256k1::VerifyOnly>,
        index: ChildNumber,
    ) -> Result<bool, ReservationError> {
        if index.is_hardened() {
            return Err(ReservationError::Exhausted);
        }
        Ok(self
            .allocate_change(chain, descriptor, secp, Some(index))?
            .is_some())
    }

    fn allocate_change(
        &mut self,
        chain: ChainId,
        descriptor: &CoincubeDescriptor,
        secp: &secp256k1::Secp256k1<secp256k1::VerifyOnly>,
        expected: Option<ChildNumber>,
    ) -> Result<Option<ChangeReservation>, ReservationError> {
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
        if expected.is_some_and(|expected| expected != index) {
            return Ok(None);
        }
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
        Ok(Some(ChangeReservation {
            chain,
            descriptor: descriptor.clone(),
            index,
        }))
    }
}
