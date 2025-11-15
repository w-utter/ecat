use crate::setup::setup_write;
use crate::txbuf::TxBuf;
use ethercrab::{
    MainDevice, PduHeader, PrepResetDevices, ResetDevices, error::Error,
    received_frame::ReceivedPdu, std::RawSocketDesc,
};
use io_uring::{IoUring, types::Timespec};
use std::collections::BTreeMap;

pub struct Reset {
    state: PrepResetDevices,
    device_count: Option<u16>,
    //reset_count: u8,
}

impl Reset {
    pub(crate) fn new() -> Self {
        Self {
            device_count: None,
            state: PrepResetDevices::default(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start(
        &mut self,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
        write_entry: impl Fn(u64) -> u64,
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
            write_entry,
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn update(
        &mut self,
        received: ReceivedPdu<'_>,
        header: PduHeader,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
        write_entry: impl Fn(u64) -> u64,
    ) -> Result<Option<u16>, ethercrab::error::Error> {
        match header.command_code {
            7 => self.device_count = Some(received.working_counter),
            8 => (),
            _ => return Ok(None),
        }

        if let Some(res) = ResetDevices::iter(maindevice, &mut self.state) {
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
                write_entry,
            )?;
            Ok(None)
        } else {
            Ok(self.device_count)
        }
    }
}
