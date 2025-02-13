//! Message models.

use crate::cell::*;
use crate::dict::{self, Dict};
use crate::error::*;
use crate::num::*;

use crate::models::account::AccountStatus;
use crate::models::currency::CurrencyCollection;
use crate::models::message::Message;
use crate::models::Lazy;

pub use self::phases::*;

mod phases;

/// Blockchain transaction.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Transaction {
    /// Account on which this transaction was produced.
    pub account: HashBytes,
    /// Logical time when the transaction was created.
    pub lt: u64,
    /// The hash of the previous transaction on the same account.
    pub prev_trans_hash: HashBytes,
    /// The logical time of the previous transaction on the same account.
    pub prev_trans_lt: u64,
    /// Unix timestamp when the transaction was created.
    pub now: u32,
    /// The number of outgoing messages.
    pub out_msg_count: Uint15,
    /// Account status before this transaction.
    pub orig_status: AccountStatus,
    /// Account status after this transaction.
    pub end_status: AccountStatus,
    /// Optional incoming message.
    pub in_msg: Option<Cell>,
    /// Outgoing messages.
    pub out_msgs: Dict<Uint15, Cell>,
    /// Total transaction fees (including extra fwd fees).
    pub total_fees: CurrencyCollection,
    /// Account state hashes.
    pub state_update: Lazy<HashUpdate>,
    /// Detailed transaction info.
    pub info: Lazy<TxInfo>,
}

impl Transaction {
    /// Tries to load the incoming message, if present.
    pub fn load_in_msg(&self) -> Result<Option<Message<'_>>, Error> {
        match &self.in_msg {
            Some(in_msg) => match in_msg.parse::<Message>() {
                Ok(message) => Ok(Some(message)),
                Err(e) => Err(e),
            },
            None => Ok(None),
        }
    }

    /// Tries to load the detailed transaction info from the lazy cell.
    pub fn load_info(&self) -> Result<TxInfo, Error> {
        self.info.load()
    }
}

impl Transaction {
    /// Gets an iterator over the output messages of this transaction, in order by lt.
    /// The iterator element type is `Result<Message<'a>>`.
    ///
    /// If the dictionary or message is invalid, finishes after the first invalid element,
    /// returning an error.
    pub fn iter_out_msgs(&'_ self) -> TxOutMsgIter<'_> {
        TxOutMsgIter {
            inner: self.out_msgs.raw_values(),
        }
    }
}

/// An iterator over the transaction output messages.
///
/// This struct is created by the [`iter_out_msgs`] method on [`Transaction`].
/// See its documentation for more.
///
/// [`iter_out_msgs`]: Transaction::iter_out_msgs
#[derive(Clone)]
pub struct TxOutMsgIter<'a> {
    inner: dict::RawValues<'a>,
}

impl<'a> Iterator for TxOutMsgIter<'a> {
    type Item = Result<Message<'a>, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.inner.next()? {
            Ok(mut value) => {
                let e = match value.load_reference_as_slice() {
                    Ok(mut value) => match Message::<'a>::load_from(&mut value) {
                        Ok(message) => return Some(Ok(message)),
                        Err(e) => e,
                    },
                    Err(e) => e,
                };

                Some(Err(self.inner.finish(e)))
            }
            Err(e) => Some(Err(e)),
        }
    }
}

impl Transaction {
    const TAG: u8 = 0b0111;
}

