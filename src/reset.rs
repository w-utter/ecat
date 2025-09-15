use crate::setup::setup_write;
use crate::txbuf::TxBuf;
use ethercrab::{
    MainDevice, PduHeader, error::Error, received_frame::ReceivedPdu, std::RawSocketDesc,
};
use io_uring::{IoUring, types::Timespec};
use std::collections::BTreeMap;

pub struct Reset {
    device_count: Option<u16>,
    reset_count: u8,
}

impl Reset {
    pub(crate) fn new() -> Self {
        Self {
            device_count: None,
            reset_count: 0,
        }
    }

    pub(crate) fn start(
        &mut self,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
    ) -> Result<(), Error> {
        let (frame, handle) = maindevice.prep_count_subdevices().unwrap().unwrap();
        setup_write(
            frame,
            handle,
            retry_count,
            timeout,
            tx_entries,
            sock,
            ring,
            None,
            None,
        )?;

        maindevice.prep_reset_subdevices(|res| {
            let (frame, handle) = res.unwrap().unwrap();
            setup_write(
                frame,
                handle,
                retry_count,
                timeout,
                tx_entries,
                sock,
                ring,
                None,
                None,
            )?;
            Ok(())
        })?;

        Ok(())
    }

    const MAX_CLEARS: u8 = 44;

    pub(crate) fn update(&mut self, received: ReceivedPdu<'_>, header: PduHeader) -> Option<u16> {
        match header.command_code {
            7 => self.device_count = Some(received.working_counter),
            8 => self.reset_count += 1,
            _ => unreachable!(),
        }

        if self.reset_count < Self::MAX_CLEARS {
            return None;
        }

        self.device_count
    }
}
