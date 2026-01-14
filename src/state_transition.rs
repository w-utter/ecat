use crate::setup::setup_write;
use crate::txbuf::TxBuf;
use ethercrab::EtherCrabWireRead;
use ethercrab::{
    AlControl, MainDevice, PduHeader, SubDeviceState, error::Error, received_frame::ReceivedPdu,
    std::RawSocketDesc,
};
use io_uring::{IoUring, types::Timespec};
use std::collections::BTreeMap;

// request transition from one state to another
// eg, init -> preop
pub struct Transition {
    requested: SubDeviceState,
    state: TransitionState,
}

impl Transition {
    pub fn new(requested: SubDeviceState) -> Self {
        Self {
            requested,
            state: TransitionState::Transition,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn start(
        &mut self,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout_duration: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
        configured_addr: u16,
        idx: u16,
        write_entry: impl Fn(u64) -> u64,
        timeout_entry: impl Fn(u64) -> u64,
    ) -> Result<(), Error> {
        let (frame, handle) = maindevice
            .prep_request_subdevice_state(configured_addr, self.requested)?
            .unwrap();

        setup_write(
            frame,
            handle,
            retry_count,
            timeout_duration,
            tx_entries,
            sock,
            ring,
            Some(idx),
            None,
            write_entry,
            timeout_entry,
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        received: ReceivedPdu<'_>,
        _header: PduHeader,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout_duration: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
        configured_addr: u16,
        idx: u16,
        write_entry: impl Fn(u64) -> u64,
        timeout_entry: impl Fn(u64) -> u64,
    ) -> Result<bool, Error> {
        match &mut self.state {
            TransitionState::Transition => {
                let res = AlControl::unpack_from_slice(&received).unwrap();

                if res.error {
                    todo!("error transitioning to {:?}", self.requested);
                }

                let (frame, handle) = maindevice
                    .prep_wait_subdevice_state(configured_addr, self.requested)?
                    .unwrap();
                setup_write(
                    frame,
                    handle,
                    retry_count,
                    timeout_duration,
                    tx_entries,
                    sock,
                    ring,
                    Some(idx),
                    None,
                    write_entry,
                    timeout_entry,
                )?;
                self.state = TransitionState::WaitForAck;
            }
            TransitionState::WaitForAck => {
                let res = AlControl::unpack_from_slice(&received).unwrap();

                if res.state != self.requested {
                    let (frame, handle) = maindevice
                        .prep_wait_subdevice_state(configured_addr, self.requested)?
                        .unwrap();

                    setup_write(
                        frame,
                        handle,
                        retry_count,
                        timeout_duration,
                        tx_entries,
                        sock,
                        ring,
                        Some(idx),
                        None,
                        write_entry,
                        timeout_entry,
                    )?;
                    return Ok(false);
                }
                return Ok(true);
            }
        }
        Ok(false)
    }
}

enum TransitionState {
    Transition,
    WaitForAck,
}