impl Store for Transaction {
    fn store_into(
        &self,
        builder: &mut CellBuilder,
        finalizer: &mut dyn Finalizer,
    ) -> Result<(), Error> {
        let messages = {
            let mut builder = CellBuilder::new();
            ok!(self.in_msg.store_into(&mut builder, finalizer));
            ok!(self.out_msgs.store_into(&mut builder, finalizer));
            ok!(builder.build_ext(finalizer))
        };

        ok!(builder.store_small_uint(Self::TAG, 4));
        ok!(builder.store_u256(&self.account));
        ok!(builder.store_u64(self.lt));
        ok!(builder.store_u256(&self.prev_trans_hash));
        ok!(builder.store_u64(self.prev_trans_lt));
        ok!(builder.store_u32(self.now));
        ok!(self.out_msg_count.store_into(builder, finalizer));
        ok!(self.orig_status.store_into(builder, finalizer));
        ok!(self.end_status.store_into(builder, finalizer));
        ok!(builder.store_reference(messages));
        ok!(self.total_fees.store_into(builder, finalizer));
        ok!(self.state_update.store_into(builder, finalizer));
        self.info.store_into(builder, finalizer)
    }
}

impl<'a> Load<'a> for Transaction {
    fn load_from(slice: &mut CellSlice<'a>) -> Result<Self, Error> {
        match slice.load_small_uint(4) {
            Ok(Self::TAG) => {}
            Ok(_) => return Err(Error::InvalidTag),
            Err(e) => return Err(e),
        }

        let (in_msg, out_msgs) = {
            let slice = &mut ok!(slice.load_reference_as_slice());
            let in_msg = ok!(Option::<Cell>::load_from(slice));
            let out_msgs = ok!(Dict::load_from(slice));
            (in_msg, out_msgs)
        };

        Ok(Self {
            account: ok!(slice.load_u256()),
            lt: ok!(slice.load_u64()),
            prev_trans_hash: ok!(slice.load_u256()),
            prev_trans_lt: ok!(slice.load_u64()),
            now: ok!(slice.load_u32()),
            out_msg_count: ok!(Uint15::load_from(slice)),
            orig_status: ok!(AccountStatus::load_from(slice)),
            end_status: ok!(AccountStatus::load_from(slice)),
            in_msg,
            out_msgs,
            total_fees: ok!(CurrencyCollection::load_from(slice)),
            state_update: ok!(Lazy::<HashUpdate>::load_from(slice)),
            info: ok!(Lazy::<TxInfo>::load_from(slice)),
        })
    }
}

/// Detailed transaction info.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum TxInfo {
    /// Ordinary transaction info.
    Ordinary(OrdinaryTxInfo),
    /// Tick-tock transaction info.
    TickTock(TickTockTxInfo),
}

impl Store for TxInfo {
    fn store_into(
        &self,
        builder: &mut CellBuilder,
        finalizer: &mut dyn Finalizer,
    ) -> Result<(), Error> {
        match self {
            Self::Ordinary(info) => {
                ok!(builder.store_small_uint(0b0000, 4));
                info.store_into(builder, finalizer)
            }
            Self::TickTock(info) => {
                ok!(builder.store_small_uint(0b001, 3));
                info.store_into(builder, finalizer)
            }
        }
    }
}

impl<'a> Load<'a> for TxInfo {
    fn load_from(slice: &mut CellSlice<'a>) -> Result<Self, Error> {
        let tag_part = ok!(slice.load_small_uint(3));
        Ok(if tag_part == 0b001 {
            match TickTockTxInfo::load_from(slice) {
                Ok(info) => Self::TickTock(info),
                Err(e) => return Err(e),
            }
        } else if tag_part == 0b000 && !ok!(slice.load_bit()) {
            match OrdinaryTxInfo::load_from(slice) {
                Ok(info) => Self::Ordinary(info),
                Err(e) => return Err(e),
            }
        } else {
            return Err(Error::InvalidTag);
        })
    }
}

