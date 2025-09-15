use crate::io::TIMEOUT_MASK;
use crate::txbuf::{TxBuf, TxIndex};
use ethercrab::error::Error;
use ethercrab::{PduResponseHandle, SendableFrame, std::RawSocketDesc};
use io_uring::{
    IoUring, opcode,
    types::{TimeoutFlags, Timespec},
};
use std::collections::BTreeMap;

// used for setting up a timeout with io_uring
pub(crate) fn setup_timeout(
    tx_handle: &PduResponseHandle,
    ring: &mut IoUring,
    duration: &Timespec,
) -> std::io::Result<()> {
    let idx = tx_handle.idx();

    let timeout = opcode::Timeout::new(duration)
        .flags(TimeoutFlags::MULTISHOT)
        .build()
        .user_data(idx | TIMEOUT_MASK);

    while unsafe { ring.submission().push(&timeout).is_err() } {
        ring.submit().expect("could not submit ops");
    }
    ring.submit()?;
    Ok(())
}

// used for setting up a write with io_uring
#[allow(clippy::too_many_arguments)]
pub fn setup_write(
    frame: SendableFrame,
    handle: PduResponseHandle,
    retry_count: usize,
    timeout_duration: &Timespec,
    tx_entries: &mut BTreeMap<u64, TxBuf>,
    sock: &RawSocketDesc,
    ring: &mut IoUring,
    configured_addr: Option<u16>,
    identifier: Option<u8>,
) -> Result<(), Error> {
    let mut buf = TxBuf::new(&handle, retry_count, configured_addr, identifier);

    frame.send_blocking(|bytes| {
        let tx_entry = buf.update(bytes, sock);
        setup_timeout(&handle, ring, timeout_duration).map_err(|_| Error::Internal)?;

        while unsafe { ring.submission().push(tx_entry).is_err() } {
            ring.submit().expect("could not submit ops");
        }
        ring.submit().map_err(|_| Error::Internal)?;
        Ok(bytes.len())
    })?;

    tx_entries.insert(buf.idx(), buf);
    Ok(())
}
