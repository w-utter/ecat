use ethercrab::{PduHeader, PduResponseHandle, received_frame::ReceivedPdu, std::RawSocketDesc};
use io_uring::{opcode, squeue, types::Fd};

pub struct TxBuf<'sto> {
    pub stored_entry: squeue::Entry,
    pub buf: [u8; 1024],
    pub id: u64,
    pub retries_remaining: usize,
    pub received: Option<(PduHeader, ReceivedPdu<'sto>)>,
    pub configured_addr: Option<u16>,
    pub identifier: Option<u8>,
}

pub trait TxIndex {
    fn idx(&self) -> u64;
}

impl TxIndex for PduResponseHandle {
    fn idx(&self) -> u64 {
        u64::from(self.pdu_idx)
            | (u64::from(self.index_in_frame) << 8)
            | (u64::from(self.command_code) << 16)
    }
}

impl TxIndex for (usize, PduHeader) {
    fn idx(&self) -> u64 {
        let pdu_offset = self.0 as u64;
        u64::from(self.1.index) | (pdu_offset << 8) | (u64::from(self.1.command_code) << 16)
    }
}

impl TxBuf<'_> {
    pub fn new(
        idx: &impl TxIndex,
        retries: usize,
        configured_addr: Option<u16>,
        identifier: Option<u8>,
    ) -> Self {
        let id = idx.idx();
        Self {
            id,
            buf: [0; 1024],
            stored_entry: unsafe { core::mem::zeroed() },
            retries_remaining: retries,
            received: None,
            configured_addr,
            identifier,
        }
    }

    pub fn update(&mut self, bytes: &[u8], sock: &RawSocketDesc) -> &squeue::Entry {
        self.buf
            .get_mut(0..bytes.len())
            .unwrap()
            .copy_from_slice(bytes);

        use std::os::fd::AsRawFd;
        self.stored_entry =
            opcode::Write::new(Fd(sock.as_raw_fd()), self.buf.as_ptr(), bytes.len() as _)
                .build()
                .user_data(self.idx() | crate::io::WRITE_MASK);

        &self.stored_entry
    }

    pub fn entry(&self) -> &squeue::Entry {
        &self.stored_entry
    }

    pub fn idx(&self) -> u64 {
        self.id
    }
}