/// Ordinary transaction info.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct OrdinaryTxInfo {
    /// Whether the credit phase was executed first
    /// (usually set when incoming message has `bounce: false`).
    pub credit_first: bool,
    /// Storage phase info.
    ///
    /// Skipped if the account did not exist prior to execution.
    pub storage_phase: Option<StoragePhase>,
    /// Credit phase info.
    ///
    /// Skipped if the incoming message is external.
    pub credit_phase: Option<CreditPhase>,
    /// Compute phase info.
    pub compute_phase: ComputePhase,
    /// Action phase info.
    ///
    /// Skipped if the transaction was aborted at the compute phase.
    pub action_phase: Option<ActionPhase>,
    /// Whether the transaction was reverted.
    pub aborted: bool,
    /// Bounce phase info.
    ///
    /// Only present if the incoming message had `bounce: true` and
    /// the compute phase failed.
    pub bounce_phase: Option<BouncePhase>,
    /// Whether the account was destroyed during this transaction.
    pub destroyed: bool,
}

impl Store for OrdinaryTxInfo {
    fn store_into(
        &self,
        builder: &mut CellBuilder,
        finalizer: &mut dyn Finalizer,
    ) -> Result<(), Error> {
        let action_phase = match &self.action_phase {
            Some(action_phase) => {
                let mut builder = CellBuilder::new();
                ok!(action_phase.store_into(&mut builder, finalizer));
                Some(ok!(builder.build_ext(finalizer)))
            }
            None => None,
        };

        ok!(builder.store_bit(self.credit_first));
        ok!(self.storage_phase.store_into(builder, finalizer));
        ok!(self.credit_phase.store_into(builder, finalizer));
        ok!(self.compute_phase.store_into(builder, finalizer));
        ok!(action_phase.store_into(builder, finalizer));
        ok!(builder.store_bit(self.aborted));
        ok!(self.bounce_phase.store_into(builder, finalizer));
        builder.store_bit(self.destroyed)
    }
}

impl<'a> Load<'a> for OrdinaryTxInfo {
    fn load_from(slice: &mut CellSlice<'a>) -> Result<Self, Error> {
        Ok(Self {
            credit_first: ok!(slice.load_bit()),
            storage_phase: ok!(Option::<StoragePhase>::load_from(slice)),
            credit_phase: ok!(Option::<CreditPhase>::load_from(slice)),
            compute_phase: ok!(ComputePhase::load_from(slice)),
            action_phase: match ok!(Option::<Cell>::load_from(slice)) {
                Some(cell) => Some(ok!(cell.as_ref().parse::<ActionPhase>())),
                None => None,
            },
            aborted: ok!(slice.load_bit()),
            bounce_phase: ok!(Option::<BouncePhase>::load_from(slice)),
            destroyed: ok!(slice.load_bit()),
        })
    }
}

/// Tick-tock transaction info.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TickTockTxInfo {
    /// Tick-tock transaction execution edge.
    pub kind: TickTock,
    /// Storage phase info.
    pub storage_phase: StoragePhase,
    /// Compute phase info.
    pub compute_phase: ComputePhase,
    /// Action phase info.
    ///
    /// Skipped if the transaction was aborted at the compute phase.
    pub action_phase: Option<ActionPhase>,
    /// Whether the transaction was reverted.
    pub aborted: bool,
    /// Whether the account was destroyed during this transaction.
    pub destroyed: bool,
}

impl Store for TickTockTxInfo {
    fn store_into(
        &self,
        builder: &mut CellBuilder,
        finalizer: &mut dyn Finalizer,
    ) -> Result<(), Error> {
        let action_phase = match &self.action_phase {
            Some(action_phase) => {
                let mut builder = CellBuilder::new();
                ok!(action_phase.store_into(&mut builder, finalizer));
                Some(ok!(builder.build_ext(finalizer)))
            }
            None => None,
        };

        let flags = ((self.aborted as u8) << 1) | (self.destroyed as u8);

        ok!(self.kind.store_into(builder, finalizer));
        ok!(self.storage_phase.store_into(builder, finalizer));
        ok!(self.compute_phase.store_into(builder, finalizer));
        ok!(action_phase.store_into(builder, finalizer));
        builder.store_small_uint(flags, 2)
    }
}

impl<'a> Load<'a> for TickTockTxInfo {
    fn load_from(slice: &mut CellSlice<'a>) -> Result<Self, Error> {
        let kind = ok!(TickTock::load_from(slice));
        let storage_phase = ok!(StoragePhase::load_from(slice));
        let compute_phase = ok!(ComputePhase::load_from(slice));
        let action_phase = match ok!(Option::<Cell>::load_from(slice)) {
            Some(cell) => Some(ok!(cell.as_ref().parse::<ActionPhase>())),
            None => None,
        };
        let flags = ok!(slice.load_small_uint(2));

        Ok(Self {
            kind,
            storage_phase,
            compute_phase,
            action_phase,
            aborted: flags & 0b10 != 0,
            destroyed: flags & 0b01 != 0,
        })
    }
}

/// Tick-tock transaction execution edge.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TickTock {
    /// Start of the block.
    Tick = 0,
    /// End of the block.
    Tock = 1,
}

impl Store for TickTock {
    #[inline]
    fn store_into(&self, builder: &mut CellBuilder, _: &mut dyn Finalizer) -> Result<(), Error> {
        builder.store_bit(*self == Self::Tock)
    }
}

impl<'a> Load<'a> for TickTock {
    #[inline]
    fn load_from(slice: &mut CellSlice<'a>) -> Result<Self, Error> {
        match slice.load_bit() {
            Ok(false) => Ok(Self::Tick),
            Ok(true) => Ok(Self::Tock),
            Err(e) => Err(e),
        }
    }
}

/// Account state hash update.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Store, Load)]
#[tlb(tag = "#72")]
pub struct HashUpdate {
    /// Old account state hash.
    pub old: HashBytes,
    /// New account state hash.
    pub new: HashBytes,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::{Boc, Cell, CellBuilder};

    fn check_tx(boc: &str) -> Cell {
        let boc = Boc::decode_base64(boc).unwrap();
        let tx = boc.parse::<Transaction>().unwrap();
        println!("tx: {tx:#?}");

        let in_msg = tx.load_in_msg().unwrap();
        println!("In message: {in_msg:?}");

        for (i, entry) in tx.out_msgs.iter().enumerate() {
            let (number, cell) = entry.unwrap();
            let message = cell.parse::<Message>().unwrap();
            assert_eq!(number, i as u16);
            println!("Out message: {i}, message: {message:?}");
        }
        assert_eq!(
            tx.out_msg_count.into_inner() as usize,
            tx.out_msgs.raw_values().count()
        );

        let mut out_msg_count = 0;
        for msg in tx.iter_out_msgs() {
            msg.unwrap();
            out_msg_count += 1;
        }
        assert_eq!(out_msg_count, tx.out_msg_count);

        let info = tx.load_info().unwrap();
        println!("info: {info:#?}");

        let serialized = CellBuilder::build_from(tx).unwrap();
        assert_eq!(serialized.as_ref(), boc.as_ref());
        serialized
    }

    #[test]
    fn ordinary_tx_without_outgoing() {
        check_tx("te6ccgECCgEAAiQAA7V2SOift2eyC7fBlt0WiLN/0hA462V/fPMQ8oEsnBB3G7AAAfY9R6LMZN1w7hT1VtMZQ34vff1IakzKvRM4657r3GeupIvoJIpQAAH2PT8NiIY8hJ2wABRl0zgoBQQBAhcEREkBdGUCGGXTNhEDAgBbwAAAAAAAAAAAAAAAAS1FLaRJ5QuM990nhh8UYSKv4bVGu4tw/IIW8MYUE5+OBACeQX3MBfVUAAAAAAAAAABSAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAACCck/lB91WD5Bky9xy1ywXY/bG7iqNzr+1DG27jQVp5OUxkl947E8nAzF+NHA+zqGpqCuZL3eq9YgWEJBLelAikwoBAaAGAbFoAYPlFQowDFvzLx17ZmWrhW1pi0YpTuBN6LYhOh6J98IfABkjon7dnsgu3wZbdFoizf9IQOOtlf3zzEPKBLJwQdxu0BdGUCAGMGa6AAA+x6j0WYjHkJO2wAcBSwAAAAtACVRPdAch0GHCu0sq7u4086DOMvZRilq2LylASpak+6fYCAGjgAvHaUKSILpcQdjjdbO/WOS2BHQw8Rn8vBldFsPGUGfY4AAAAAAAAAAAAAAAAAdlcwAAAAAAAAAAAAAAAAAAAAAgAAAAAAAAAAAAAAAAB19IkAkAIAAAAAAAAAAAAAAAAAA6+kQ=");
    }

    #[test]
    fn ordinary_tx_with_outgoing() {
        check_tx("te6ccgECGgEABPQAA7d9z+fCq1SjdzIW3cWMo/2pYrA4pkV/IS8ngy0EVS/oG5AAAfax/zS4OpRftPiDkS8YMj1KWTiQwQSYK7NlTiRqhW4I9QG+p38AAAH2sEpsUDY8me7AAJaARP9tSAUEAQIbBIjbiSysaa4YgEBlWhMDAgBv3NuB/iZJWlgAAAAAAAQAAAAAAAQBHCsIhRCq5P8FMG8flwwgRNH2WhuPUG/uZDiNwGJxGSFIVP4AnlB8TD0JAAAAAAAAAAADTQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAgnLZb3+dbs+xByMvmHutLzN3GyE8lMcQeTvr3SCQc3ScC5CuyJZa+rsW68PLm0COuucbYY14eIvIDQmENZPKyY2kAgHgFwYCAdsPBwIBIAoIAQEgCQCxSAG5/PhVapRu5kLbuLGUf7UsVgcUyK/kJeTwZaCKpf0DcwAjvLvDBBHD2FKY9lF2tlW2KfekP/IutbAthrLnsm7NJJB9QV1IBhRYYAAAPtY/5pcOx5M92EABASALAbFoAbn8+FVqlG7mQtu4sZR/tSxWBxTIr+Ql5PBloIql/QNzAB5aJdM28ct7yt8uYEgbborLmhcxBQFKEZnnpDch5N3r0BdGUCAGMGa6AAA+1j/mlwzHkz3YwAwBSwAAAAxABeO0oUkQXS4g7HG62d+sclsCOhh4jP5eDK6LYeMoM+x4DQGjgBHlfKxhXeft5K5sKDIIOm/wpEkrIrCamVABvEzcpCOfYAAAAAAAAAAAAAAAAF4N1iAAAAAAAAAAAAAAAAAAAAAgAAAAAAAAAAAAArefTpzAEA4AIAAAAAAAAAAAAAAAAAAAAAACASAUEAEBIBEBs2gBufz4VWqUbuZC27ixlH+1LFYHFMiv5CXk8GWgiqX9A3MANSuO7zsQ3zhNZmTONopY8im0iF8AkI6GP8iVZHBTWkAUBD3i/+AGNFPwAAA+1j/mlwrHkz3YwBIB0AAAASwAAABqAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAC8G6xAAAAAAAAAAAAAAAAAAAAAQD6URtVYTw4eBTWuXZchYWswosdVGZn3Ylvat6GUWKKEwCHAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAEHh/KJtPYiKAHHeP+Pt9pjQLZJg34+cx3nVl6KuuadUFZSDUopwW3S1hIkgBASAVAbFoAbn8+FVqlG7mQtu4sZR/tSxWBxTIr+Ql5PBloIql/QNzADUrju87EN84TWZkzjaKWPIptIhfAJCOhj/IlWRwU1pAEF8RKIAGGaKOAAA+1j/mlwjHkz3YwBYAKAAAABIAAAAAAAAAP/0O2lP7jIWgAbFoAalcd3nYhvnCazMmcbRSx5FNpEL4BIR0Mf5EqyOCmtIBADc/nwqtUo3cyFt3FjKP9qWKwOKZFfyEvJ4MtBFUv6BuUsrGmuAGKPR2AAA+1j/mlwTHkz3YwBgBiwAAAM1ACr7GCbsSjAfOP/evdEGNiCizc88BZrZyQlXjm8okJa7wD6URtVYTw4eBTWuXZchYWswosdVGZn3Ylvat6GUWKKgZAEGAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAIPD+UTaexEUA=");
    }

    #[test]
    fn ordinary_tx_with_external() {
        check_tx("te6ccgECEAEABCwAA7d/+1dDaSA3L2uibiQKv/JnIaNNn9WJtYieOs24uU8oUyAAARC3HEQsF4sbMIVMw2GJZ6YXtDxBuuUMHi11U9pKdB3vZW20NyawAAEQtwCcGBYUWY9AADSAMRKM6AUEAQIPDFnGHvr8xEADAgBvyaoTfE1Qm+AAAAAAAAIAAAAAAAK4sbHPs40L/+ZOYZTx8kBg3rDrUQKTevced6g9zSjJoEHQjiwAnUcl4xOIAAAAAAAAAABNwAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAACAAgnJJi+OXTuW6+ukeF09C5U7aOyCCB2vTGGfIBMpUz1N36QkaLDsxwWPePHHr9YTX1v135KTbkfOo5mdye6UVZ0BdAgHgCgYBAd8HAZ/gB/2robSQG5e10TcSBV/5M5DRps/qxNrETx1m3FynlCmTAIviBVbU9tFL6ORp6Y+Q3g5gYbQ8ZrQIVg/sKlYsewVEAAARC3HEQsJhRZj0YAgBdnfSJj9hRZj0ABIAYUWYtGFFmPQAAAAXSHboABUxNjMxOTE4NzQ1NTY4AAAAAAAAAAAAAAAAAAAAAAAACQFDgAY9rky4OsV5MdwUCx+JS2GQSGNJFcUhkbPBdDzyiZIMEAwBRYgB/2robSQG5e10TcSBV/5M5DRps/qxNrETx1m3FynlCmQMCwHhoZC88bv/ZdGKH/CXOHkHxWN5IyQlL5eBgvUZlCXejnT+k2B0aVKhFWtkDByRuBjlhyZMEngedFiTBMk/FS//AkgwHXhXMBNEwUCGrfyLQvMIa7ago3bhFgPvpwjzuUIpQAAAXv33KOMYUaDHWnklGGAMAf57Im5vbmNlIjoiYWM1ZmE2MDAxYzVhNjJiOTBlMzI5ZThmNTIyZmQwZGY2M2ZmMTA5MWY1ODcxMjYzIiwiYmFzZTY0IjoiUHZlc1BWRHo2ckFhQlpvd1Nkekp1ZTZoQVAvUkxwclJBcVR0eFpoSENWV0ZPRFgrR0NlcndKQ0VMDQH+eG82R1pGaHlJR29DK3lJaHlvelVRem9aYlcyTHk3QWxxTGtOc1JNRFlhZWFDZENPa043V0hmV2l6ZVJyMU02VklKSERXTXJjTktoeFdUSnl1eUNQUTlueEZSaEt0QmJ6aWE1Y3pnTVdHbDVGcEtwZGFSKzMvbmxSTUZFSFBLUA4B/mZwTi9RbXNPSEFjWmM0QnQ4QXM0VTBVdlNBRW1GRFJTK3UyNTV1elR3NW0vNHhING5sZ2NySTZWWVFicU93PT0iLCJwdWJfa2V5IjoiMjkwNGEwN2MzNTljMzE3YWQzNDMwMWU4MjVmNjE4NGIyOWYxNGI1YzZiNzMzMTU1ZTAPACBhZWI2MmI4YmZiZWUwNCJ9");
    }

    #[test]
    fn ordinary_bounce() {
        // Ok, no state
        check_tx("te6ccgECCAEAAdkAA7V1mlCYYM8b7NOFt7rztgCKPqqX5CQlGJJYLGKJ2Xtj2BAAAfayVBU8wAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAY8mf7wAD5gosIIAwIBAB8ECRj/+U1BwDBRYQMKLDBAAIJykK7Illr6uxbrw8ubQI665xthjXh4i8gNCYQ1k8rJjaSQrsiWWvq7FuvDy5tAjrrnG2GNeHiLyA0JhDWTysmNpAIB4AYEAQHfBQD5WACzShMMGeN9mnC29152wBFH1VL8hISjEksFjFE7L2x7AwAZ1ptY/J+PyTUHShgQ6koay4+trhPpMMwjMKDPvb1TWhGPwovUBhRYYAAAPtZKgqeax5M/3n////+BdwdxAAAAwuXMTVMAAAAAAAAAAAAAAIuyyXAAQAjiWEABsWgAzrTax+T8fkmoOlDAh1JQ1lx9bXCfSYZhGYUGfe3qmtEAFmlCYYM8b7NOFt7rztgCKPqqX5CQlGJJYLGKJ2Xtj2BRj/+U1AYgXoYAAD7WSoKnlseTP97ABwB7Au4O4gAAAYXLmJqmAAAAAAAAAAAAAAEXZZLgAIARxLDMBKJ9M0q/RXwQrCjTj5yjT6tEsu6rHO/eV5Jw9pA=");

        // NoFunds
        check_tx("te6ccgECBgEAAXEAA7d0XhMZi+e9SzhsQrBsY7gPnCKq299VsH5C63y8SRRRpMAAAaVqFwSIZbzAOEp/YjBJHOmFMdmJVZSnJ7/WUho73oMUXVuJHV0AAAGlahcEiEYuFylAABSFiSrsCAQDAQEhBAkLElbR0IWJKuwNAMPQkBACAKhhasuMLVlwAf///+UAC2QDAAAO8wAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAgnKWgrG3bY1QSltXmrTHAt5/OnOO6mlvzVOT3CgQ6A5fKlEFVtRu1kb0kWrMbHz20Ag4AZKdZI9LU9YcapS6G7paAQGgBQC5aACLwmMxfPepZw2IVg2MdwHzhFVbe+q2D8hdb5eJIoo0mQAReExmL571LOGxCsGxjuA+cIqrb31WwfkLrfLxJFFGkxCxJW0cBhRYYAAANK1C4JEKxcLlKAVRhK5A");
    }

    #[test]
    fn tick_tock_tx() {
        // Tock
        check_tx("te6ccgECBgEAASwAA691VVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVAAAfax88MIPSw0moITCuvilvszvMTwjAWqBNl6BXOXvVmKdtxaKU8AAAH2sfLO5DY8meygABQIBQQBAgUwMCQDAgBbwAAAAAAAAAAAAAAAAS1FLaRJ5QuM990nhh8UYSKv4bVGu4tw/IIW8MYUE5+OBACgQVxQF9eEAAAAAAAAAAAAQgAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAgnImgq+NHcQq6sTadKlyN/JVsIjCmWhcl81ZK/uRSGSbXHd22xrbnN+GPXJpoXBZ6pxl7sArfWoZr5BuYa29vanoAAEg");

        // Tick
        check_tx("te6ccgECBgEAASwAA69zMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzAAAfax88MIGhxBVccog1yEGiaTdf1fIS97n6H7Nx0kV91X6d2gb3JgAAH2sfLO5CY8meygABQIBQQBAgUgMCQDAgBbwAAAAAAAAAAAAAAAAS1FLaRJ5QuM990nhh8UYSKv4bVGu4tw/IIW8MYUE5+OBACgQsMQF9eEAAAAAAAAAAAAiAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAgnKdvt2OBw9r4/ZICoIsb9ckq/90a1fWXhthgXldZUG1cEPP/jeD7UbLLMewICVZHh9eY00PRU4gZB47Vtmn9I7zAAEg");
    }
}
